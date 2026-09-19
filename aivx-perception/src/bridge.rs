//! PlaneBridge —— 双平面间的唯一桥（DESIGN.md §1.3 / §5.4，ADR-020/022）。
//!
//! 数据面（T1/T2/T3 同步线程）→ 控制面（tokio）的全部出口：
//! 1. `events`: std::sync_channel —— T2 `try_send`，满则按 I12 分级处置
//! 2. `metrics`: AtomicU64 集 —— 无锁指标
//! 3. `control`: AtomicBool 集 —— stop/重载意图
//!
//! I12 背压：Critical 事件满队列时写 spill 文件（append 一行 JSON）；
//! 恢复 task 在 channel 排空后回灌。Info/Debug 丢弃 + 计数。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Arc;

use aivx_events::{Event, Grade};

/// 每路摄像头指标（控制面采集 task 每秒读一次）。
#[derive(Default)]
pub struct CameraMetrics {
    pub decode_frames: AtomicU64,
    pub analyze_frames: AtomicU64,
    /// 推理次数。
    pub inferences: AtomicU64,
    /// 丢帧数（gen 差值累计）。
    pub dropped_gen: AtomicU64,
    /// 被背压丢弃的事件数（Info/Debug）。
    pub dropped_info_events: AtomicU64,
    /// spill 的 Critical 事件数。
    pub spilled_critical: AtomicU64,
    /// 最近一次报警的单调钟纳秒（延迟测量的终点对照）。
    pub last_alarm_mono_ns: AtomicU64,
    /// 运动检测累计耗时 ns。
    pub motion_ns_total: AtomicU64,
    /// 推理累计耗时 ns。
    pub infer_ns_total: AtomicU64,
}

/// 控制位（控制面写意图，数据面轮询）。
#[derive(Default)]
pub struct ControlFlags {
    pub stop: AtomicBool,
    /// 暂停分析（录像继续）。
    pub pause_analysis: AtomicBool,
}

/// 平面桥（每路一个实例；控制面持有 Receiver 聚合进 forwarder）。
pub struct PlaneBridge {
    tx: SyncSender<Event>,
    pub metrics: Arc<CameraMetrics>,
    pub control: Arc<ControlFlags>,
    /// spill 目录（Critical 满队列兜底）。None = 测试模式不落盘。
    spill_dir: Option<PathBuf>,
}

impl PlaneBridge {
    /// 生产构造：`bound` 为 channel 容量（建议 1024）。
    pub fn new(bound: usize, spill_dir: Option<PathBuf>) -> (Self, Receiver<Event>) {
        let (tx, rx) = std::sync::mpsc::sync_channel(bound);
        let bridge = Self {
            tx,
            metrics: Arc::new(CameraMetrics::default()),
            control: Arc::new(ControlFlags::default()),
            spill_dir,
        };
        (bridge, rx)
    }

    /// T2 热路径出口（I2/I12）：永不阻塞，分级处置。
    pub fn emit(&self, ev: Event) {
        match self.tx.try_send(ev) {
            Ok(()) => {}
            Err(TrySendError::Full(critical)) => self.handle_full(critical),
            Err(TrySendError::Disconnected(_)) => {
                // 控制面已关停——丢弃即可（关停序列由 forwarder drain 保证）
            }
        }
    }

    fn handle_full(&self, ev: Event) {
        match ev.grade() {
            Grade::Critical => {
                self.metrics.spilled_critical.fetch_add(1, Ordering::Relaxed);
                if let Some(dir) = &self.spill_dir {
                    // serde_json 仅 dev/此处用——events crate 自带 JSON 能力更干净：
                    // 走 aivx_events 的 serde 手写行格式，避免生产依赖 serde_json。
                    if let Ok(json) = event_to_json(&ev) {
                        let _ = std::fs::create_dir_all(dir);
                        let path = dir.join(format!(
                            "spill-{}-{}.json",
                            std::process::id(),
                            crate::mono_ns()
                        ));
                        let _ = std::fs::write(path, json.as_bytes());
                    }
                }
            }
            _ => {
                self.metrics
                    .dropped_info_events
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 便捷：记录最近报警时间（控制面算报警延迟）。
    pub fn mark_alarm(&self, mono_ns: u64) {
        self.metrics
            .last_alarm_mono_ns
            .store(mono_ns, Ordering::Release);
    }

    /// 仅测试：spill 回灌需要发送端。
    pub fn tx_for_test(&self) -> &SyncSender<Event> {
        &self.tx
    }
}

/// 事件 → 单行 JSON。serde_json 是最直接的方式，但 perception 保持零多余生产依赖
/// 的成本高于收益——直接依赖 serde_json（workspace 共享，无版本风险）。
fn event_to_json(ev: &Event) -> serde_json::Result<String> {
    serde_json::to_string(ev)
}

fn event_from_json(s: &str) -> serde_json::Result<Event> {
    serde_json::from_str(s)
}

/// spill 恢复：把目录里的 spill 文件按序回灌 channel（forwarder 在排空后调用）。
///
/// 返回回灌条数；回灌失败的文件保留（下轮再试）。
pub fn replay_spills(dir: &PathBuf, tx: &SyncSender<Event>) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    files.sort(); // 文件名含 pid+ns，按序回灌
    let mut replayed = 0;
    for path in files {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(ev) = event_from_json(&content) else {
            continue;
        };
        if tx.try_send(ev).is_ok() {
            replayed += 1;
            let _ = std::fs::remove_file(&path);
        } else {
            break; // 又满了——到此为止，剩余下轮
        }
    }
    replayed
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivx_events::AlarmId;
    use std::time::Duration;

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

    fn stream_down() -> Event {
        Event::StreamDown {
            device_id: "cam-1".into(),
            reason: "test".into(),
            mono_ns: 1,
        }
    }

    /// I12 核心断言：灌满 channel 后，Critical 经 spill 100% 可达，Info 计数丢弃。
    #[test]
    fn critical_never_lost_under_backpressure() {
        let dir = std::env::temp_dir().join(format!("aivx-spill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (bridge, rx) = PlaneBridge::new(4, Some(dir.clone()));

        // 灌满（容量 4）——前 4 条进队列
        for i in 0..4 {
            bridge.emit(stream_down()); // Info
        }
        // 现在 Info 满了：Info 被丢计数；Critical 全部走 spill
        for i in 0..10 {
            let ev = alarm(i);
            bridge.emit(ev.clone());
        }
        assert_eq!(bridge.metrics.dropped_info_events.load(Ordering::Relaxed), 0);
        assert_eq!(bridge.metrics.spilled_critical.load(Ordering::Relaxed), 10);
        // Info 没溢出过（前 4 条刚好填满）——再发一条验证丢弃
        bridge.emit(stream_down());
        assert_eq!(bridge.metrics.dropped_info_events.load(Ordering::Relaxed), 1);

        // 排空队列
        for _ in 0..4 {
            rx.try_recv().expect("前 4 条应在队列");
        }
        // 回灌 spill
        let replayed = replay_spills(&dir, &bridge.tx_for_test());
        assert_eq!(replayed, 10, "10 条 Critical 应全部回灌");
        // 全部可达
        let mut got = 0;
        while rx.try_recv().is_ok() {
            got += 1;
        }
        assert_eq!(got, 10, "Critical 100% 到达");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Grade 正确性：Alarm 是 Critical，StreamDown 是 Info。
    #[test]
    fn grade_classification() {
        assert_eq!(alarm(0).grade(), Grade::Critical);
        assert_eq!(stream_down().grade(), Grade::Info);
    }
}
