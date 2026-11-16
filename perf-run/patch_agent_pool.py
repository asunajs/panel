import sys
from pathlib import Path
p = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/hostapi.rs')
src = p.read_text()
def rep(old, new, name=''):
    global src
    n = src.count(old)
    if n != 1: print('FAIL', name, n); sys.exit(1)
    src = src.replace(old, new); print('ok', name)

rep("""fn build_agent_uncached(timeout_ms: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(timeout_ms)))
        .build()
        .new_agent()
}""",
"""fn build_agent_uncached(timeout_ms: u64) -> ureq::Agent {
    // 连接池要**收紧**：ureq 默认最多 10 条空闲连接、每 host 3 条、15 秒才过期，
    // 每条带 128KB 读缓冲。实测缓存 agent 后面板里多出 8-12 块 128KB 常驻
    // （约 1.0-1.5MB），比它省下来的碎片还多。这里压到 3 条总量 / 每 host 1 条 /
    // 5 秒过期 —— 仍然复用连接（延迟实测 574ms -> 217ms），但缓冲有界。
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_millis(timeout_ms)))
        .max_idle_connections(3)
        .max_idle_connections_per_host(1)
        .max_idle_age(std::time::Duration::from_secs(5))
        .build()
        .new_agent()
}""",
name='pool-caps')
p.write_text(src)
print('ok')
