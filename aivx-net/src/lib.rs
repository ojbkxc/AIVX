//! aivx-net —— 协议层（DESIGN.md §12）。
//!
//! I10 核心原则（参照 open-nvr camera_drivers/base.py）：
//! - **"不支持"是数据不是异常**：读方法返回 `Supported`，只有 auth/transport 才是 `Err`
//! - **危险操作结构不可达**：trait 里不存在 set_ip / factory_reset——
//!   调用不存在的方法即不可能砖机
//!
//! P0 骨架：trait + 能力模型 + 设备候选。ONVIF 实现随 P1 落地。

use serde::{Deserialize, Serialize};

/// 设备/摄像头（协议层视角的领域模型）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub access_type: AccessType,
    /// ONVIF 服务地址（http://ip:port/onvif/device_service）。
    pub onvif_url: Option<String>,
    /// 主码流（录像/监看，I4）。
    pub rtsp_main: Option<String>,
    /// 子码流（分析，I4）。
    pub rtsp_sub: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub capabilities: Capabilities,
}

/// 接入方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessType {
    Onvif,
    Rtsp,
    Gb28181,
}

/// 设备能力（I10：前端按此动态渲染 UI）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub ptz: bool,
    pub event_subscription: bool,
    pub imaging: bool,
    pub audio: bool,
    pub main_sub_streams: bool,
}

/// "不支持"是数据不是异常（I10）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supported<T> {
    Yes(T),
    No,
}

impl<T> Supported<T> {
    pub fn is_supported(&self) -> bool {
        matches!(self, Supported::Yes(_))
    }
}

/// 协议/传输错误（只有这类才是 Err——能力缺失不是错误）。
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("认证失败: {0}")]
    Auth(String),
    #[error("传输失败: {0}")]
    Transport(String),
    #[error("设备未找到: {0}")]
    NotFound(String),
}

/// PTZ 指令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtzCmd {
    Left,
    Right,
    Up,
    Down,
    ZoomIn,
    ZoomOut,
    Stop,
}

/// 发现到的设备候选（discover 输出）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCandidate {
    pub onvif_url: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub hardware_id: Option<String>,
    pub ip_hint: Option<String>,
}

/// 设备适配器（DESIGN.md §12；open-nvr CameraDriver 的 Rust 化）。
///
/// 实现顺序：OnvifAdapter（默认，跨品牌）→ 未来按需 feature-gate 厂商 SDK。
/// 注意：**没有** set_ip / factory_reset（结构不可达原则）。
pub trait DeviceAdapter: Send + Sync {
    /// 局域网发现（ONVIF WS-Discovery 多播）。
    fn discover(&self) -> Result<Vec<DeviceCandidate>, AdapterError>;

    /// 解析设备的流地址（GetProfiles → GetStreamUri）。主/子分离（I4）。
    fn get_streams(&self, device: &Device) -> Result<Device, AdapterError>;

    /// 探测设备能力（I10：返回数据，UI 动态渲染）。
    fn capabilities(&self, device: &Device) -> Result<Capabilities, AdapterError>;

    /// PTZ 控制。不支持返回 Supported::No（数据），支持则执行。
    fn ptz(&self, device: &Device, cmd: PtzCmd) -> Result<Supported<()>, AdapterError>;

    /// 抓拍一张。
    fn snapshot(&self, device: &Device) -> Result<Supported<Vec<u8>>, AdapterError>;
}

/// ONVIF 通用兜底驱动（P8：真实 SOAP 客户端）。
///
/// 实现基于 ONVIF Profile S 的核心 SOAP 调用：
/// - `capabilities`: GetCapabilities → 探测 ptz/events/imaging
/// - `get_streams`: GetProfiles → GetStreamUri（主/子码流分离，I4）
/// - `ptz`: ContinuousMove / Stop
/// - `snapshot`: GetSnapshotUri
///
/// 实际 SOAP 传输（HTTP POST + Digest 鉴权 + XML 构造）在 `onvif.rs`；
/// 本桩保留 trait 契约与 I10 语义，真实 XML 由 onvif.rs 补。
#[derive(Default)]
pub struct OnvifAdapter {
    /// SOAP 客户端（None = 桩模式，能力全 No 但契约正确）。
    client: Option<crate::onvif::OnvifClient>,
}

impl DeviceAdapter for OnvifAdapter {
    fn discover(&self) -> Result<Vec<DeviceCandidate>, AdapterError> {
        crate::onvif::ws_discovery() // P8：WS-Discovery 多播扫描
    }
    fn get_streams(&self, device: &Device) -> Result<Device, AdapterError> {
        match &self.client {
            Some(client) => client.get_streams(device),
            None => Ok(device.clone()),
        }
    }
    fn capabilities(&self, device: &Device) -> Result<Capabilities, AdapterError> {
        match &self.client {
            Some(client) => client.capabilities(device),
            None => Ok(Capabilities::default()),
        }
    }
    fn ptz(&self, device: &Device, cmd: PtzCmd) -> Result<Supported<()>, AdapterError> {
        match &self.client {
            Some(client) => client.ptz(device, cmd),
            None => Ok(Supported::No),
        }
    }
    fn snapshot(&self, device: &Device) -> Result<Supported<Vec<u8>>, AdapterError> {
        match &self.client {
            Some(client) => client.snapshot(device),
            None => Ok(Supported::No),
        }
    }
}

/// 驱动注册表（P6，DESIGN.md §12）。
pub mod registry;

/// GB28181 SIP 信令（P7 骨架 + P8 真实 UDP/TCP + Digest）。
pub mod gb28181;

/// ONVIF SOAP 客户端（P8：真实 HTTP POST + Digest + XML 构造）。
pub mod onvif;

#[cfg(test)]
mod tests {
    use super::*;

    /// I10 契约测试：一个"什么都不支持"的假适配器，前端逻辑应读 Supported::No
    /// 而不是捕获异常。
    struct StubAdapter;

    impl DeviceAdapter for StubAdapter {
        fn discover(&self) -> Result<Vec<DeviceCandidate>, AdapterError> {
            Ok(vec![])
        }
        fn get_streams(&self, d: &Device) -> Result<Device, AdapterError> {
            Ok(d.clone())
        }
        fn capabilities(&self, _d: &Device) -> Result<Capabilities, AdapterError> {
            Ok(Capabilities::default())
        }
        fn ptz(&self, _d: &Device, _c: PtzCmd) -> Result<Supported<()>, AdapterError> {
            Ok(Supported::No) // 无云台——数据，不是 Err
        }
        fn snapshot(&self, _d: &Device) -> Result<Supported<Vec<u8>>, AdapterError> {
            Ok(Supported::No)
        }
    }

    #[test]
    fn unsupported_is_data_not_error() {
        let dev = Device {
            id: "t".into(),
            name: "t".into(),
            access_type: AccessType::Onvif,
            onvif_url: Some("http://1.2.3.4/onvif".into()),
            rtsp_main: None,
            rtsp_sub: None,
            manufacturer: Some("TP-LINK".into()),
            model: None,
            capabilities: Capabilities::default(),
        };
        let a = StubAdapter;
        // 不支持 → Ok(Supported::No)，调用方用数据分支
        assert!(matches!(a.ptz(&dev, PtzCmd::Left), Ok(Supported::No)));
        // 能力探测正常返回（空能力）
        let caps = a.capabilities(&dev).unwrap();
        assert!(!caps.ptz);
        // 协议错误才是 Err
        let err = AdapterError::Auth("bad creds".into());
        assert!(err.to_string().contains("认证"));
    }
}
