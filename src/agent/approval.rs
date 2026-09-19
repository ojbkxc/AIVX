//! 高危写审批矩阵（DESIGN.md §11）——对齐 AIGX `src/agent/approval.rs`。
//!
//! 高危工具（删设备/删录像）挂起等人工确认；`RememberAllow` 记入本会话免审集。
//! 审批通过才执行，拒绝/超时视为拒绝。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

/// 审批结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResult {
    Approved,
    Denied,
    /// 本会话总是允许（免审）。
    RememberAllow,
}

/// 审批矩阵。
#[derive(Default)]
pub struct AgentApprovals {
    /// request_id → oneshot 发送端（审批 UI 调用 resolve）。
    pending: Mutex<HashMap<String, std::sync::mpsc::Sender<ApprovalResult>>>,
    /// 本会话免审工具集（session_id → tool 名）。
    remembered: Mutex<HashMap<String, Vec<String>>>,
}

    pub fn is_remembered(&self, session_id: &str, tool: &str) -> bool {
        self.remembered
            .lock()
            .unwrap()
            .get(session_id)
            .map(|v| v.iter().any(|t| t == tool))
            .unwrap_or(false)
    }

    pub fn remember(&self, session_id: &str, tool: &str) {
        self.remembered
            .lock()
            .unwrap()
            .entry(session_id.into())
            .or_default()
            .push(tool.into());
    }

    /// 发起审批：返回 request_id + 接收端。调用方等待结果（超时视为拒绝）。
    pub fn request(&self, tool: &str) -> (String, std::sync::mpsc::Receiver<ApprovalResult>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let req_id = format!("req-{tool}-{}", crate::agent::runner::next_req_seq());
        self.pending.lock().unwrap().insert(req_id.clone(), tx);
        (req_id, rx)
    }

    /// 审批解决：Approved/Denied/RememberAllow。
    pub fn resolve(&self, request_id: &str, result: ApprovalResult) -> bool {
        if let Some(tx) = self.pending.lock().unwrap().remove(request_id) {
            let _ = tx.send(result);
            true
        } else {
            false
        }
    }

    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }
}

/// 审批超时（默认 300s，0/缺省回退）。
pub fn approval_timeout_from(secs: u64) -> Duration {
    if secs == 0 {
        Duration::from_secs(300)
    } else {
        Duration::from_secs(secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_lifecycle() {
        let ap = AgentApprovals::default();
        let (req_id, rx) = ap.request("nvr_delete_device");
        assert_eq!(ap.pending_count(), 1);
        assert!(ap.resolve(&req_id, ApprovalResult::Approved));
        assert_eq!(ap.pending_count(), 0);
        // 接收端拿到结果
        assert_eq!(rx.recv().unwrap(), ApprovalResult::Approved);
    }

    #[test]
    fn remember_allow_bypasses_next_time() {
        let ap = AgentApprovals::default();
        ap.remember("s-1", "nvr_delete_device");
        assert!(ap.is_remembered("s-1", "nvr_delete_device"));
        assert!(!ap.is_remembered("s-1", "nvr_snapshot"));
        assert!(!ap.is_remembered("s-2", "nvr_delete_device"));
    }
}
