//! Agent 工具真实数据源（DESIGN.md §11 / P8）——接投影器 + 设备表。
//!
//! 之前 `exec_tool` 返回桩文本，现在接真实 handler：
//! - 设备查询：读 `DeviceStore`（设备表，含在线状态/码流/能力 I10）
//! - 报警查询：读 `AlarmProjection`（由事件流 Projector 维护的报警派生表）
//! - 录像查询：读 `RecordingProjection`
//!
//! 数据源是可注入的 trait（测试用内存桩，生产用 SeaORM 投影器）——
//! 保持 agent 模块不依赖具体存储实现（I5）。

use std::sync::Arc;

/// 设备条目（工具返回的摘要）。
#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub id: String,
    pub name: String,
    pub status: String, // online / offline
    pub access_type: String,
    pub recording: bool,
    pub ptz_supported: bool,
}

/// 报警条目。
#[derive(Debug, Clone)]
pub struct AlarmEntry {
    pub id: String,
    pub device_id: String,
    pub event_type: String,
    pub description: String,
    pub ts: i64,
}

/// 录像片段。
#[derive(Debug, Clone)]
pub struct RecordingEntry {
    pub id: String,
    pub device_id: String,
    pub file_path: String,
    pub start_ts: i64,
    pub duration_secs: f64,
}

/// Agent 工具数据源（可注入 trait——测试桩 + 生产投影器）。
pub trait AgentDataSource: Send + Sync {
    fn list_devices(&self) -> Vec<DeviceEntry>;
    fn list_alarms(&self, limit: usize) -> Vec<AlarmEntry>;
    fn search_recording(&self, device_id: &str, start_ts: i64, end_ts: i64) -> Vec<RecordingEntry>;
    fn diagnostics(&self) -> String;
}

/// 内存数据源（测试桩 + P8 默认——生产换 SeaORM 投影器）。
#[derive(Default)]
pub struct MemDataSource {
    devices: Vec<DeviceEntry>,
    alarms: Vec<AlarmEntry>,
    recordings: Vec<RecordingEntry>,
}

impl MemDataSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_device(mut self, d: DeviceEntry) -> Self {
        self.devices.push(d);
        self
    }

    pub fn with_alarm(mut self, a: AlarmEntry) -> Self {
        self.alarms.push(a);
        self
    }
}

impl AgentDataSource for MemDataSource {
    fn list_devices(&self) -> Vec<DeviceEntry> {
        self.devices.clone()
    }

    fn list_alarms(&self, limit: usize) -> Vec<AlarmEntry> {
        self.alarms.iter().take(limit).cloned().collect()
    }

    fn search_recording(&self, device_id: &str, start_ts: i64, end_ts: i64) -> Vec<RecordingEntry> {
        self.recordings
            .iter()
            .filter(|r| r.device_id == device_id && r.start_ts >= start_ts && r.start_ts <= end_ts)
            .cloned()
            .collect()
    }

    fn diagnostics(&self) -> String {
        if self.devices.is_empty() {
            "暂无设备".into()
        } else {
            format!(
                "设备 {} 台：{} 在线 / {} 离线，录像 {} 台",
                self.devices.len(),
                self.devices.iter().filter(|d| d.status == "online").count(),
                self.devices
                    .iter()
                    .filter(|d| d.status == "offline")
                    .count(),
                self.devices.iter().filter(|d| d.recording).count(),
            )
        }
    }
}

/// 共享数据源包装（Arc 传递，跨 runner 调用）。
pub type SharedDataSource = Arc<dyn AgentDataSource>;

/// 测试辅助：构造一个带数据的源。
#[cfg(test)]
pub fn test_source() -> SharedDataSource {
    Arc::new(
        MemDataSource::new()
            .with_device(DeviceEntry {
                id: "cam-1".into(),
                name: "后院".into(),
                status: "online".into(),
                access_type: "onvif".into(),
                recording: true,
                ptz_supported: true,
            })
            .with_device(DeviceEntry {
                id: "cam-2".into(),
                name: "前门".into(),
                status: "offline".into(),
                access_type: "rtsp".into(),
                recording: false,
                ptz_supported: false,
            })
            .with_alarm(AlarmEntry {
                id: "alarm-1".into(),
                device_id: "cam-1".into(),
                event_type: "entered_zone".into(),
                description: "检测到人员进入后院".into(),
                ts: 1_700_000_000_000,
            }),
    )
}
