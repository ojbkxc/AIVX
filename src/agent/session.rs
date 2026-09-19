//! 会话持久化（DESIGN.md §11）——对齐 AIGX `src/agent/session.rs`。
//!
//! 角色：观察员（默认，只读工具）vs 运维员（解锁写工具）。会话存标题/角色/消息。

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 会话角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    /// 观察员：只读工具，写工具一律拒绝。
    Observer,
    /// 运维员：读写工具均可（高危仍需审批）。
    Operator,
}

impl AgentRole {
    pub fn allows_write(&self) -> bool {
        matches!(self, AgentRole::Operator)
    }
}

impl Default for AgentRole {
    fn default() -> Self {
        AgentRole::Observer
    }
}

/// 一条会话消息。
#[derive(Debug, Clone)]
pub struct AgentMessage {
    pub role: String,
    pub content: String,
    pub tool_calls: Option<Vec<String>>,
    pub tool_result: Option<String>,
}

/// 会话。
#[derive(Debug, Clone)]
pub struct AgentSession {
    pub id: String,
    pub title: String,
    pub role: AgentRole,
}

/// 会话存储（P5 桩：内存 HashMap；P6 换事件溯源的会话表或 SeaORM）。
#[derive(Default)]
pub struct SessionStore {
    inner: Mutex<std::collections::HashMap<String, AgentSession>>,
    msgs: Mutex<std::collections::HashMap<String, Vec<AgentMessage>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(&self, id: &str, title: &str, role: AgentRole) {
        self.inner.lock().unwrap().insert(
            id.into(),
            AgentSession {
                id: id.into(),
                title: title.into(),
                role,
            },
        );
        self.msgs.lock().unwrap().entry(id.into()).or_default();
    }

    pub fn get(&self, id: &str) -> Option<AgentSession> {
        self.inner.lock().unwrap().get(id).cloned()
    }

    pub fn rename_if_default(&self, id: &str, first_message: &str) {
        let mut map = self.inner.lock().unwrap();
        if let Some(s) = map.get_mut(id) {
            if s.title.is_empty() || s.title == "新会话" {
                s.title = first_message.chars().take(30).collect();
            }
        }
    }

    pub fn append_message(&self, id: &str, msg: AgentMessage) {
        self.msgs
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .push(msg);
    }

    pub fn messages(&self, id: &str) -> Vec<AgentMessage> {
        self.msgs
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn list(&self) -> Vec<AgentSession> {
        self.inner.lock().unwrap().values().cloned().collect()
    }

    pub fn delete(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
        self.msgs.lock().unwrap().remove(id);
    }
}

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lifecycle() {
        let store = SessionStore::new();
        store.create("s-1", "", AgentRole::Observer);
        store.rename_if_default("s-1", "查一下昨晚的报警");
        let s = store.get("s-1").unwrap();
        assert_eq!(s.title, "查一下昨晚的报警");
        assert_eq!(s.role, AgentRole::Observer);
        assert!(!s.role.allows_write());
        // 运维员可写
        store.create("s-2", "", AgentRole::Operator);
        assert!(store.get("s-2").unwrap().role.allows_write());
    }
}
