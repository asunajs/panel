// Stage 0 判据 2 + 4 的场景驱动。组装路径见 spike/src/engine.rs。
use std::time::{Duration, Instant};

use llrt_core::{context::EvalOptions, libs::context::set_spawn_error_handler, Value};
use spike::{
    engine::{assemble, global_inits, read_string, run_module},
    footprint::footprint_mb,
};

#[tokio::main]
async fn main() {
    set_spawn_error_handler(|_ctx, err| {
        eprintln!("    [spawn-error] {err:?} (process kept alive)");
    });
    global_inits();

    println!("footprint before any runtime: {:.3} MiB", footprint_mb());

    // --- A: 自己组装 + native async 模块 ---
    let (rt, ctx) = match assemble().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[A] assemble FAILED: {e:?}");
            std::process::exit(1);
        }
    };
    println!(
        "[A] assembled without Vm; footprint {:.3} MiB",
        footprint_mb()
    );

    let src = r#"
import host, { sleep, info } from 'panel:host';
const t0 = Date.now();
const slept = await sleep(40);
info('async op resolved after ' + (Date.now() - t0) + 'ms');
let caught = 'none';
try { await host.boom('synthetic-host-error'); } catch (e) { caught = String(e.message ?? e); }
globalThis.__a = [
  'default-import=' + (typeof host.sleep),
  'named-import=' + (typeof sleep),
  'slept=' + slept,
  'caught=' + caught,
  'fetch=' + typeof fetch,
  'Buffer=' + typeof Buffer,
  'structuredClone=' + typeof structuredClone,
].join(' | ');
"#;
    match run_module(&ctx, &rt, src).await {
        Ok(()) => println!("[A] {}", read_string(&ctx, "__a").await),
        Err(e) => println!("[A] eval FAILED: {e:?}"),
    }

    // --- B: interrupt handler 挂在 AsyncRuntime 上。
    // rquickjs 文档明写它抛的是 uncatchable 异常，JS 侧 try/catch 抓不到，
    // 只能在 Rust 侧接 eval 的 Err。所以它不能用来做协作式让出。
    let deadline = Instant::now() + Duration::from_millis(200);
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)))
        .await;
    let t0 = Instant::now();
    let busy: Result<(), String> = ctx
        .with(|ctx| {
            let mut opts = EvalOptions::default();
            opts.global = true;
            opts.promise = false;
            ctx.eval_with_options::<Value, _>("while (true) {}", opts)
                .map(|_| ())
                .map_err(|e| format!("{e:?}"))
        })
        .await;
    let elapsed = t0.elapsed();
    match busy {
        Ok(()) => println!("[B] NOT interrupted after {elapsed:?} — handler did not fire"),
        Err(e) => println!("[B] interrupted after {elapsed:?}: {e}"),
    }
    rt.set_interrupt_handler(None).await;
    let after: Result<i32, String> = ctx
        .with(|ctx| {
            ctx.eval_with_options::<i32, _>("1 + 1", EvalOptions::default())
                .map_err(|e| format!("{e:?}"))
        })
        .await;
    println!("[B] runtime usable after interrupt: {after:?}");

    // --- C: memory limit 挂在 AsyncRuntime 上，抛的是可捕获的 OOM ---
    rt.set_memory_limit(16 * 1024 * 1024).await;
    let hog = r#"
try {
  const a = [];
  for (;;) a.push('x'.repeat(4096));
} catch (e) {
  globalThis.__c = 'caught=' + e.name + ': ' + e.message;
}
"#;
    match run_module(&ctx, &rt, hog).await {
        Ok(()) => println!("[C] {}", read_string(&ctx, "__c").await),
        Err(e) => println!("[C] eval returned Err (process alive): {e:?}"),
    }
    rt.set_memory_limit(0).await;
    println!(
        "[C] process still alive after memory limit; footprint {:.3} MiB",
        footprint_mb()
    );

    // --- D: 第二个 runtime 与第一个并存，互不影响 ---
    let (rt2, ctx2) = assemble().await.expect("second assemble");
    run_module(
        &ctx2,
        &rt2,
        "globalThis.__d = 'second-runtime-ok ' + (typeof fetch);",
    )
    .await
    .expect("second eval");
    println!("[D] {}", read_string(&ctx2, "__d").await);
    println!(
        "[D] two runtimes coexist; footprint {:.3} MiB",
        footprint_mb()
    );

    run_module(&ctx, &rt, "globalThis.__d2 = 'first-still-works';")
        .await
        .expect("first still works");
    println!("[D] {}", read_string(&ctx, "__d2").await);

    drop(ctx2);
    drop(rt2);
    println!("[D] after dropping second: {:.3} MiB", footprint_mb());
    drop(ctx);
    drop(rt);
    println!("[D] after dropping first:  {:.3} MiB", footprint_mb());
}