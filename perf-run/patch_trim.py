import sys
from pathlib import Path

p = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/hostapi.rs')
s = p.read_text()
old = """pub fn build_agent(timeout_ms: u64) -> ureq::Agent {
    build_agent_cached(timeout_ms)
}"""
new = """pub fn build_agent(timeout_ms: u64) -> ureq::Agent {
    // 默认**不缓存**。实测（24h 长跑）：缓存 agent 后，面板里多出 8-12 块 128KB 的
    // 常驻连接缓冲（约 1.0-1.5MB），比它省下的"每次重建根证书"碎片更贵 ——
    // 同一时刻对照：不缓存 7.09MB / 缓存 8.45MB。延迟确实从 574ms 降到 217ms，
    // 所以留成开关：想要低延迟且能接受这点常驻的人设 PLUGIN_HTTP_AGENT_CACHE=1。
    if agent_cache_enabled() {
        build_agent_cached(timeout_ms)
    } else {
        build_agent_uncached(timeout_ms)
    }
}

fn agent_cache_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("PLUGIN_HTTP_AGENT_CACHE")
            .map(|v| matches!(v.trim(), "1" | "on" | "true" | "yes"))
            .unwrap_or(false)
    })
}"""
assert s.count(old) == 1
s = s.replace(old, new)
p.write_text(s)
print('ok hostapi default-off')

m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
anchor = """    log::plog!(
        "boot",
        info,
        "tokio runtime 就绪（{workers} 个 worker 线程）"
    );"""
add = anchor + """

    // 周期把分配器里**空闲**的页还给系统（macOS: malloc_zone_pressure_relief；
    // glibc: malloc_trim）。为什么需要：QuickJS 每次热更新/装回都是"建一个 Context
    // 再销毁"，每次 http_fetch 也是"建一个客户端再销毁" —— libmalloc 会把释放的页
    // 留在自己的空闲链表里，从不主动还给系统。实测 24h 常驻从 5.6MB 爬到 11MB，
    // 而 JS 堆只有 0.9MB，涨的全是这些"高水位"。
    //
    // 每分钟一次、每次几十微秒；真归还了才打日志。PLUGIN_MEM_TRIM_MS=0 关闭。
    spawn_mem_trim();"""
assert t.count(anchor) == 1
t = t.replace(anchor, add)

funcs = """

/// 周期还页线程（调用点在 real_main）。
fn spawn_mem_trim() {
    let ms = std::env::var("PLUGIN_MEM_TRIM_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60_000);
    if ms == 0 {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("mem-trim".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(ms));
            let freed = unsafe { platform_trim() };
            if freed > 0 {
                log::plog!(
                    "kernel",
                    info,
                    "归还 {:.1} KB 空闲内存给系统",
                    freed as f64 / 1024.0
                );
            }
        });
}

#[cfg(target_os = "macos")]
unsafe fn platform_trim() -> usize {
    extern "C" {
        fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
    }
    unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
unsafe fn platform_trim() -> usize {
    if unsafe { libc::malloc_trim(0) } == 1 {
        1
    } else {
        0
    }
}

#[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
unsafe fn platform_trim() -> usize {
    0
}
"""
t = t.rstrip() + funcs + "\n"
m.write_text(t)
print('ok main trim')
