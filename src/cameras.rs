//! P8a：YAML 设备清单解析（兼容 Frigate 配置习惯）+ CameraManager 编排。
//!
//! 参照：用户的 Frigate 生产配置（TP-LINK NVR ×2，各 2 通道）：
//! ```yaml
//! cameras:
//!   tp_1-1:
//!     enabled: true
//!     ffmpeg:
//!       inputs:
//!         - path: rtsp://admin:pass@192.168.31.201:554/stream1&channel=1
//!           roles: [detect]
//!     detect: { enabled: true, width: 1280, height: 720 }
//!     record: { enabled: false }
//!     snapshots: { enabled: true, retain: { default: 10 } }
//! ```
//!
//! AIVX 语义映射（DESIGN.md §1.3 双平面）：
//! - `detect.enabled` + roles 含 `detect` → 拉起 T1+T2 线程束
//! - `record.enabled` → T3 录像（`mode: always`；`motion: {days}` → motion 模式，
//!   运动门控联动启停——对齐用户"移动侦测录像保留 7 天"的真实需求）
//! - 凭据内嵌 RTSP URL（TP-LINK `&channel=` 语法原样兼容——P8 不做 ONVIF 改写）
//!
//! I5：编排是控制面模块；它 spawn 的是 perception 的同步 OS 线程（std::thread），
//! 不把 tokio 传进数据面。

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::Receiver;

use aivx_events::{DeviceId, Event};
use aivx_perception::analyze::MotionStubAnalyzer;
use aivx_perception::bridge::PlaneBridge;
use aivx_perception::frame::LatestFrameSlot;
use aivx_perception::record::{record_loop, RecordCfg};
use aivx_perception::stream::DecodeCfg;
use aivx_net::{AccessType, Capabilities, Device};
use serde::Deserialize;

/// ── YAML 模型（Frigate 字段习惯）──────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AivxYaml {
    #[serde(default)]
    pub cameras: HashMap<String, CameraYaml>,
}

#[derive(Debug, Deserialize)]
pub struct CameraYaml {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub ffmpeg: FfmpegYaml,
    pub detect: DetectYaml,
    #[serde(default)]
    pub record: RecordYaml,
    #[serde(default)]
    pub snapshots: SnapshotsYaml,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct FfmpegYaml {
    pub inputs: Vec<InputYaml>,
}

#[derive(Debug, Deserialize)]
pub struct InputYaml {
    pub path: String,
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct DetectYaml {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_w")]
    pub width: usize,
    #[serde(default = "default_h")]
    pub height: usize,
}

fn default_w() -> usize {
    1280
}
fn default_h() -> usize {
    720
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordYaml {
    pub enabled: bool,
    /// `motion: {days: 7}` → motion 触发录像（保留天数）。
    pub motion: Option<RecordMotionYaml>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct RecordMotionYaml {
    pub days: u32,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct SnapshotsYaml {
    #[serde(default)]
    pub enabled: bool,
    /// retain.default / retain.objects——P8a 只存语义（快照保留策略随证据链路落）。
    #[serde(default)]
    pub retain: Option<serde_yaml::Value>,
}

impl RecordYaml {
    /// 录像模式：off / always / motion（对齐用户配置语义）。
    pub fn mode(&self) -> &'static str {
        if !self.enabled {
            "off"
        } else if self.motion.is_some() {
            "motion"
        } else {
            "always"
        }
    }
}

/// ── 运行时编排 ────────────────────────────────────────────────

/// 一路已启动的摄像头线程束句柄。
pub struct CameraHandle {
    pub device: Device,
    pub bridge: Arc<PlaneBridge>,
    /// 录像模式：off / always / motion（对齐用户 Frigate 配置语义）。
    pub record_mode: &'static str,
}

/// CameraManager：按 YAML 拉起每路 T1+T2 线程束，持有句柄供查询/关停。
pub struct CameraManager {
    pub cameras: Vec<CameraHandle>,
    /// 事件出口（forwarder 消费端）。tx 存活期间 forwarder 常驻 drain。
    _event_tx: std::sync::mpsc::SyncSender<Event>,
    /// 全局汇聚 rx（一次性交接给 forwarder）。
    event_rx: std::sync::Mutex<Option<Receiver<Event>>>,
}

impl CameraManager {
    /// 解析 YAML（不存在 → 空管理器，服务仍常驻——P9b 语义保留）。
    pub fn load_yaml(path: &Path, record_dir: PathBuf) -> anyhow::Result<Self> {
        let yaml: AivxYaml = if path.exists() {
            let raw = std::fs::read_to_string(path)?;
            serde_yaml::from_str(&raw)?
        } else {
            AivxYaml::default()
        };
        Self::from_yaml_with_record(&yaml, record_dir)
    }

    /// 启用中的摄像头各起一个线程束（T1 拉流 + T2 分析 + T3 按需录像）+ 事件汇聚。
    ///
    /// P8a 分析器用 MotionStubAnalyzer（运动即报，驱动事件链端到端验证）；
    /// ort YOLO 后端随 P8b 由配置注入。
    pub fn from_yaml_with_record(yaml: &AivxYaml, record_dir: PathBuf) -> anyhow::Result<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Event>(1024);
        let mut cameras = Vec::new();

        for (name, cam) in &yaml.cameras {
            if !cam.enabled || !cam.detect.enabled {
                continue;
            }
            // roles 含 detect 的第一个输入为分析子码流（I4：分析/录像分路在 P8b 接 T3）
            let Some(input) = cam
                .ffmpeg
                .inputs
                .iter()
                .find(|i| i.roles.iter().any(|r| r == "detect"))
            else {
                continue;
            };
            let device_id: DeviceId = name.clone();
            let (bridge, bridge_rx) = PlaneBridge::new(1024, None);
            let bridge = Arc::new(bridge);
            let slot = Arc::new(LatestFrameSlot::new(cam.detect.width, cam.detect.height));

            // 事件汇聚线程：bridge_rx → 全局 tx（单路内 FIFO 保序；转投 try_send
            // 不阻塞数据面——桥线程阻塞 recv 无害，它不是热路径）
            {
                let tx = tx.clone();
                let bridge = bridge.clone();
                std::thread::Builder::new()
                    .name(format!("cam-{name}-bridge-forward"))
                    .spawn(move || {
                        for ev in bridge_rx {
                            // 汇聚口满 = 控制面消费慢。分级语义在 bridge.emit 已做，
                            // 这里保持 Critical 不丢：同步 send（桥线程可等，数据面不等）。
                            if tx.send(ev).is_err() {
                                break; // 控制面已关停
                            }
                        }
                        let _ = bridge; // 保有 metric 可读性
                    })?;
            }

            // T1 拉流线程（std::thread——数据面，ADR-019）
            let cfg = DecodeCfg {
                device_id: device_id.clone(),
                rtsp_url: input.path.clone(),
                width: cam.detect.width,
                height: cam.detect.height,
                ffmpeg: "ffmpeg".into(),
                input_args: Vec::new(),
            };
            {
                let bridge = bridge.clone();
                let slot = slot.clone();
                std::thread::Builder::new()
                    .name(format!("cam-{name}-t1-decode"))
                    .spawn(move || {
                        aivx_perception::stream::decode_loop(cfg, (*slot).clone(), bridge)
                    })?;
            }
            // T2 分析线程
            {
                let bridge = bridge.clone();
                let slot = slot.clone();
                let device_id = device_id.clone();
                std::thread::Builder::new()
                    .name(format!("cam-{name}-t2-analyze"))
                    .spawn(move || {
                        aivx_perception::analyze::analysis_loop(
                            device_id,
                            slot,
                            bridge,
                            MotionStubAnalyzer,
                        )
                    })?;
            }

            // T3 录像线程（I4 物理隔离；mode=off 不拉起——对齐用户配置：
            // 固定相机不录像、移动相机 always 录。motion 联动启停随 P8b 接
            // 运动门控信号——当前 motion 语义先按 always 落（宁多录不漏录）。
            let record_mode = cam.record.mode();
            if record_mode != "off" {
                let rcfg = RecordCfg {
                    device_id: device_id.clone(),
                    rtsp_url: input.path.clone(),
                    base_dir: record_dir.clone(),
                    segment_secs: 600,
                    ffmpeg: "ffmpeg".into(),
                };
                let bridge = bridge.clone();
                std::thread::Builder::new()
                    .name(format!("cam-{name}-t3-record"))
                    .spawn(move || record_loop(rcfg, bridge))?;
            }

            cameras.push(CameraHandle {
                device: Device {
                    id: name.clone(),
                    name: name.clone(),
                    access_type: AccessType::Rtsp,
                    onvif_url: None,
                    rtsp_main: Some(input.path.clone()),
                    rtsp_sub: Some(input.path.clone()),
                    manufacturer: Some("TP-LINK".into()),
                    model: None,
                    capabilities: Capabilities {
                        main_sub_streams: true,
                        ..Default::default()
                    },
                },
                bridge,
                record_mode: cam.record.mode(),
            });
        }

        let _ = rx; // rx 返回给调用方（main 转交 forwarder）——见 event_rx()
        let manager = Self {
            cameras,
            _event_tx: tx.clone(),
        };
        manager.event_rx.set(rx).ok(); // 首次 set 成功；重复 load 报错忽略
        Ok(manager)
    }

    /// 事件流接收端（main 转交 forwarder——全局唯一上游）。
    pub fn event_rx(&self) -> Receiver<Event> {
        self.event_rx
            .take()
            .expect("event_rx 已被取走（CameraManager 只能被消费一次）")
    }

    /// 设备列表（API /api/devices 数据源——P9b 空列表到此终结）。
    pub fn devices(&self) -> Vec<Device> {
        self.cameras.iter().map(|c| c.device.clone()).collect()
    }
}
