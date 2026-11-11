//! 面板配置的**热快照**。
//!
//! 旧做法：`scheduler::start` 里把配置序列化一次成 `Arc<str>`，之后所有插件共享。
//! 这样省掉了每次 `host.config()` 的序列化开销，但配置就成了**启动时快照** ——
//! 界面上改了配置文件，插件要重启才看得到。
//!
//! 现在换成这个：写入侧序列化一次，读侧只做一次 `Arc` clone（读锁覆盖范围极窄）。
//! 于是 `PUT /api/config` 之后插件立刻读到新值，面板不用重启。

use std::sync::{Arc, RwLock};

pub struct ConfigStore {
    text: RwLock<Arc<str>>,
}

impl ConfigStore {
    pub fn new(value: &serde_json::Value) -> Arc<Self> {
        Arc::new(Self {
            text: RwLock::new(Arc::from(serialize(value))),
        })
    }

    /// 取当前配置的 JSON 文本。热路径：只 clone 一个 `Arc`。
    pub fn snapshot(&self) -> Arc<str> {
        // 锁中毒不该让面板起不来：数据本身是完整的（写侧整体替换）。
        match self.text.read() {
            Ok(g) => g.clone(),
            Err(p) => p.into_inner().clone(),
        }
    }

    /// 换一份配置，返回新的 JSON 文本。
    pub fn set(&self, value: &serde_json::Value) -> Arc<str> {
        let text: Arc<str> = Arc::from(serialize(value));
        match self.text.write() {
            Ok(mut g) => *g = text.clone(),
            Err(p) => *p.into_inner() = text.clone(),
        }
        text
    }
}

fn serialize(v: &serde_json::Value) -> String {
    // 解析出来的 Value 序列化不会失败；万一失败也退成空对象，别把启动搞挂。
    serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshot_is_the_serialized_form() {
        let store = ConfigStore::new(&json!({ "greet": "hi" }));
        assert_eq!(store.snapshot().as_ref(), r#"{"greet":"hi"}"#);
    }

    #[test]
    fn set_replaces_what_plugins_see() {
        let store = ConfigStore::new(&json!({ "a": 1 }));
        let old = store.snapshot();
        store.set(&json!({ "a": 2, "b": [1, 2] }));
        assert_eq!(store.snapshot().as_ref(), r#"{"a":2,"b":[1,2]}"#);
        // 已经拿走的快照不受影响：插件正在跑的那次调用看到的是自己那一刻的值。
        assert_eq!(old.as_ref(), r#"{"a":1}"#);
    }

    #[test]
    fn empty_object_is_fine() {
        let store = ConfigStore::new(&json!({}));
        assert_eq!(store.snapshot().as_ref(), "{}");
    }
}