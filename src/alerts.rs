//! 进程内的「运行告警」：数据库写不进去时也要能说出来的话。
//!
//! 为什么要它：日志/通知都是**先落库**再算「记下来了」。磁盘满的时候落库失败，
//! 那行日志就没了 —— 界面上什么都没变，人只会觉得「面板变哑了」。
//! 这里放一个**纯内存**的环（不碰磁盘），界面上以黄条显示。
//!
//! 判重：同一个 `scope + message` 连着来只算一条，`count` 累加 —— 磁盘满时每批日志
//! 都失败，不去重就是几十条一模一样的黄条。

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde::Serialize;

/// 最多留多少条：多了没用，界面上也放不下。
pub const CAP: usize = 16;

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub id: u64,
    pub ts: i64,
    /// info / warn / error
    pub level: &'static str,
    /// 谁的问题：`logs` / `notify` / `storage` …
    pub scope: String,
    pub message: String,
    /// 同一个问题连着发生的次数（界面显示 ×N）。
    pub count: u32,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    items: VecDeque<Alert>,
}

pub struct Alerts {
    inner: Mutex<Inner>,
}

impl Default for Alerts {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }
}

impl Alerts {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记一条。同一个 `scope + message` 且**紧挨着**上一条时只累加次数。
    pub fn push(&self, level: &'static str, scope: &str, message: impl Into<String>) {
        let message = message.into();
        let now = chrono::Local::now().timestamp_millis();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last) = g.items.back_mut() {
            if last.scope == scope && last.message == message {
                last.count = last.count.saturating_add(1);
                last.ts = now;
                return;
            }
        }
        g.next_id += 1;
        let id = g.next_id;
        g.items.push_back(Alert {
            id,
            ts: now,
            level,
            scope: scope.to_string(),
            message,
            count: 1,
        });
        while g.items.len() > CAP {
            g.items.pop_front();
        }
    }

    pub fn list(&self) -> Vec<Alert> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.items.iter().cloned().collect()
    }

    /// 人点「知道了」：清空。返回清掉的条数。
    pub fn clear(&self) -> usize {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let n = g.items.len();
        g.items.clear();
        n
    }
}

/// 进程级单例：日志、通知、界面都从这里取。
pub fn global() -> &'static Alerts {
    static A: OnceLock<Alerts> = OnceLock::new();
    A.get_or_init(Alerts::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_problem_in_a_row_is_one_line_with_a_count() {
        let a = Alerts::new();
        for _ in 0..5 {
            a.push("error", "logs", "日志没写进库");
        }
        let list = a.list();
        assert_eq!(list.len(), 1, "连着 5 次只该留一条: {list:?}");
        assert_eq!(list[0].count, 5);
    }

    #[test]
    fn a_different_problem_after_it_is_its_own_line() {
        let a = Alerts::new();
        a.push("error", "logs", "A");
        a.push("error", "logs", "B");
        a.push("error", "logs", "A");
        let list = a.list();
        assert_eq!(list.len(), 3, "中间的 B 打断了合并: {list:?}");
        assert_eq!(list[2].count, 1);
        assert!(list[0].id < list[1].id && list[1].id < list[2].id, "id 要单调: {list:?}");
    }

    #[test]
    fn the_ring_keeps_at_most_cap_items_and_keeps_the_newest() {
        let a = Alerts::new();
        for i in 0..(CAP + 5) {
            a.push("warn", "notify", format!("第 {i} 条"));
        }
        let list = a.list();
        assert_eq!(list.len(), CAP);
        assert_eq!(list[CAP - 1].message, format!("第 {} 条", CAP + 4), "最新的要在最后");
    }

    #[test]
    fn clearing_empties_it_and_ids_keep_growing() {
        let a = Alerts::new();
        a.push("error", "logs", "x");
        let first = a.list()[0].id;
        assert_eq!(a.clear(), 1);
        assert!(a.list().is_empty());
        a.push("error", "logs", "x");
        assert!(a.list()[0].id > first, "清空后 id 不能回到 1（否则界面上的「已看过」会错）");
    }
}
