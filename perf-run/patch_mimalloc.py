from pathlib import Path
p = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
s = p.read_text()
anchor = "fn main() {"
add = """/// 全局分配器换成 mimalloc。
///
/// 为什么：QuickJS 的 Context 反复建销、每次 http_fetch 建一个 agent，都是"大块分配
/// 反复建销"。libmalloc 会把高水位留住不还页 —— 24h 实测常驻 5.6MB -> 11MB，
/// 而 JS 堆只有 0.9MB；试过 malloc_zone_pressure_relief，返回 0（手里没有"空闲页"）。
/// mimalloc 在页空闲后按 decay 主动还给系统，正好对症。
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {"""
assert s.count(anchor) == 1
s = s.replace(anchor, add)
p.write_text(s)
print('ok global allocator')
