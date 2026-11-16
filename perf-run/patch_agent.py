import sys
from pathlib import Path
p = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/hostapi.rs')
src = p.read_text()
orig = src
def rep(old, new, count=1, name=''):
    global src
    n = src.count(old)
    if n != count:
        print(f'FAIL {name}: {n} != {count}'); sys.exit(1)
    src = src.replace(old, new); print('ok', name)

rep("""pub fn build_agent(timeout_ms: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(timeout_ms)))
        .build()
        .new_agent()
}""",
"""pub fn build_agent(timeout_ms: u64) -> ureq::Agent {
    build_agent_uncached(timeout_ms)
}

fn build_agent_uncached(timeout_ms: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(timeout_ms)))
        .build()
        .new_agent()
}

/// 按 timeout_ms 缓存 agent。
///
/// # 为什么要缓存
///
/// 新建一个 agent 要重新解析一整包根证书（rustls webpki-roots 约 150 张），
/// 一次 100-300KB 的瞬时分配。24h 长跑里插件有 1492 次 http_fetch，
/// 这些"大块分配反复建销"把 libmalloc 的高水位顶上去且不还页 ——
/// 和热更新反复重建 Context 是同一类问题。
///
/// 缓存还顺带复用了连接池。表最多 8 个条目（插件通常只用一两种 timeout），
/// 不会无界增长。
static AGENT_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, ureq::Agent>>> =
    std::sync::OnceLock::new();

pub fn build_agent_cached(timeout_ms: u64) -> ureq::Agent {
    let cache = AGENT_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Ok(mut g) = cache.lock() {
        if let Some(a) = g.get(&timeout_ms) {
            return a.clone();
        }
        let a = build_agent_uncached(timeout_ms);
        if g.len() < 8 {
            g.insert(timeout_ms, a.clone());
        }
        return a;
    }
    build_agent_uncached(timeout_ms)
}""",
name='agent-cache')

rep("        let agent = build_agent(timeout_ms);",
    "        let agent = build_agent_cached(timeout_ms);",
    name='use-cache')
p.write_text(src)
print('written +%d' % (len(src)-len(orig)))
