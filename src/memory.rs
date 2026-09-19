//! 内存事件库（P0 测试替身）。
//!
//! P0 用它验证 forwarder/DbWriter/Projector 的正确性（顺序 fan-out 无洞、
//! seq 单调、僵尸清扫），不引 SQLite——P1 把 `EventStore` trait 对到 SeaORM。

use std::collections::HashMap;
use std::sync::Mutex;

use aivx_events::Event;

/// 落库后的定序事件（带全局 seq）。
#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub seq: u64,
    pub event: Event,
}

/// 内存 events 表：append-only + 单写者（模拟 SQLite 语义）。
#[derive(Default)]
pub struct MemEventStore {
    inner: Mutex<Vec<StoredEvent>>,
}

impl MemEventStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// DbWriter 专用：追加一批（单事务等价）。
    pub fn append_batch(&self, events: Vec<StoredEvent>) {
        self.inner.lock().unwrap().extend(events);
    }

    /// Projector 恢复：读 checkpoint 之后的事件。
    pub fn events_after(&self, seq: u64) -> Vec<StoredEvent> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.seq > seq)
            .cloned()
            .collect()
    }

    /// ADR-026：启动恢复——max(seq)。
    pub fn max_seq(&self) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .last()
            .map(|e| e.seq)
            .unwrap_or(0)
    }
}

/// 内存投影（模拟 alarms/派生表）。
#[derive(Default)]
pub struct MemProjections {
    pub alarms: Mutex<HashMap<aivx_events::AlarmId, ()>>,
    /// Projector 实际消费到的 seq 列表（无洞断言用，ADR-022 回归测试）。
    pub consumed_seqs: Mutex<Vec<u64>>,
}

impl MemProjections {
    pub fn record(&self, seq: u64) {
        self.consumed_seqs.lock().unwrap().push(seq);
    }

    /// 断言 Projector 收到的 seq 连续无洞（ADR-022 的机器强制）。
    pub fn assert_no_gaps(&self, from: u64) -> bool {
        let seqs = self.consumed_seqs.lock().unwrap();
        for (i, s) in seqs.iter().enumerate() {
            let expect = from + i as u64 + 1;
            if *s != expect {
                return false;
            }
        }
        true
    }
}
