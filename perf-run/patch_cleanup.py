from pathlib import Path
e = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
s = e.read_text()
# 去掉不再使用的 NEXT_FIRE_MS / next_fire_in_ms 以及 maintain 里的刷新代码
start = s.index('/// 最近一次「下一次触发」的时刻（epoch 毫秒，0 = 没有任务）。')
end = s.index('pub fn next_fire_in_ms() -> i64 {')
end = s.index('}', s.index('v - now', end)) + 1
s = s[:start] + s[end:]
s = s.replace("""
    // 记录最近的下一次触发时刻（内存自愈据此判断「现在是不是闲时」）。
    {
        let next = sched
            .tasks()
            .iter()
            .filter(|t| t.enabled)
            .map(|t| t.next_fire.timestamp_millis())
            .min();
        NEXT_FIRE_MS.store(next.unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
    }
""", "")
e.write_text(s)
print('ok cleanup')
