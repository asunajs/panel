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
    prelude::Func,
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

/// 试跑时 console 输出的去处。
pub type ConsoleCapture = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// 装进试跑上下文的 console 覆盖。
///
/// 为什么要在 Rust 侧抢：llrt 自带的 console 直写进程 stdout（守护进程跑起来就是
/// journal / 日志文件），试跑的人在那个框里什么也看不见。字符串化用 JSON.stringify
/// 兜底，`console.log({a:1})` 至少还能看懂。
const TEST_CONSOLE: &str = r#"
(() => {
  const fmt = (v) => {
    if (typeof v === 'string') return v;
    if (v instanceof Error) return v.stack || String(v);
    try { return JSON.stringify(v); } catch { return String(v); }
  };
  const make = (level) => (...a) => __test_console(level, a.map(fmt).join(' '));
  globalThis.console = {
    log: make('log'), info: make('info'), warn: make('warn'),
    error: make('error'), debug: make('debug'), trace: make('trace'),
  };
})();
"#;

/// 一次试跑的结果。
pub struct TestOutcome {
    /// console 输出（按顺序）。
    pub lines: Vec<String>,
    /// 加载/执行期的错误。`None` = 跑完了。
    pub error: Option<String>,
}

/// 一次 invocation 的停止位。**绑 invocation，不绑插件、不绑全局** ——
/// 基线 R1 High 就是绑错了层（`src/engine.rs:815-834`）。
#[derive(rquickjs::JsLifetime)]
pub struct Invocation {
    stopped: AtomicBool,
    /// 是不是**人**点的「停止」（而不是重载/卸载顺手停的）。
    /// 两者都置 `stopped`，但给人看的说法不一样 —— 说错会让人以为插件被卸载了。
    manual: AtomicBool,
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
            manual: AtomicBool::new(false),
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
    /// 人点的「停止」（面板按钮 / `POST /api/plugins/<name>/stop`）。
    pub fn stop_by_hand(&self) {
        self.manual.store(true, Ordering::SeqCst);
        self.stop();
    }
    /// 这次停下来是不是人点的。只在 `stopped` 为真时有意义。
    pub fn stopped_by_hand(&self) -> bool {
        self.manual.load(Ordering::SeqCst)
    }
    pub fn reset(&self) {
        self.stopped.store(false, Ordering::SeqCst);
        self.manual.store(false, Ordering::SeqCst);
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
// 定时器闸门：JS 定时器会踩「1 插件 1 runtime」的退出路径（docs/engine-teardown.md）。
//
// setInterval 直接禁掉：它的循环任务让 eval_module 永不返回（实测 load 卡死），
// 而且我们没有任何办法在析构前把它清掉。
// setTimeout 只放行 5 秒以内的：长等待会让卸载等它烧完，短定时器是插件里常见的
// 防抖/延迟用途，代价可控。
(() => {
  const MAX_DELAY = 5000;
  const st = globalThis.setTimeout;
  if (typeof st !== 'function') return;
  globalThis.setInterval = () => {
    throw new Error('面板里不能用 setInterval：它的循环任务会让插件加载和卸载卡死。' +
      '周期性的活请用 host.register_cron(名字, 表达式, 预算)，要等请用 await host.sleep(ms)');
  };
  globalThis.setTimeout = (fn, ms, ...a) => {
    const d = Number(ms) || 0;
    if (d > MAX_DELAY) {
      throw new Error('面板里 setTimeout 最多 ' + MAX_DELAY + 'ms（这次是 ' + d + 'ms）：' +
        '长等待会让插件卸载卡住。要等请用 await host.sleep(ms)');
    }
    return st(fn, d, ...a);
  };
})();
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
    // 顺序有讲究：**ctx 必须先于 rt 释放**（Rust 按声明顺序 drop）。反过来是
    // AsyncRuntime 先走、Context 后走，QuickJS 在 JS_FreeRuntime 里就有机会撞上
    // 还没释放的对象。
    //
    // 注意：只调这个顺序**治不好**定时器那个坑（见 docs/engine-teardown.md），
    // 但它是正确顺序，留着。
    ctx: AsyncContext,
    rt: AsyncRuntime,
    /// 运行时的裸指针（`usize`，不是引用）：Drop 里要拿它把定时器状态摘掉。
    /// 存成整数是因为 `Plugin` 要 `Send`，而 `*mut` 裸指针不是。
    rt_raw: usize,
    inv: Arc<Invocation>,
}

impl Drop for Plugin {
    /// 释放前先把这个运行时的定时器状态摘掉（见 docs/engine-teardown.md）。
    ///
    /// 顺序很重要：Drop 体在字段析构**之前**跑，所以此时 rt 还活着，
    /// 摘掉状态能把挂着的 `Persistent<Function>` 正常释放；等字段析构时，
    /// 定时器循环任务已经拿到「状态没了」的结论，会自己退出。
    ///
    /// 不做这一步会连撞三件事：析构等循环任务（`setInterval` 永远等不到）、
    /// `JS_FreeRuntime` 断言 abort、循环任务踩已释放的 rt 指针。
    fn drop(&mut self) {
        llrt_timers::forget_runtime_ptr(self.rt_raw);
    }
}

impl Plugin {
    pub async fn load(source: &str, handle: HostHandle) -> std::result::Result<Self, String> {
        Self::load_inner(source, handle, None, None).await
    }

    /// 跑「脚本」用：顶层代码带**墙钟死线**。
    ///
    /// 脚本是一次性的（跑完就扔），没有 dispatch 那层预算保护，
    /// 所以死线必须在这里装 —— 否则一个死循环脚本能把面板拖死。
    pub async fn load_with_budget(
        source: &str,
        handle: HostHandle,
        budget_ms: u64,
    ) -> std::result::Result<Self, String> {
        Self::load_inner(source, handle, Some(budget_ms), None).await
    }

    async fn load_inner(
        source: &str,
        handle: HostHandle,
        budget_ms: Option<u64>,
        capture: Option<ConsoleCapture>,
    ) -> std::result::Result<Self, String> {
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
                // 试跑：把 console 抢过来，输出回到调用方而不是进程 stdout。
                if let Some(cap) = &capture {
                    let sink = cap.clone();
                    g.set(
                        "__test_console",
                        Func::from(move |level: String, text: String| {
                            sink.lock().unwrap_or_else(|p| p.into_inner()).push(
                                if level == "log" { text } else { format!("[{level}] {text}") },
                            );
                        }),
                    )?;
                    ctx.eval_with_options::<Value, _>(TEST_CONSOLE, EvalOptions::default())?;
                }
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

        // 死线自己留一份：eval 被 interrupt 打断时 rquickjs 只给一个 `interrupted`，
        // 光看那个字符串分不出「预算耗尽」和「被 stop」——判断得靠这里的墙钟。
        let mut deadline: Option<Instant> = None;
        if let Some(ms) = budget_ms {
            let dl = Instant::now() + Duration::from_millis(ms);
            deadline = Some(dl);
            let stopped = inv.clone();
            rt.set_interrupt_handler(Some(Box::new(move || {
                stopped.is_stopped() || Instant::now() >= dl
            })))
            .await;
        }

        let rt_raw = ctx.with(|ctx| llrt_timers::runtime_ptr(&ctx)).await;
        let plugin = Self {
            rt,
            ctx,
            rt_raw,
            inv,
        };
        if let Err(e) = plugin.eval_module(source).await {
            // 加载期也可能被预算打断（脚本就是「加载=执行」）。这里的原始错误只有
            // 一个 `interrupted`，直接冒出去等于让用户看天书，也没记进预算打断的计数。
            let budget_hit = deadline.is_some_and(|dl| Instant::now() >= dl);
            if budget_hit {
                crate::stats::stats().record_budget_kill();
                let ms = budget_ms.unwrap_or(0);
                return Err(format!("CPU 预算耗尽（{ms}ms 墙钟死线）"));
            }
            if plugin.invocation().is_stopped() {
                return Err(format!("已停止（加载期被中断）：{e}"));
            }
            return Err(e);
        }
        Ok(plugin)
    }

    /// 在**一次性上下文**里跑一段源码：不碰任何插件的状态，跑完就扔。
    pub async fn run_test(source: &str, handle: HostHandle, budget_ms: u64) -> TestOutcome {
        let cap: ConsoleCapture = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let result =
            Self::load_inner(source, handle, Some(budget_ms), Some(cap.clone())).await;
        let lines = cap.lock().map(|g| g.clone()).unwrap_or_default();
        TestOutcome {
            lines,
            error: result.err(),
        }
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
        self.call_inner(handler, evt, budget_ms).await.map(|_| ())
    }

    /// 同 `dispatch`，但把 handler 的**返回值**带出来 —— 插件 WebUI 靠它回响应。
    /// 读一次 JS 堆占用（QuickJS 的 \`malloc_size\`）。
    ///
    /// **不等**：插件在跑的时候 runtime 锁在 \`idle()\` 手里（architecture.md §2.1），
    /// 等它等于让仪表盘跟着一个 20 分钟的长任务一起卡住。所以只给一个很短的等待上限，
    /// 拿不到就返回 None —— 界面上说清楚「这次没取到」，而不是报个假的 0。
    pub async fn memory_usage(&self, wait: std::time::Duration) -> Option<u64> {
        match tokio::time::timeout(wait, self.rt.memory_usage()).await {
            Ok(m) => Some(m.malloc_size.max(0) as u64),
            Err(_) => None,
        }
    }

    pub async fn call_json(
        &self,
        handler: &str,
        evt: &serde_json::Value,
        budget_ms: u64,
    ) -> std::result::Result<Option<serde_json::Value>, String> {
        self.call_inner(handler, evt, budget_ms).await
    }

    async fn call_inner(
        &self,
        handler: &str,
        evt: &serde_json::Value,
        budget_ms: u64,
    ) -> std::result::Result<Option<serde_json::Value>, String> {
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
        // 返回值也带出来：`dispatch` 不要它，`call_json`（WebUI）要。
        let call = format!(
            "(async () => {{ try {{ const v = await globalThis.__dispatch({}, {}); \
             globalThis.__result = JSON.stringify({{ok:true, value: v === undefined ? null : v}}); }} \
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
                                    Ok(Some(v["value"].clone()))
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
