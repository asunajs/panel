// 绕开 llrt_core::vm::Vm 的组装路径。Vm::from_options / run_with 在失败时调
// print_error_and_exit，对多插件面板不可接受；它内部那三个私有 init 全是
// 「读环境变量 → 调公开 setter」，可以用公开零件逐个复现。
use std::time::Duration;

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
    loader::FileResolver,
    module::{Declarations, Exports, ModuleDef},
    modules::{
        embedded::{init as embedded_init, loader::EmbeddedLoader, resolver::EmbeddedResolver},
        https::{set_http_version, HttpVersion},
        module_builder::ModuleBuilder,
        package::{loader::PackageLoader, resolver::PackageResolver},
    },
    prelude::{Async, Func},
    AsyncContext, AsyncRuntime, Ctx, Error, Exception, Object, Result, Value,
};

pub struct HostModule;

/// 挂起预算与取消的归属点：每个 runtime 一个，host 侧的 async op 必须 select 它。
/// interrupt handler 只在解释器执行时才会被调用，对挂起中的 await 无效，
/// 所以「停一个 invocation」不能只靠 interrupt。
#[derive(Default)]
pub struct Invocation {
    stopped: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

// rquickjs 的 JsLifetime 文档明写：不含任何非 'static 生命周期的类型总是可以安全实现。
// Invocation 只有 AtomicBool 和 Notify，没有 JS 值。derive 宏会按名字找 rquickjs crate，
// 而这里只依赖 llrt_core 的再导出，所以手写。
#[allow(unsafe_code)]
unsafe impl<'js> llrt_core::JsLifetime<'js> for Invocation {
    type Changed<'to> = Invocation;
}

impl Invocation {
    pub fn stop(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 返回 true 表示被取消。op 用它包自己的等待。
    pub async fn cancelled(&self) {
        if self.is_stopped() {
            return;
        }
        self.notify.notified().await;
    }
}

pub fn invocation(ctx: &Ctx<'_>) -> Result<std::sync::Arc<Invocation>> {
    ctx.userdata::<std::sync::Arc<Invocation>>()
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

async fn host_sleep(ctx: Ctx<'_>, ms: f64) -> Result<f64> {
    let inv = invocation(&ctx)?;
    let dur = Duration::from_millis(ms as u64);
    tokio::select! {
        _ = tokio::time::sleep(dur) => Ok(ms),
        _ = inv.cancelled() => Err(Exception::throw_message(&ctx, "AbortError: invocation stopped")),
    }
}

async fn host_boom(ctx: Ctx<'_>, msg: String) -> Result<()> {
    tokio::time::sleep(Duration::from_millis(5)).await;
    Err(Exception::throw_message(&ctx, &msg))
}

impl ModuleDef for HostModule {
    fn declare(declare: &Declarations) -> Result<()> {
        declare.declare("info")?;
        declare.declare("sleep")?;
        declare.declare("boom")?;
        declare.declare("default")?;
        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> Result<()> {
        exports.export(
            "info",
            Func::from(|msg: String| println!("    [host.info] {msg}")),
        )?;
        exports.export("sleep", Func::from(Async(host_sleep)))?;
        exports.export("boom", Func::from(Async(host_boom)))?;

        let obj = Object::new(ctx.clone())?;
        obj.set(
            "info",
            Func::from(|msg: String| println!("    [host.info] {msg}")),
        )?;
        obj.set("sleep", Func::from(Async(host_sleep)))?;
        obj.set("boom", Func::from(Async(host_boom)))?;
        exports.export("default", obj)?;
        Ok(())
    }
}

pub fn global_inits() {
    time::init();
    set_http_version(HttpVersion::Http1_1);
    // security::init() 只在设了 LLRT_NET_ALLOW/DENY 时才有动作，而且是进程级全局。
    // 面板要的是 per-plugin SSRF 门，不能依赖它，留到 host-api 自己做。
}

pub async fn assemble() -> Result<(AsyncRuntime, AsyncContext)> {
    let (module_resolver, module_loader, globals) =
        ModuleBuilder::default().with_module(HostModule).build();

    let mut file_resolver = FileResolver::default();
    file_resolver.add_path(".");
    let resolver = (
        module_resolver,
        EmbeddedResolver,
        PackageResolver,
        file_resolver,
    );
    let loader = (module_loader, EmbeddedLoader, PackageLoader);

    let rt = AsyncRuntime::new()?;
    rt.set_host_promise_rejection_tracker(Some(Box::new(
        |_ctx, _promise, reason, handled| {
            if !handled {
                println!("    [unhandled-rejection] {reason:?}");
            }
        },
    )))
    .await;
    rt.set_max_stack_size(512 * 1024).await;
    rt.set_gc_threshold(20 * 1024 * 1024).await;
    rt.set_loader(resolver, loader).await;

    let ctx = AsyncContext::full(&rt).await?;
    ctx.with(|ctx| {
        BasePrimordials::init(&ctx)?;
        globals.attach(&ctx)?;
        embedded_init(&ctx)?;
        numbers::redefine_prototype(&ctx)?;
        json::redefine_static_methods(&ctx)?;
        let g = ctx.globals();
        g.set("global", g.clone())?;
        g.set("self", g.clone())?;
        let _ = ctx.store_userdata(std::sync::Arc::new(Invocation::default()));
        Ok::<_, Error>(())
    })
    .await?;

    Ok((rt, ctx))
}

/// 停这个 runtime 的在途 op。只影响它自己。
pub async fn stop_invocation(ctx: &AsyncContext) -> Result<()> {
    let inv = ctx.with(|ctx| invocation(&ctx)).await?;
    inv.stop();
    Ok(())
}

/// ESM + top-level await。结果由插件自己写进 globalThis，Rust 侧只负责驱动到 idle。
pub async fn start_module(ctx: &AsyncContext, src: &str) -> Result<()> {
    ctx.with(|ctx| {
        let mut opts = EvalOptions::default();
        opts.strict = true;
        opts.promise = true;
        opts.global = false;
        let v: Value = ctx.eval_with_options(src, opts)?;
        ctx.globals().set("__pending", v)?;
        Ok::<_, Error>(())
    })
    .await
}

pub async fn run_module(ctx: &AsyncContext, rt: &AsyncRuntime, src: &str) -> Result<()> {
    start_module(ctx, src).await?;
    rt.idle().await;
    Ok(())
}

pub async fn read_string(ctx: &AsyncContext, name: &str) -> String {
    ctx.with(|ctx| {
        ctx.globals()
            .get::<_, Option<String>>(name)
            .unwrap_or(None)
            .unwrap_or_else(|| "<unset>".into())
    })
    .await
}