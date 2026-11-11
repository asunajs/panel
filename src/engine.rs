//! 引擎层：1 插件 = 1 `AsyncRuntime` + 1 `AsyncContext`，跑在自己的 tokio task 上。
//!
//! 不用 `Vm::from_options` —— 它在 init/eval 失败时 `print_error_and_exit`，
//! 一个插件写错语法就带走整个面板。组装路径与实测见 docs/llrt-spike.md 判据 2。
//!
//! ⚠ `idle()` 全程持有 runtime 锁，`ctx.with()` 要同一把锁。所以：
//! - 取消必须在 idle **之前**把 token 拿到手，之后纯 Rust 触发；
//! - 宿主不能在插件跑着的时候读它的 JS 状态。
//! 详见 docs/architecture.md §2.1/§2.2。

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use llrt_core::{
    context::EvalOptions,
    libs::{
        json, numbers,
        utils::{
            module::ModuleInfo,
            primordials::{BasePrimordials, Primordial},
            time,
        },
    },
    loader::{FileResolver, ImportAttributes, Resolver},
    modules::{
        embedded::{init as embedded_init, loader::EmbeddedLoader, resolver::EmbeddedResolver},
        https::{set_http_version, HttpVersion},
        module::resolver::ModuleResolver,
        module_builder::ModuleBuilder,
        package::{loader::PackageLoader, resolver::PackageResolver},
    },
    AsyncContext, AsyncRuntime, Ctx, Error, Result, Value,
};
use tokio::sync::Notify;

use crate::hostapi::{HostHandle, HostModule};

/// 一次 invocation 的停止位。**绑 invocation，不绑插件、不绑全局** ——
/// 基线 R1 High 就是绑错了层（`src/engine.rs:815-834`）。
#[derive(rquickjs::JsLifetime)]
pub struct Invocation {
    stopped: AtomicBool,
    notify: Notify,
    /// 当前 run 的 id，供日志归属；-1 = 没有。
    pub run_id: AtomicI64,
}

impl Default for Invocation {
    fn default() -> Self {
        // 不能用 derive：AtomicI64 的默认值是 0，而 0 是一个合法的 run id，
        // 加载期的日志会被错记到「第 0 次运行」上。
        Self {
            stopped: AtomicBool::new(false),
            notify: Notify::new(),
            run_id: AtomicI64::new(-1),
        }
    }
}

impl Invocation {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
    pub fn reset(&self) {
        self.stopped.store(false, Ordering::SeqCst);
    }
    /// 已经停了就立刻返回，否则等通知。host op 用它做挂起预算的取消臂。
    pub async fn cancelled(&self) {
        if self.is_stopped() {
            return;
        }
        self.notify.notified().await;
    }
    pub fn run_id(&self) -> Option<i64> {
        let v = self.run_id.load(Ordering::SeqCst);
        if v < 0 {
            None
        } else {
            Some(v)
        }
    }
}

pub fn invocation(ctx: &Ctx<'_>) -> Result<Arc<Invocation>> {
    ctx.userdata::<Arc<Invocation>>()
        .ok_or_else(|| Error::new_from_js("ctx", "Invocation userdata missing"))
        .map(|g| (*g).clone())
}

impl From<HostModule> for ModuleInfo<HostModule> {
    fn from(val: HostModule) -> Self {
        ModuleInfo {
            name: "panel:host",
            module: val,
        }
    }
}

/// `Vm` 内部那三个私有 init 的公开替代。都是环境变量驱动的开关，setter 全公开。
pub fn global_inits() {
    time::init();
    set_http_version(HttpVersion::Http1_1);
    // security::init() 只在设了 LLRT_NET_ALLOW/DENY 时才有动作，而且是进程级全局。
    // 面板要的是 per-plugin SSRF 门（src/ssrf.rs），不依赖它。
}

/// 插件侧的 handler 注册表与派发入口。宿主只按名字派发，不持有 JS 函数引用。
const BOOTSTRAP: &str = r#"
globalThis.__handlers = Object.create(null);
globalThis.__dispatch = (name, evtJson) => {
  const fn = globalThis.__handlers[name];
  if (typeof fn !== 'function') throw new Error('没有这个 handler: ' + name);
  return fn(JSON.parse(evtJson));
};
"#;

/// 把引擎错误变成人能读的一行。
///
/// `Error::Exception` **本身不带消息** —— 真正的 JS 异常在 `ctx.catch()` 里。
/// 不取出来，日志里就只剩 "Exception" 这个词，插件作者根本不知道哪行错了。
fn describe(ctx: &Ctx<'_>, e: Error) -> String {
    if !matches!(e, Error::Exception) {
        return e.to_string();
    }
    let v = ctx.catch();
    if let Some(o) = v.as_object() {
        for key in ["stack", "message"] {
            if let Ok(Some(s)) = o.get::<_, Option<String>>(key) {
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }
    if let Some(s) = v.as_string() {
        if let Ok(text) = s.to_string() {
            if !text.is_empty() {
                return text;
            }
        }
    }
    "未知异常（既没有 message 也没有 stack）".to_string()
}

/// LLRT 自带的出站出口**不经过**面板的 SSRF 门。实测（docs/llrt-spike.md 判据 7）：
/// `host.fetch` 打回环被拒，但全局 `fetch` 拿到 200、`net.connect` 真连上了。
/// 所以这些模块名在解析层就拒掉，插件发请求只剩 `panel:host` 的 fetch 一条路。
///
/// `child_process` 也在内：留着它，插件 spawn 一个 curl 就绕过了整道门。
const DENIED_MODULES: [&str; 4] = ["https", "net", "dgram", "child_process"];

struct GatedResolver(ModuleResolver);

impl Resolver for GatedResolver {
    fn resolve<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        attributes: Option<ImportAttributes<'js>>,
    ) -> Result<String> {
        // 先让内层归一化再判：`node:` 前缀、CJS 前缀、结尾斜杠都在它里面处理，
        // 自己按原始名字判会漏掉 `node:https`。
        let resolved = self.0.resolve(ctx, base, name, attributes)?;
        if DENIED_MODULES.contains(&resolved.as_str()) {
            return Err(Error::new_resolving_message(
                base,
                name,
                format!("{resolved} 被面板禁用：出站请求请用 panel:host 的 fetch"),
            ));
        }
        Ok(resolved)
    }
}

pub struct Plugin {
    rt: AsyncRuntime,
    ctx: AsyncContext,
    inv: Arc<Invocation>,
}

impl Plugin {
    pub async fn load(source: &str, handle: HostHandle) -> std::result::Result<Self, String> {
        let inv = handle.inv.clone();
        let (module_resolver, module_loader, globals) =
            ModuleBuilder::default().with_module(HostModule).build();
        let mut file_resolver = FileResolver::default();
        file_resolver.add_path(".");
        let resolver = (
            GatedResolver(module_resolver),
            EmbeddedResolver,
            PackageResolver,
            file_resolver,
        );
        let loader = (module_loader, EmbeddedLoader, PackageLoader);

        let rt = AsyncRuntime::new().map_err(|e| e.to_string())?;
        rt.set_max_stack_size(512 * 1024).await;
        rt.set_gc_threshold(20 * 1024 * 1024).await;
        rt.set_loader(resolver, loader).await;

        let ctx = AsyncContext::full(&rt).await.map_err(|e| e.to_string())?;
        ctx.with(|ctx| {
            let init = || -> Result<()> {
                BasePrimordials::init(&ctx)?;
                globals.attach(&ctx)?;
                embedded_init(&ctx)?;
                numbers::redefine_prototype(&ctx)?;
                json::redefine_static_methods(&ctx)?;
                let g = ctx.globals();
                // fetch 全局是 ModuleBuilder::default() 挂上的，它直连 HTTP_CLIENT，
                // 不经过 hostapi 的 SSRF/限额。摘掉它，Request/Response 等纯数据类留着。
                g.remove("fetch")?;
                g.set("global", g.clone())?;
                g.set("self", g.clone())?;
                let _ = ctx.store_userdata(inv.clone());
                let _ = ctx.store_userdata(handle);
                ctx.eval_with_options::<Value, _>(BOOTSTRAP, EvalOptions::default())?;
                Ok(())
            };
            init().map_err(|e| describe(&ctx, e))
        })
        .await?;

        let plugin = Self { rt, ctx, inv };
        plugin.eval_module(source).await?;
        Ok(plugin)
    }

    /// 求值一段 ESM 并把它挂起的顶层 await 跑完。加载期的错误要报出来，
    /// 不能像 `Vm::from_options` 那样把进程带走。
    async fn eval_module(&self, source: &str) -> std::result::Result<(), String> {
        self.ctx
            .with(|ctx| {
                let mut opts = EvalOptions::default();
                opts.strict = true;
                opts.promise = true;
                opts.global = false;
                let p: Result<llrt_core::Promise> = ctx.eval_with_options(source, opts);
                let p = p.map_err(|e| describe(&ctx, e))?;
                ctx.globals()
                    .set("__pending", p)
                    .map_err(|e| describe(&ctx, e))
            })
            .await?;
        self.rt.idle().await;
        self.ctx
            .with(|ctx| {
                let p: llrt_core::Promise = ctx
                    .globals()
                    .get("__pending")
                    .map_err(|e| describe(&ctx, e))?;
                // 顶层 await 的 rejection 在这里才浮出来
                p.finish::<Value>().map_err(|e| describe(&ctx, e))?;
                Ok(())
            })
            .await
    }

    pub fn invocation(&self) -> Arc<Invocation> {
        self.inv.clone()
    }

    /// 派发一次 invocation。`budget_ms` 是**墙钟死线**，不是固定切片。
    ///
    /// interrupt 抛的是 uncatchable 异常（rquickjs 文档明写），JS 侧 try/catch
    /// 抓不到，所以只能在这里接 `Err`。触发后 runtime 仍可用。
    pub async fn dispatch(
        &self,
        handler: &str,
        evt: &serde_json::Value,
        budget_ms: u64,
    ) -> std::result::Result<(), String> {
        self.inv.reset();
        let deadline = Instant::now() + Duration::from_millis(budget_ms);
        let stopped = self.inv.clone();
        self.rt
            .set_interrupt_handler(Some(Box::new(move || {
                stopped.is_stopped() || Instant::now() >= deadline
            })))
            .await;

        let evt_json = serde_json::to_string(evt).map_err(|e| e.to_string())?;
        // 包一层 try/catch 把结果落到 __result：顶层 await 的 rejection 不会从
        // idle() 冒出来，而 interrupt 抛的是 uncatchable 异常 —— 它会让 __result
        // 保持未设置，正好成为「被预算/停止打断」的判据。
        let call = format!(
            "(async () => {{ try {{ await globalThis.__dispatch({}, {}); \
             globalThis.__result = '{{\"ok\":true}}'; }} \
             catch (e) {{ globalThis.__result = JSON.stringify({{ok:false, error: String((e && e.stack) || e)}}); }} }})()",
            serde_json::to_string(handler).map_err(|e| e.to_string())?,
            serde_json::to_string(&evt_json).map_err(|e| e.to_string())?
        );
        // ⚠ 闭包不能返回 JS Value —— ctx.with 要求返回值 Send，而 JSValue 里是裸指针。
        // 所以求值结果在闭包内就丢掉，只把「起没起来」这件事带出来。
        let started = self
            .ctx
            .with(|ctx| {
                let mut opts = EvalOptions::default();
                opts.strict = true;
                opts.promise = true;
                opts.global = false;
                ctx.eval_with_options::<Value, _>(
                    "globalThis.__result = undefined;",
                    EvalOptions::default(),
                )
                .map_err(|e| describe(&ctx, e))?;
                ctx.eval_with_options::<Value, _>(call, opts)
                    .map(|_| ())
                    .map_err(|e| describe(&ctx, e))
            })
            .await;

        let out = match started {
            Err(e) => Err(e),
            Ok(_) => {
                self.rt.idle().await;
                self.ctx
                    .with(|ctx| {
                        let raw: Option<String> = ctx
                            .globals()
                            .get("__result")
                            .map_err(|e| describe(&ctx, e))?;
                        match raw {
                            None => Err(if self.inv.is_stopped() {
                                "invocation 被停止".to_string()
                            } else {
                                crate::stats::stats().record_budget_kill();
                                format!("CPU 预算耗尽（{budget_ms}ms 墙钟死线）")
                            }),
                            Some(s) => {
                                let v: serde_json::Value =
                                    serde_json::from_str(&s).map_err(|e| e.to_string())?;
                                if v["ok"].as_bool() == Some(true) {
                                    Ok(())
                                } else {
                                    Err(v["error"].as_str().unwrap_or("未知错误").to_string())
                                }
                            }
                        }
                    })
                    .await
            }
        };
        self.rt.set_interrupt_handler(None).await;
        out
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;