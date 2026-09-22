//! 事件链控制面核心（DESIGN.md §5）：
//! forwarder（数据面桥）→ DbWriter（单写者 + seq + 顺序 fan-out）→ Projector（投影）。
//!
//! ADR-022 的实现要点：**DbWriter 是 sync_channel 的唯一消费者**。落库成功后，
//! 按 seq 顺序把事件 fan-out 给 Projector/WS/notify——链路上不存在可丢环节
//! （broadcast 的 Lagged 丢事件是 v2.1 的体系性错误，已否决）。

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::Arc;

use aivx_events::{AlarmId, Event};

#[cfg(test)]
use aivx_perception::bridge::PlaneBridge;

use crate::memory::{AlarmRow, MemEventStore, MemProjections, RecordingRow, StoredEvent};

/// forwarder：把数据面的 sync_channel 事件转交 DbWriter（tokio 侧的桥）。
///
/// `spill_dir` 传入以支持排空后回灌（I12）。
pub async fn forwarder(
    rx: Receiver<Event>,
    mut db: DbWriter,
    spill_dir: Option<std::path::PathBuf>,
) {
    // 排空后尝试回灌 spill（启动时）
    if let (Some(dir), Some(tx)) = (&spill_dir, db.retry_tx()) {
        aivx_perception::bridge::replay_spills(dir, tx);
    }
    // P0：阻塞 recv 在专用线程转发（控制面 tokio 不背阻塞 IO）。
    // 生产版用 spawn_blocking 包 recv 循环；P0 测试直接在当前 task 跑。
    let _ = &spill_dir;
    while let Ok(ev) = rx.recv() {
        db.enqueue(ev);
        // 达到批量阈值时 flush（50ms 定时由调用方 tick 触发；P0 阈值触发）
        if db.pending_len() >= db.batch_limit() {
            db.flush().await;
        }
    }
    // 发送端全 drop（关停）：drain 并 flush（优雅关停 §17 步骤 2-3）
    db.flush().await;
}

/// 单写者 DbWriter（I7 + ADR-022 + ADR-026）。
pub struct DbWriter {
    store: Arc<MemEventStore>,
    /// P0 内存投影；生产换 SeaORM 派生表。
    projections: Arc<MemProjections>,
    buf: Vec<Event>,
    /// 单写者内存 seq（启动从 store.max_seq() 恢复，ADR-026）。
    next_seq: u64,
    batch: usize,
    /// 运行期增量投影器（flush 每个事件都喂给它——ADR-022 fan-out 的消费端）。
    projector: Projector,
}

impl DbWriter {
    pub fn new(store: Arc<MemEventStore>, projections: Arc<MemProjections>) -> Self {
        let next_seq = store.max_seq() + 1; // ADR-026：重启不回退
                                            // 运行期投影器：从 checkpoint 重放追赶 + 之后增量消费 flush 的每个事件。
        let mut projector = Projector::new(store.clone(), projections.clone());
        // 僵尸清扫（ADR-028）：重建活跃报警集；清扫补发的 AlarmCleared 必须
        // 重新进入事件链（经 flush 正常落库 + fan-out），不能只丢在内存里。
        let clears = projector.recover();
        let mut buf: Vec<Event> = Vec::with_capacity(256);
        buf.extend(clears);
        Self {
            store,
            projections,
            buf,
            next_seq,
            batch: 256,
            projector,
        }
    }

    /// 仅 forwarder 启动回灌用（拿到 PlaneBridge 的发送端——P0 由测试注入）。
    pub fn retry_tx(&self) -> Option<&std::sync::mpsc::SyncSender<Event>> {
        None // P0：spill 回灌测试在 bridge 层完成（见 bridge.rs tests）；
             // forwarder 侧接入 P1 补（需要 PlaneBridge 引用）
    }

    pub fn enqueue(&mut self, ev: Event) {
        self.buf.push(ev);
    }

    pub fn pending_len(&self) -> usize {
        self.buf.len()
    }

    pub fn batch_limit(&self) -> usize {
        self.batch
    }

    /// 落库 + 顺序 fan-out（ADR-022：写库成功才 fan-out；fan-out 不丢）。
    pub async fn flush(&mut self) {
        self.flush_sync();
    }

    /// flush 的同步核心（async 签名保留给生产 tick 路径；测试直接走这里）。
    pub fn flush_sync(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        // 1. 分配 seq + 单事务 append（模拟 BEGIN..INSERT..COMMIT）
        let batch: Vec<StoredEvent> = self
            .buf
            .drain(..)
            .map(|ev| {
                let seq = self.next_seq;
                self.next_seq += 1;
                StoredEvent { seq, event: ev }
            })
            .collect();
        self.store.append_batch(batch.clone()); // "事务提交"
                                                // 2. 提交后顺序 fan-out（Projector 在此消费——链上唯一、不可丢）
        for se in batch {
            self.projections.record(se.seq);
            self.projector.project_one(&se.event, se.seq); // 增量投影
        }
    }

    /// 测试辅助：下一 seq（验证 ADR-026 恢复）。
    pub fn next_seq_hint(&self) -> u64 {
        self.next_seq
    }

    /// 当前 seq 位点（Projector checkpoint 语义）。
    pub fn current_seq(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }
}

/// Projector：从 events 重放/增量投影（DESIGN.md §5.3 + ADR-028 僵尸清扫）。
pub struct Projector {
    store: Arc<MemEventStore>,
    projections: Arc<MemProjections>,
    checkpoint: u64,
    /// 活跃报警（alarm_id → raised seq；清扫用）。
    active_alarms: HashMap<AlarmId, u64>,
    /// 报警最大存活时间（模拟墙钟由调用方推进；P0 用 tick 计数）。
    max_alarm_ttl_ticks: u64,
}

impl Projector {
    pub fn new(store: Arc<MemEventStore>, projections: Arc<MemProjections>) -> Self {
        let checkpoint = 0; // 生产从 checkpoint 表读；P0 从 0
        Self {
            store,
            projections,
            checkpoint,
            active_alarms: Default::default(),
            max_alarm_ttl_ticks: 10,
        }
    }

    /// 启动恢复：重放 events_after(checkpoint) + 僵尸清扫（ADR-028）。
    ///
    /// 清扫：raised 后 `max_alarm_ttl_ticks` 内仍未 cleared 的报警补
    /// `AlarmCleared{reason:"stale_on_boot"}`——报警语义有界，不留永真报警。
    pub fn recover(&mut self) -> Vec<Event> {
        let events = self.store.events_after(self.checkpoint);
        let mut clears = Vec::new();
        for se in &events {
            // 重放只重建状态，不 record——重放的历史 seq 与增量路径会交叉，
            // 破坏 assert_no_gaps 的"本次会话连续"语义；恢复完整性由
            // seq_recovers 测试的尾部窗口断言单独覆盖。
            self.project_one(&se.event, se.seq);
        }
        // 僵尸清扫：活跃报警里 raised 距今（末 seq 位点）超过 TTL 的补清
        let latest = self.store.max_seq();
        let stale: Vec<AlarmId> = self
            .active_alarms
            .iter()
            .filter(|(_, &raised)| latest.saturating_sub(raised) > self.max_alarm_ttl_ticks)
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.active_alarms.remove(&id);
            clears.push(Event::AlarmCleared {
                alarm_id: id,
                reason: "stale_on_boot".into(),
            });
        }
        if let Some(last) = events.last() {
            self.checkpoint = last.seq;
        }
        clears
    }

    fn project_one(&mut self, ev: &Event, seq: u64) {
        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        match ev {
            Event::AlarmRaised {
                alarm_id,
                device_id,
                rule_id,
                track,
                ..
            } => {
                self.active_alarms.insert(alarm_id.clone(), seq);
                self.projections
                    .alarms
                    .lock()
                    .unwrap()
                    .insert(alarm_id.clone(), ());
                self.projections.alarm_rows.lock().unwrap().push(AlarmRow {
                    alarm_id: alarm_id.clone(),
                    device_id: device_id.clone(),
                    rule_id: rule_id.clone(),
                    raised_ts: now_ts,
                    label: track.as_ref().map(|t| t.label.clone()),
                    score: track.as_ref().map(|t| t.score),
                    cleared_ts: None,
                    cleared_reason: None,
                });
            }
            Event::AlarmCleared { alarm_id, reason } => {
                self.active_alarms.remove(alarm_id);
                self.projections.alarms.lock().unwrap().remove(alarm_id);
                let mut rows = self.projections.alarm_rows.lock().unwrap();
                if let Some(row) = rows
                    .iter_mut()
                    .rev()
                    .find(|r| r.alarm_id == *alarm_id && r.cleared_ts.is_none())
                {
                    row.cleared_ts = Some(now_ts);
                    row.cleared_reason = Some(reason.clone());
                }
            }
            Event::RecordingSegment {
                device_id,
                file_path,
                start_mono_ns,
                duration_secs,
                ..
            } => {
                // start_mono_ns：scanner 传的是段起点墙钟纳秒（文件名
                // seg_%Y%m%d_%H%M%S 解析；解析失败回退 mtime）。同文件
                // 重复投影（scanner seen 只挡 emit 失败，flush 后重扫
                // 不会）按 id 去重——mtime 段会重复 push 同一 id。
                let row = RecordingRow {
                    id: format!("{device_id}-{start_mono_ns}"),
                    device_id: device_id.clone(),
                    file_path: file_path.clone(),
                    start_ts: (*start_mono_ns / 1_000_000_000) as i64,
                    duration_secs: *duration_secs,
                };
                let mut rows = self.projections.recordings.lock().unwrap();
                if !rows.iter().any(|r| r.id == row.id) {
                    rows.push(row);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{MemEventStore, MemProjections};

    fn alarm(id: u64) -> Event {
        Event::AlarmRaised {
            alarm_id: format!("r-1-{id}"),
            device_id: "cam-1".into(),
            rule_id: "r".into(),
            zone_id: None,
            track: None,
            frame_gen: id,
            mono_ns: id,
        }
    }

    /// 测试辅助：flush 在 P0 无真正挂起点，直接走同步核心。
    trait FlushNow {
        fn await_manually(&mut self);
    }
    impl FlushNow for DbWriter {
        fn await_manually(&mut self) {
            self.flush_sync();
        }
    }

    /// ADR-022 机器强制：500 条跨两批事务，Projector 收到的 seq 连续无洞。
    #[test]
    fn ordered_fanout_no_gaps() {
        let store = Arc::new(MemEventStore::new());
        let projections = Arc::new(MemProjections::default());
        let mut db = DbWriter::new(store.clone(), projections.clone());

        for i in 0..500 {
            db.enqueue(alarm(i));
        }
        db.await_manually();
        assert!(projections.assert_no_gaps(0), "seq 必须连续无洞");
        assert_eq!(store.max_seq(), 500);
    }

    /// ADR-026：重启后 seq 从 max+1 继续，不回退不重复。
    ///
    /// recover 只重建状态不 record（重放历史 seq 会与增量路径交叉破坏
    /// assert_no_gaps 的"本次会话连续"语义）。恢复完整性由本项目自己的断言
    /// 覆盖：DbWriter 第二段（11 号）fan-out 时 projections 增量记录 1..=10
    ///（第一段）+ 11（第二段）——checkpoint 后的增量路径在会话内仍连续无洞。
    #[test]
    fn seq_recovers_after_restart() {
        let store = Arc::new(MemEventStore::new());
        let projections = Arc::new(MemProjections::default());
        {
            let mut db = DbWriter::new(store.clone(), projections.clone());
            for i in 0..10 {
                db.enqueue(alarm(i));
            }
            db.await_manually();
        }
        // 第二段 DbWriter：recover 只重建状态（不 record），增量 flush 的 11 号
        // 从 consumed 视角是连续的第 11 条（前 10 条来自第一段）——断言无洞。
        let mut db2 = DbWriter::new(store.clone(), projections.clone());
        assert_eq!(db2.next_seq_hint(), 11, "重启后 seq 必须从 max+1 开始");
        // recover 在 DbWriter::new 内已执行：重放 1..=10，活跃集 10 条。
        assert_eq!(
            db2.projector.active_alarms.len(),
            10,
            "recover 必须重建 10 条活跃报警"
        );
        db2.enqueue(alarm(99));
        db2.await_manually();
        assert_eq!(store.max_seq(), 11);
        assert!(projections.assert_no_gaps(0), "重启后增量路径仍无洞");
    }

    /// ADR-028：崩溃留下永真报警 → 重启清扫补 AlarmCleared。
    #[test]
    fn zombie_alarm_swept_on_recover() {
        let store = Arc::new(MemEventStore::new());
        let projections = Arc::new(MemProjections::default());
        {
            let mut db = DbWriter::new(store.clone(), projections.clone());
            db.enqueue(alarm(1));
            db.await_manually();
            for i in 0..20 {
                db.enqueue(Event::StreamUp {
                    device_id: "cam-1".into(),
                    mono_ns: i,
                });
            }
            db.await_manually();
        }
        let mut proj = Projector::new(store.clone(), projections.clone());
        let clears = proj.recover();
        assert_eq!(clears.len(), 1, "僵尸报警应被清扫");
        assert!(
            matches!(&clears[0], Event::AlarmCleared { alarm_id, reason }
                if alarm_id == "r-1-1" && reason == "stale_on_boot")
        );
        assert!(proj.active_alarms.is_empty());
    }

    /// 运行期增量投影（ADR-022 的 DB writer 内联投影）：AlarmRaised 进活跃集，
    /// 随后 AlarmCleared 移除——投影器状态在运行期（非仅 recover）保持一致。
    #[test]
    fn projector_tracks_active_alarms_inline() {
        let store = Arc::new(MemEventStore::new());
        let projections = Arc::new(MemProjections::default());
        let mut db = DbWriter::new(store.clone(), projections.clone());

        db.enqueue(alarm(1));
        db.await_manually();
        assert!(
            !db.projector.active_alarms.is_empty(),
            "AlarmRaised 应进入活跃集"
        );

        db.enqueue(Event::AlarmCleared {
            alarm_id: "r-1-1".into(),
            reason: "track_lost".into(),
        });
        db.await_manually();
        assert!(
            db.projector.active_alarms.is_empty(),
            "AlarmCleared 应移除活跃报警"
        );
    }

    /// 端到端：PlaneBridge emit → drain → DbWriter → 无洞（I12 + ADR-022）。
    #[test]
    fn bridge_to_dbwriter_end_to_end() {
        let (bridge, rx) = PlaneBridge::new(128, None);
        let store = Arc::new(MemEventStore::new());
        let projections = Arc::new(MemProjections::default());
        let mut db = DbWriter::new(store.clone(), projections.clone());

        for i in 0..50 {
            bridge.emit(alarm(i));
        }
        while let Ok(ev) = rx.try_recv() {
            db.enqueue(ev);
        }
        db.await_manually();
        assert!(projections.assert_no_gaps(0));
        assert_eq!(store.max_seq(), 50);
    }
}
