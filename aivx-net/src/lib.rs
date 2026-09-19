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

/// ONVIF 通用兜底驱动（P6 桩：能力全 No，等 P7 接真实 SOAP 实现）。
pub struct OnvifAdapter;

impl DeviceAdapter for OnvifAdapter {
    fn discover(&self) -> Result<Vec<DeviceCandidate>, AdapterError> {
        Ok(vec![]) // P7：WS-Discovery 多播扫描
    }
    fn get_streams(&self, device: &Device) -> Result<Device, AdapterError> {
        Ok(device.clone()) // P7：GetProfiles → GetStreamUri
    }
    fn capabilities(&self, _d: &Device) -> Result<Capabilities, AdapterError> {
        Ok(Capabilities::default()) // P7：GetCapabilities 探测
    }
    fn ptz(&self, _d: &Device, _c: PtzCmd) -> Result<Supported<()>, AdapterError> {
        Ok(Supported::No) // P7：ContinuousMove 实现
    }
    fn snapshot(&self, _d: &Device) -> Result<Supported<Vec<u8>>, AdapterError> {
        Ok(Supported::No) // P7：GetSnapshotUri
    }
}

/// 驱动注册表（P6，DESIGN.md §12）。
pub mod registry;

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
