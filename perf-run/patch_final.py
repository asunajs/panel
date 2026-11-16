import re
from pathlib import Path
m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
s = m.read_text()
n = len(s)
s2, cnt = re.subn(r'(?:///[^\n]*\n)*#\[global_allocator\]\nstatic GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;\n\n', '', s)
print('allocator block removed:', cnt)
s2 = s2.replace('.unwrap_or(60_000);', '.unwrap_or(0);')
print('trim default off:', '.unwrap_or(0);' in s2)
m.write_text(s2)
print('bytes %d -> %d' % (n, len(s2)))
