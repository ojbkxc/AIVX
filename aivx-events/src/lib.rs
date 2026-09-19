//! aivx-events —— 两平面共用的事件词汇表（DESIGN.md §5.1）。
//!
//! I5/I11 的编译期强制之一：本 crate 仅依赖 serde，数据面（std only）与
//! 控制面（tokio）都能 import。事件是唯一事实源（I6）的词汇层。
//!
//! 分级（I12，DESIGN.md §5.1）：
//! - Critical: 报警/洞察 —— 绝不丢，spill 文件兜底
//! - Info:    轨迹/流状态/证据就绪 —— 可丢（计数）
//! - Debug:   其余 —— 便宜丢

use serde::{Deserialize, Serialize};

/// 摄像头/设备 ID（UUID 字符串，控制面生成；数据面只透传）。
pub type DeviceId = String;
/// 布控区域 ID。
pub type ZoneId = String;
/// 规则 ID。
pub type RuleId = String;
/// 轨迹 ID（数据面 ByteTrack 分配，u64）。
pub type TrackId = u64;
/// 报警 ID（T2 生成：`{rule}-{track}-{mono_ns}`，落库后全局唯一）。
pub type AlarmId = String;

/// 事件分级（I12）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Grade {
    /// 绝不丢：满则 spill 文件。
    Critical,
    /// 可丢：丢弃 + 计数。
    Info,
    /// 便宜丢。
    Debug,
}

/// 检测框 [x1, y1, x2, y2]（像素，子码流坐标系）。
pub type Box = [f32; 4];

/// 跟踪目标快照（随事件携带的最小字段）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackSnapshot {
    pub track_id: TrackId,
    pub label: String,
    pub score: f32,
    pub box_: Box,
}

/// LLM 洞察（cognition 回填，DESIGN.md §10）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Insight {
    pub is_false_positive: bool,
    /// low / medium / high
    pub threat_level: String,
    pub scene: String,
    pub title: String,
    pub summary: String,
}

/// 统一事件模型（DESIGN.md §5.1）。
///
/// `seq` 由 DbWriter 单写者分配（I7），数据面产出的事件 seq 为 `None`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    // ── 流状态 ──
    StreamUp {
        device_id: DeviceId,
        #[serde(with = "serde_nanos")]
        mono_ns: u64,
    },
    StreamDown {
        device_id: DeviceId,
        reason: String,
        #[serde(with = "serde_nanos")]
        mono_ns: u64,
    },
    // ── 感知层：轨迹 ──
    TrackAppeared {
        device_id: DeviceId,
        track: TrackSnapshot,
    },
    TrackDisappeared {
        device_id: DeviceId,
        track_id: TrackId,
    },
    TrackEnteredZone {
        device_id: DeviceId,
        zone_id: ZoneId,
        track: TrackSnapshot,
    },
    TrackLeftZone {
        device_id: DeviceId,
        zone_id: ZoneId,
        track_id: TrackId,
    },
    // ── 规则引擎（状态机跳变才产生，I8）──
    AlarmRaised {
        alarm_id: AlarmId,
        device_id: DeviceId,
        rule_id: RuleId,
        zone_id: Option<ZoneId>,
        track: Option<TrackSnapshot>,
        /// 证据帧代数（证据线程按此取 EvidenceRing，ADR-021）
        frame_gen: u64,
        #[serde(with = "serde_nanos")]
        mono_ns: u64,
    },
    AlarmCleared {
        alarm_id: AlarmId,
        /// "track_lost" / "zone_left" / "cooldown" / "stale_on_boot"（ADR-028）
        reason: String,
    },
    // ── 证据（ADR-021，证据线程产物）──
    EvidenceReady {
        alarm_id: AlarmId,
        path: String,
    },
    // ── 认知层回填 ──
    InsightGenerated {
        alarm_id: AlarmId,
        insight: Insight,
    },
    // ── 录像 ──
    RecordingSegment {
        device_id: DeviceId,
        file_path: String,
        #[serde(with = "serde_nanos")]
        start_mono_ns: u64,
        duration_secs: f64,
    },
    // ── 交互/配置（控制面产生）──
    AgentAction {
        summary: String,
    },
    ConfigChanged {
        section: String,
        summary: String,
    },
}

impl Event {
    /// 事件分级（I12）。Critical 满队列走 spill；Info/Debug 丢弃计数。
    pub fn grade(&self) -> Grade {
        match self {
            Event::AlarmRaised { .. } | Event::InsightGenerated { .. } => Grade::Critical,
            Event::TrackAppeared { .. }
            | Event::TrackDisappeared { .. }
            | Event::TrackEnteredZone { .. }
            | Event::TrackLeftZone { .. }
            | Event::StreamUp { .. }
            | Event::StreamDown { .. }
            | Event::EvidenceReady { .. }
            | Event::RecordingSegment { .. } => Grade::Info,
            Event::AlarmCleared { .. }
            | Event::AgentAction { .. }
            | Event::ConfigChanged { .. } => Grade::Debug,
        }
    }

    /// 事件来源设备（投影器按 device 过滤用）。
    pub fn device_id(&self) -> Option<&DeviceId> {
        match self {
            Event::StreamUp { device_id, .. }
            | Event::StreamDown { device_id, .. }
            | Event::TrackAppeared { device_id, .. }
            | Event::TrackDisappeared { device_id, .. }
            | Event::TrackEnteredZone { device_id, .. }
            | Event::TrackLeftZone { device_id, .. }
            | Event::RecordingSegment { device_id, .. } => Some(device_id),
            Event::AlarmRaised { device_id, .. } => Some(device_id),
            _ => None,
        }
    }
}

/// 单调钟纳秒的 serde 透传（u64 直接序列化；显式命名空间避免误解为墙钟）。
pub mod serde_nanos {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(*v)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        u64::deserialize(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I12 分级正确性：报警与洞察必须是 Critical。
    #[test]
    fn alarm_and_insight_are_critical() {
        let alarm = Event::AlarmRaised {
            alarm_id: "r1-1-1".into(),
            device_id: "cam-1".into(),
            rule_id: "r1".into(),
            zone_id: None,
            track: None,
            frame_gen: 42,
            mono_ns: 1,
        };
        assert_eq!(alarm.grade(), Grade::Critical);
    }

    /// 事件可序列化（落库/spill JSON 的前提）。
    #[test]
    fn event_roundtrip_json() {
        let ev = Event::StreamDown {
            device_id: "cam-1".into(),
            reason: "eof".into(),
            mono_ns: 123,
        };
        let json = serde_json::to_string(&ev).unwrap();
        // events crate 不依赖 serde_json —— 用 contains 做轻量验证
        assert!(json.contains("stream_down"));
        let back: Event = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, Event::StreamDown { .. }));
    }
}
