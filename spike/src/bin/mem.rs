// Stage 0 判据 3：1 插件 = 1 Vm 的常驻内存边际成本。
// 只报边际成本与末段斜率；不同隔离模型之间的比值不当效率判决。
use spike::{
    engine::{assemble, global_inits, run_module},
    footprint::footprint_mb,
};

const PLUGIN: &str = r#"
import host from 'panel:host';
globalThis.handler = async (evt) => { await host.sleep(1); return evt; };
globalThis.__ready = 'ok';
"#;

#[tokio::main]
async fn main() {
    global_inits();
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    println!("pid={} n={n}", std::process::id());
    let base = footprint_mb();
    println!("tier 0 (no runtime):        {base:.3} MiB");

    let mut vms = Vec::with_capacity(n);
    let mut marks = vec![(0usize, base)];
    for i in 0..n {
        let (rt, ctx) = assemble().await.expect("assemble");
        run_module(&ctx, &rt, PLUGIN).await.expect("load plugin");
        vms.push((rt, ctx));
        let f = footprint_mb();
        if i + 1 == 1 || i + 1 == 2 || (i + 1) % 5 == 0 || i + 1 == n {
            println!(
                "tier 1 ({:>3} plugin loaded): {f:.3} MiB   marginal {:.3} MiB/plugin",
                i + 1,
                (f - base) / (i + 1) as f64
            );
            marks.push((i + 1, f));
        }
    }

    // 末段斜率：最后 5 个的边际，避开首个 runtime 的一次性开销
    if marks.len() >= 2 {
        let (na, fa) = marks[marks.len() - 2];
        let (nb, fb) = marks[marks.len() - 1];
        if nb > na {
            println!(
                "tail slope ({}→{}): {:.3} MiB/plugin",
                na,
                nb,
                (fb - fa) / (nb - na) as f64
            );
        }
    }

    let before_cache = footprint_mb();
    let (rt0, ctx0) = &vms[0];
    run_module(
        ctx0,
        rt0,
        "globalThis.cache = Array.from({length: 20*1024}, () => 'x'.repeat(1024)); globalThis.__c = String(cache.length);",
    )
    .await
    .expect("cache");
    let after_cache = footprint_mb();
    println!(
        "tier 2 (plugin 0 + 20 MiB cache): {after_cache:.3} MiB   delta {:.3} MiB",
        after_cache - before_cache
    );

    let last = marks.last().map(|(_, f)| *f).unwrap_or(base);
    println!(
        "summary: base {base:.3} / loaded {last:.3} / +cache {after_cache:.3} MiB"
    );

    // 给 /usr/bin/footprint 留出交叉校验的窗口
    if let Ok(secs) = std::env::var("HOLD_SECS") {
        if let Ok(secs) = secs.parse::<u64>() {
            println!("holding {secs}s for external cross-check (pid {})", std::process::id());
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
        }
    }

    drop(vms);
    println!("after dropping all runtimes: {:.3} MiB", footprint_mb());
}