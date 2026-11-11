// Stage 0 判据 6：长 async 任务在跑时，50ms 周期任务的 p99 是否被拖长；
// 以及停 A 是否只停 A（旧面板 R1 High 的反向例）。
//
// 每个插件跑在自己的 tokio task 上——tokio::join! 会在同一个 task 里轮询两个
// future，那样测出来的是串行，不是隔离。
//
// 环境变量：LONG_MS / TICKS / INTERVAL_MS / BURN_MS / STOP_A_MS
use std::time::{Duration, Instant};

use spike::{
    engine::{assemble, global_inits, read_string, start_module},
    footprint::footprint_mb,
};

fn env_ms(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn report(tag: &str, mut s: Vec<f64>, ticks: u64) {
    s.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let n = s.len();
    let sum: f64 = s.iter().sum();
    println!(
        "[{tag}] ticks={n}/{ticks} mean={:.2}ms p50={:.2}ms p99={:.2}ms max={:.2}ms",
        if n > 0 { sum / n as f64 } else { f64::NAN },
        pct(&s, 0.50),
        pct(&s, 0.99),
        s.last().copied().unwrap_or(f64::NAN)
    );
}

async fn pure_rust_control(long: u64, ticks: u64, interval: u64, burn: u64) {
    let burn_thread = std::env::var("BURN_THREAD").unwrap_or_else(|_| "tokio".into());
    let tick_thread = std::env::var("TICK_THREAD").unwrap_or_else(|_| "tokio".into());
    println!(
        "PURE_RUST config: long={long}ms ticks={ticks} interval={interval}ms burn={burn}ms burn_thread={burn_thread} tick_thread={tick_thread}"
    );

    let burner = move || {
        let t0 = Instant::now();
        if burn > 0 {
            while t0.elapsed() < Duration::from_millis(long) {
                std::thread::sleep(Duration::from_millis(1000));
                let b = Instant::now();
                while b.elapsed() < Duration::from_millis(burn) {
                    std::hint::spin_loop();
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(long));
        }
        format!("completed {}ms", t0.elapsed().as_millis())
    };
    let ticker = move || {
        let mut samples = Vec::new();
        let mut last = Instant::now();
        for _ in 0..ticks {
            std::thread::sleep(Duration::from_millis(interval));
            let now = Instant::now();
            samples.push(now.duration_since(last).as_secs_f64() * 1000.0);
            last = now;
        }
        samples
    };

    let hd: tokio::task::JoinHandle<String> = if burn_thread == "std" {
        let h = std::thread::spawn(burner);
        tokio::spawn(async move { h.join().expect("burner thread") })
    } else {
        tokio::spawn(async move {
            let t0 = Instant::now();
            if burn > 0 {
                while t0.elapsed() < Duration::from_millis(long) {
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                    let b = Instant::now();
                    while b.elapsed() < Duration::from_millis(burn) {
                        std::hint::spin_loop();
                    }
                }
            } else {
                tokio::time::sleep(Duration::from_millis(long)).await;
            }
            format!("completed {}ms", t0.elapsed().as_millis())
        })
    };
    let hc: tokio::task::JoinHandle<Vec<f64>> = if tick_thread == "std" {
        let h = std::thread::spawn(ticker);
        tokio::spawn(async move { h.join().expect("ticker thread") })
    } else {
        tokio::spawn(async move {
            let mut samples = Vec::new();
            let mut last = Instant::now();
            for _ in 0..ticks {
                tokio::time::sleep(Duration::from_millis(interval)).await;
                let now = Instant::now();
                samples.push(now.duration_since(last).as_secs_f64() * 1000.0);
                last = now;
            }
            samples
        })
    };
    let (d, c) = tokio::join!(hd, hc);
    println!("[D-rust-burn] {}", d.expect("task D"));
    report("C-rust", c.expect("task C"), ticks);
}

fn main() {
    let workers: usize = std::env::var("WORKERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder.enable_all();
    if workers > 0 {
        builder.worker_threads(workers);
    }
    let rt = builder.build().expect("tokio runtime");
    println!(
        "available_parallelism={:?} workers={}",
        std::thread::available_parallelism().map(|n| n.get()),
        if workers > 0 { workers } else { 0 }
    );
    rt.block_on(run());
}

async fn run() {
    global_inits();
    let long = env_ms("LONG_MS", 10_000);
    let ticks = env_ms("TICKS", 200);
    let interval = env_ms("INTERVAL_MS", 50);
    let burn = env_ms("BURN_MS", 0);
    let stop_a = env_ms("STOP_A_MS", 0);

    // PURE_RUST=1：完全不建 JS 引擎，只有一个自旋的 Rust 任务 + 周期任务。
    // 用来判别"自旋任务饿死 tokio 定时器"是不是与 LLRT 无关的现象。
    if std::env::var("PURE_RUST").is_ok() {
        pure_rust_control(long, ticks, interval, burn).await;
        return;
    }

    println!(
        "config: long={long}ms ticks={ticks} interval={interval}ms burn={burn}ms stop_a={stop_a}ms"
    );

    let (rt_a, ctx_a) = assemble().await.expect("assemble A");
    let (rt_b, ctx_b) = assemble().await.expect("assemble B");

    // CPU 预算：死线取该 invocation 的墙钟 deadline，不是固定切片。
    // 20 分钟任务配 25 分钟死线，用来证明预算机制不会误杀长任务。
    let deadline_ms = env_ms("DEADLINE_MS", 0);
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    if deadline_ms > 0 {
        let deadline = Instant::now() + Duration::from_millis(deadline_ms);
        let f = fired.clone();
        rt_a.set_interrupt_handler(Some(Box::new(move || {
            let hit = Instant::now() >= deadline;
            if hit {
                f.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            hit
        })))
        .await;
        println!("[budget] A interrupt deadline = +{deadline_ms}ms");
    }

    let burn_mode = std::env::var("BURN_MODE").unwrap_or_else(|_| "date".into());
    let b_clock = std::env::var("B_CLOCK").unwrap_or_else(|_| "perf".into());
    let burn_body = if burn_mode == "count" {
        format!("let s = 0; for (let i = 0; i < {burn} * 20000; i++) s += i; globalThis.__sink = s;")
    } else {
        format!("const b = Date.now(); while (Date.now() - b < {burn}) {{}}")
    };
    let clock_expr = if b_clock == "date" { "Date.now()" } else { "performance.now()" };

    let src_a = format!(
        r#"
import host from 'panel:host';
const t0 = Date.now();
const long = {long}, burn = {burn};
try {{
  if (burn > 0) {{
    while (Date.now() - t0 < long) {{
      await host.sleep(1000);
      {burn_body}
    }}
  }} else {{
    await host.sleep(long);
  }}
  globalThis.__a = 'completed ' + (Date.now() - t0) + 'ms';
}} catch (e) {{
  globalThis.__a = 'aborted ' + (Date.now() - t0) + 'ms: ' + (e.message ?? e);
}}
"#
    );

    let src_b = format!(
        r#"
const samples = [];
let last = {clock_expr};
let n = 0;
const id = setInterval(() => {{
  const now = {clock_expr};
  samples.push(now - last);
  last = now;
  if (++n >= {ticks}) {{
    clearInterval(id);
    globalThis.__b = samples.map((x) => x.toFixed(3)).join(',');
  }}
}}, {interval});
"#
    );

    start_module(&ctx_a, &src_a).await.expect("start A");
    start_module(&ctx_b, &src_b).await.expect("start B");

    // idle() 全程持有 runtime 锁，with() 也要同一把锁。
    // 所以取消必须在 idle 之前把 token 拿到手，之后纯 Rust 触发，不再碰 JS 锁。
    let inv_a = ctx_a
        .with(|ctx| spike::engine::invocation(&ctx))
        .await
        .expect("invocation A");

    let ha = tokio::spawn(async move {
        rt_a.idle().await;
        read_string(&ctx_a, "__a").await
    });
    let hb = tokio::spawn(async move {
        rt_b.idle().await;
        read_string(&ctx_b, "__b").await
    });

    // 对照组：完全不碰 JS 的纯 Rust 周期任务。它若也抖，问题在 tokio/机器层；
    // 它若不抖而 B 抖，问题在引擎层。
    let hc = tokio::spawn(async move {
        let mut samples = Vec::new();
        let mut last = Instant::now();
        for _ in 0..ticks {
            tokio::time::sleep(Duration::from_millis(interval)).await;
            let now = Instant::now();
            samples.push(now.duration_since(last).as_secs_f64() * 1000.0);
            last = now;
        }
        samples
    });

    if stop_a > 0 {
        tokio::time::sleep(Duration::from_millis(stop_a)).await;
        println!("[stop] stopping A's invocation at +{stop_a}ms; B untouched");
        inv_a.stop();
    }

    // 长任务期间定期报常驻内存与机器负载。负载必须一起报：
    // 本机是共享的，p99 抖动不注明负载就无法归因。
    let report_secs = env_ms("REPORT_SECS", 0);
    let hr = tokio::spawn(async move {
        if report_secs == 0 {
            return;
        }
        let t0 = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_secs(report_secs)).await;
            let load = std::fs::read_to_string("/proc/loadavg")
                .ok()
                .or_else(|| {
                    std::process::Command::new("sysctl")
                        .args(["-n", "vm.loadavg"])
                        .output()
                        .ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                })
                .unwrap_or_default();
            println!(
                "[t+{}s] footprint={:.3} MiB load={load}",
                t0.elapsed().as_secs(),
                footprint_mb()
            );
        }
    });

    let t0 = Instant::now();
    let (a, b, c) = tokio::join!(ha, hb, hc);
    let wall = t0.elapsed();
    hr.abort();
    let a = a.expect("task A");
    let b = b.expect("task B");
    let mut c = c.expect("task C");
    c.sort_by(|x, y| x.partial_cmp(y).unwrap());

    let mut samples: Vec<f64> = b.split(',').filter_map(|s| s.parse().ok()).collect();
    samples.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let n = samples.len();
    let sum: f64 = samples.iter().sum();
    println!("[A] {a}");
    println!(
        "[B] ticks={n}/{ticks} mean={:.2}ms p50={:.2}ms p99={:.2}ms max={:.2}ms",
        if n > 0 { sum / n as f64 } else { f64::NAN },
        pct(&samples, 0.50),
        pct(&samples, 0.99),
        samples.last().copied().unwrap_or(f64::NAN)
    );
    println!("[wall after stop] {wall:?}");
    let cn = c.len();
    let csum: f64 = c.iter().sum();
    println!(
        "[C-rust] ticks={cn}/{ticks} mean={:.2}ms p50={:.2}ms p99={:.2}ms max={:.2}ms",
        if cn > 0 { csum / cn as f64 } else { f64::NAN },
        pct(&c, 0.50),
        pct(&c, 0.99),
        c.last().copied().unwrap_or(f64::NAN)
    );
    println!("[mem] {:.3} MiB", footprint_mb());
    if deadline_ms > 0 {
        println!(
            "[budget] interrupt fired={} (long task must not be killed by its own budget)",
            fired.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    if stop_a > 0 {
        let b_ok = n as u64 == ticks;
        let a_stopped = a.starts_with("aborted");
        println!(
            "[isolation] A stopped={a_stopped} B completed all ticks={b_ok} -> {}",
            if a_stopped && b_ok { "PASS" } else { "FAIL" }
        );
    }
}