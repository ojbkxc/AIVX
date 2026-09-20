//! Agent 工具真实数据源（P8e）：把 tools.rs 的查询桥到运行时真数据
//! ——CameraManager（设备/在线状态）+ MemProjections（报警/录像派生表）。
//!
//! 之前 ActionContext::default() 用 MemDataSource 空桩；HTTP 接线后用本源，
//! Agent 的"自然语言查报警/查设备/查录像"就是真数据了（I6：派生表只读）。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::cameras::CameraManager;
use crate::memory::MemProjections;
use aivx_perception::stream;

use super::data::{AgentDataSource, AlarmEntry, DeviceEntry, RecordingEntry};

/// 运行时数据源：设备表 + 投影派生表（只读，I2/I6 无侵犯）。
pub struct LiveDataSource {
    cameras: Arc<CameraManager>,
    projections: Arc<MemProjections>,
}

impl LiveDataSource {
    pub fn new(cameras: Arc<CameraManager>, projections: Arc<MemProjections>) -> Self {
        Self {
            cameras,
            projections,
        }
    }
}

impl AgentDataSource for LiveDataSource {
    fn list_devices(&self) -> Vec<DeviceEntry> {
        self.cameras
            .cameras
            .iter()
            .map(|c| DeviceEntry {
                id: c.device.id.clone(),
                name: c.device.name.clone(),
                status: match c.bridge.metrics.stream_state.load(Ordering::Relaxed) {
                    stream::state::OK => "online".into(),
                    stream::state::STOPPED => "offline".into(),
                    _ => "connecting".into(),
                },
                access_type: format!("{:?}", c.device.access_type).to_lowercase(),
                recording: c.record_mode != "off",
                ptz_supported: c.device.capabilities.ptz,
            })
            .collect()
    }

    fn list_alarms(&self, limit: usize) -> Vec<AlarmEntry> {
        self.projections
            .recent_alarms(limit)
            .into_iter()
            .map(|r| AlarmEntry {
                id: r.alarm_id.clone(),
                device_id: r.device_id.clone(),
                event_type: r.rule_id.clone(),
                description: format!(
                    "{}（{}）",
                    r.label.as_deref().unwrap_or("目标"),
                    if r.cleared_ts.is_some() {
                        "已清除"
                    } else {
                        "活跃"
                    }
                ),
                ts: r.raised_ts,
            })
            .collect()
    }

    fn search_recording(&self, device_id: &str, start_ts: i64, end_ts: i64) -> Vec<RecordingEntry> {
        self.projections
            .recordings_of(device_id)
            .into_iter()
            .filter(|r| r.start_ts >= start_ts && r.start_ts <= end_ts)
            .map(|r| RecordingEntry {
                id: r.id,
                device_id: r.device_id,
                file_path: r.file_path,
                start_ts: r.start_ts,
                duration_secs: r.duration_secs,
            })
            .collect()
    }

    fn diagnostics(&self) -> String {
        let streams: Vec<String> = self
            .cameras
            .cameras
            .iter()
            .map(|c| {
                let m = &c.bridge.metrics;
                format!(
                    "- {}（{}）：state={} decode_frames={} inferences={}",
                    c.device.name,
                    c.device.id,
                    stream::state::name(m.stream_state.load(Ordering::Relaxed)),
                    m.decode_frames.load(Ordering::Relaxed),
                    m.inferences.load(Ordering::Relaxed),
                )
            })
            .collect();
        format!(
            "流体检：\n{}\n已投影事件：{}",
            streams.join("\n"),
            self.projections.projected_count()
        )
    }
}
