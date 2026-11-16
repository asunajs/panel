from pathlib import Path
e = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
s = e.read_text()
s = s.replace("        use chrono::TimeZone as _;\n", "")
e.write_text(s)
print('ok import removed')
