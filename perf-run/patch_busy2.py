from pathlib import Path
e = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
s = e.read_text()

i = s.index('fn dispatch_route(')
j = s.index(') -> String {', i) + len(') -> String {')
if 'BusyGuard::new()' not in s[i:j+80]:
    s = s[:j] + '\n    let _busy = BusyGuard::new();' + s[j:]
print('ok route guard')

old2 = """        if self.runtime.is_none() || !self.has_on_tick {
            return;
        }
        let name = self.name.clone();"""
if 'BusyGuard::new()' not in s[s.index(old2):s.index(old2)+300] if old2 in s else True:
    pass
if s.count(old2) == 1:
    s = s.replace(old2, """        if self.runtime.is_none() || !self.has_on_tick {
            return;
        }
        let _busy = BusyGuard::new();
        let name = self.name.clone();""")
    print('ok tick guard')
else:
    print('tick anchor count', s.count(old2))
e.write_text(s)
