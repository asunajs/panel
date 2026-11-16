from pathlib import Path
m = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/main.rs')
t = m.read_text()
old = """            match crate::db::prune_runs(&db, keep_days, &now) {
                Ok(0) => {}
                Ok(n) => log::plog!("kernel", info, "清理了 {n} 条超过 {keep_days} 天的运行记录"),
                Err(e) => log::plog!("kernel", warn, "清理运行记录失败: {e}"),
            }"""
new = """            // 每轮都记一条（哪怕是 0）：否则「线程没跑」和「没有过期记录」在日志里长得一样，
            // 排查时只能靠猜。一小时一行，不吵。
            match crate::db::prune_runs(&db, keep_days, &now) {
                Ok(n) => log::plog!(
                    "kernel",
                    info,
                    "运行记录清理完成：删除 {n} 条（保留 {keep_days} 天）"
                ),
                Err(e) => log::plog!("kernel", warn, "清理运行记录失败: {e}"),
            }"""
assert t.count(old) == 1
t = t.replace(old, new)
m.write_text(t)
print('ok prune heartbeat')
