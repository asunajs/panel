from pathlib import Path
e = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
s = e.read_text()

old1 = """    budget: Duration,
) -> String {
    // 路由也能声明自己的预算（register_route(…, { budget_ms })）。"""
new1 = """    budget: Duration,
) -> String {
    let _busy = BusyGuard::new();
    // 路由也能声明自己的预算（register_route(…, { budget_ms })）。"""
assert s.count(old1) == 1
s = s.replace(old1, new1)

old2 = """        if self.runtime.is_none() || !self.has_on_tick {
            return;
        }
        let name = self.name.clone();"""
new2 = """        if self.runtime.is_none() || !self.has_on_tick {
            return;
        }
        let _busy = BusyGuard::new();
        let name = self.name.clone();"""
assert s.count(old2) == 1
s = s.replace(old2, new2)
e.write_text(s)
print('ok route+tick guards')
