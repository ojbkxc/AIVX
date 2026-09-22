//! P9-5 TP-LINK 摄像头 PTZ 操控（NVR 私有协议，实证 2026-09-22）。
//!
//! 协议实证结论（TL-NVR 通过 IPC 私有 RPC 控云台）：
//! - 登录：`POST /passport/auth` → nonce → md5(pwd:nonce) → `POST /` → stok
//! - 会话：`POST /stok=<enc>/ds`（aivx-net::tplink_ipc 现成）
//! - `ptz.get_ptz_status`：position_pan/tilt + status（idle/moving）
//! - `ptz.absolute_move`：绝对定位（度数；此机 pan≈±1.x° tilt≈[-1,0.5]，超范围 -64314）
//! - `preset.set_preset` {name, channel_id} → {id}（**NVR 上限 8 个**，满 → -64306）
//! - `preset.goto_preset` {id, channel_id} → 跳转
//! - `preset.remove_preset`：**NVR 固件恒 -1 不生效**（IPC 才支持）——AIVX 不暴露删除
//! - NVR 的 get_ptz_status **忽略 channel_id**（只回一路云台状态）——状态回显
//!   按 NVR 级展示，不按通道
//!
//! 设备映射：config.yml 的 RTSP URL `rtsp://user:pass@host:554/stream1&channel=N`
//! → NVR host + channel。凭据内嵌 URL，启动期提取注册 IpcRegistry。

use std::collections::HashMap;

use aivx_net::tplink_ipc::{IpcDevice, IpcPtzStatus};
use serde::{Deserialize, Serialize};

/// 一路 PTZ 可控设备的注册信息（从 RTSP URL 提取）。
#[derive(Clone)]
pub struct PtzTarget {
    pub device_id: String,
    pub nvr_host: String,
    pub channel: u32,
    pub ipc: std::sync::Arc<IpcDevice>,
}

/// PTZ 注册表：设备 ID → 控制目标。启动期从 config.yml 建立。
#[derive(Default)]
pub struct PtzRegistry {
    targets: HashMap<String, PtzTarget>,
}

impl PtzRegistry {
    /// 从 AivxYaml 的 RTSP URL 提取 PTZ 目标并注册。
    ///
    /// URL 形如 `rtsp://admin:pass@192.168.31.201:554/stream1&channel=2`：
    /// host/凭据来自 authority 段，channel 来自 `&channel=` 查询参数。
    /// 非 TP-LINK 形态（无 channel）不注册——capabilities.ptz 按注册判定。
    pub fn from_yaml_cameras(yaml: &crate::cameras::AivxYaml) -> Self {
        let mut targets = HashMap::new();
        for (name, cam) in &yaml.cameras {
            let Some(url) = cam.ffmpeg.inputs.first().map(|i| i.path.clone()) else {
                continue;
            };
            let Some((host, user, pass, channel)) = parse_rtsp_nvr(&url) else {
                continue;
            };
            targets.insert(
                name.clone(),
                PtzTarget {
                    device_id: name.clone(),
                    nvr_host: host.clone(),
                    channel,
                    ipc: std::sync::Arc::new(IpcDevice::new(&host, &user, &pass)),
                },
            );
        }
        Self { targets }
    }

    pub fn get(&self, device_id: &str) -> Option<&PtzTarget> {
        self.targets.get(device_id)
    }

    /// PTZ 可控设备 ID 集（list_config 标 capabilities 用）。
    pub fn controllable(&self, device_id: &str) -> bool {
        self.targets.contains_key(device_id)
    }
}

/// 解析 `rtsp://user:pass@host:port/stream1&channel=N` → (host, user, pass, channel)。
///
/// channel 缺省 1（TP-LINK NVR 单通道相机场景）。authority 段无凭据则放弃
/// （NVR 登录必须有用户名密码）。
fn parse_rtsp_nvr(url: &str) -> Option<(String, String, String, u32)> {
    let rest = url.strip_prefix("rtsp://")?;
    // authority（@ 前是凭据，后到首个 / 是 host）
    let (auth, path) = rest.split_once('@')?;
    let (host, _) = path.split_once('/').unwrap_or((path, ""));
    let host = host.split(':').next().unwrap_or(host).to_string();
    if host.is_empty() {
        return None;
    }
    let (user, pass) = auth.split_once(':')?;
    if user.is_empty() || pass.is_empty() {
        return None;
    }
    // channel：路径/查询里的 `&channel=N`（TP-LINK RTSP 语法）
    let channel = path
        .split("channel=")
        .nth(1)
        .and_then(|s| s.split('&').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    Some((host, user.to_string(), pass.to_string(), channel))
}

// ── API DTO ───────────────────────────────────────────────────

/// GET /api/ptz/{id}/status 响应。
#[derive(Serialize)]
pub struct PtzStatusResp {
    pub device_id: String,
    pub position_pan: f64,
    pub position_tilt: f64,
    pub moving: bool,
}

impl PtzStatusResp {
    pub fn from_ipc(device_id: &str, s: &IpcPtzStatus) -> Self {
        Self {
            device_id: device_id.to_string(),
            position_pan: s.position_pan.parse().unwrap_or(0.0),
            position_tilt: s.position_tilt.parse().unwrap_or(0.0),
            moving: s.status_pan == "moving" || s.status_tilt == "moving",
        }
    }
}

/// POST /api/ptz/{id}/move 请求：相对增量（度）或绝对定位。
#[derive(Deserialize)]
pub struct PtzMoveReq {
    /// 绝对定位（度）；与 delta 二选一。
    pub pan: Option<f64>,
    pub tilt: Option<f64>,
    /// 相对增量（度）：当前位置 + delta。
    pub d_pan: Option<f64>,
    pub d_tilt: Option<f64>,
}

/// POST /api/ptz/{id}/preset 请求。
#[derive(Deserialize)]
pub struct PtzPresetReq {
    /// set：新建预置位（名字必填）；goto：跳转（id 必填）。
    #[serde(rename = "type")]
    pub action: String,
    pub name: Option<String>,
    pub id: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// URL 解析：TP-LINK NVR 通道语法。
    #[test]
    fn parse_nvr_url() {
        let (h, u, p, ch) =
            parse_rtsp_nvr("rtsp://admin:secret@192.168.31.201:554/stream1&channel=2").unwrap();
        assert_eq!(
            (h.as_str(), u.as_str(), p.as_str(), ch),
            ("192.168.31.201", "admin", "secret", 2)
        );
        // 缺 channel → 默认 1
        let (_, _, _, ch) = parse_rtsp_nvr("rtsp://a:b@10.0.0.5:554/stream1").unwrap();
        assert_eq!(ch, 1);
        // 无凭据/非 rtsp → None
        assert!(parse_rtsp_nvr("rtsp://192.168.31.201/stream1").is_none());
        assert!(parse_rtsp_nvr("http://a:b@1.2.3.4/x").is_none());
    }

    /// registry 注册：yaml 两台 NVR 四通道各得一个 PTZ 目标。
    #[test]
    fn registry_from_yaml() {
        let mut yaml = crate::cameras::AivxYaml::default();
        for (name, ch) in [("tp_1-1", 1u32), ("tp_1-2", 2)] {
            yaml.cameras.insert(
                name.into(),
                crate::config_store::new_camera(
                    &format!("rtsp://admin:pw@192.168.31.201:554/stream1&channel={ch}"),
                    "off",
                    0,
                ),
            );
        }
        let reg = PtzRegistry::from_yaml_cameras(&yaml);
        assert!(reg.controllable("tp_1-1"));
        assert!(!reg.controllable("tp_1-9"));
        let t = reg.get("tp_1-2").unwrap();
        assert_eq!(t.nvr_host, "192.168.31.201");
        assert_eq!(t.channel, 2);
    }

    /// 状态 DTO：字符串位置 → 数值 + moving 判定。
    #[test]
    fn status_dto() {
        let s = IpcPtzStatus {
            position_pan: "-0.268136".into(),
            position_tilt: "0.380497".into(),
            status_pan: "moving".into(),
            status_tilt: "idle".into(),
        };
        let r = PtzStatusResp::from_ipc("tp_1-1", &s);
        assert!((r.position_pan - (-0.268136)).abs() < 1e-6);
        assert!(r.moving);
    }
}
