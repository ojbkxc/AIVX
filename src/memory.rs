//! 内存事件库（P0 测试替身）。
//!
//! P0 用它验证 forwarder/DbWriter/Projector 的正确性（顺序 fan-out 无洞、
//! seq 单调、僵尸清扫），不引 SQLite——P1 把 `EventStore` trait 对到 SeaORM。

use std::collections::HashMap;
use std::sync::Mutex;

use aivx_events::Event;
use serde::Serialize;

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

/// 报警派生表行（/api/alarms items 的形状）。
#[derive(Debug, Clone, Serialize)]
pub struct AlarmRow {
    pub alarm_id: String,
    pub device_id: String,
    pub rule_id: String,
    /// 投影时刻墙钟（Unix 秒）——事件只有 mono_ns（单调钟），不可读；
    /// 内存链路投影延迟 ms 级，投影时刻≈报警时刻。
    pub raised_ts: i64,
    /// 轨迹标签（MotionStub 恒 motion；YOLO 后是有语义的 label）。
    pub label: Option<String>,
    pub score: Option<f32>,
    /// 清除时刻（Unix 秒）；活跃报警为 None。
    pub cleared_ts: Option<i64>,
    /// 清除原因（track_lost / zone_left / cooldown / stale_on_boot）。
    pub cleared_reason: Option<String>,
}

/// 录像段派生表行（/api/recordings 的形状）。
#[derive(Debug, Clone, Serialize)]
pub struct RecordingRow {
    pub id: String,
    pub device_id: String,
    pub file_path: String,
    pub start_ts: i64,
    pub duration_secs: f64,
}

/// 内存投影（模拟 alarms/recordings 派生表）。
#[derive(Default)]
pub struct MemProjections {
    /// 活跃报警集（I8 状态机跳变时写；zombie sweep 用）。
    pub alarms: Mutex<HashMap<aivx_events::AlarmId, ()>>,
    /// 报警派生表（含已清除历史，供 /api/alarms items 查询）。
    pub alarm_rows: Mutex<Vec<AlarmRow>>,
    /// 录像段派生表（RecordingSegment 投影）。
    pub recordings: Mutex<Vec<RecordingRow>>,
    /// Projector 实际消费到的 seq 列表（无洞断言用，ADR-022 回归测试）。
    pub consumed_seqs: Mutex<Vec<u64>>,
}

impl MemProjections {
    pub fn record(&self, seq: u64) {
        self.consumed_seqs.lock().unwrap().push(seq);
    }

    /// 已投影事件数（healthz 暴露用）。
    pub fn projected_count(&self) -> usize {
        self.consumed_seqs.lock().unwrap().len()
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

    /// 最近报警（倒序，/api/alarms items 用）。
    pub fn recent_alarms(&self, limit: usize) -> Vec<AlarmRow> {
        let mut rows = self.alarm_rows.lock().unwrap().clone();
        rows.sort_by(|a, b| b.raised_ts.cmp(&a.raised_ts).then(b.alarm_id.cmp(&a.alarm_id)));
        rows.truncate(limit);
        rows
    }

    /// 设备录像段（start_ts 升序）。duration 由 API 层差分补（段固定 600s）。
    pub fn recordings_of(&self, device_id: &str) -> Vec<RecordingRow> {
        let rows = self.recordings.lock().unwrap();
        let mut out: Vec<RecordingRow> = rows
            .iter()
            .filter(|r| r.device_id == device_id)
            .cloned()
            .collect();
        out.sort_by_key(|r| r.start_ts);
        out
    }
}
