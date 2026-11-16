import sys
from pathlib import Path
p = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/hostapi.rs')
src = p.read_text()
def rep(old, new, count=1, name=''):
    global src
    n = src.count(old)
    if n != count: print(f'FAIL {name}: {n}!={count}'); sys.exit(1)
    src = src.replace(old, new); print('ok', name)

rep("""pub fn build_agent(timeout_ms: u64) -> ureq::Agent {
    build_agent_uncached(timeout_ms)
}

fn build_agent_uncached""",
"""pub fn build_agent(timeout_ms: u64) -> ureq::Agent {
    build_agent_cached(timeout_ms)
}

fn build_agent_uncached""",
name='wrapper')

rep("        let agent = build_agent_cached(timeout_ms);",
    "        let agent = build_agent(timeout_ms);",
    name='call-site')
p.write_text(src)
print('ok')
