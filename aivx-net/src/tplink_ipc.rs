//! TP-LINK IPC 私有控制协议（P8c，协议实证 2026-09-20）。
//!
//! 探测实证（Debian 服务器 → TL-IPC6109-A4）：
//! - 登录：`POST /passport/auth {"method":"get"}` 拿 nonce →
//!   `POST / {"method":"do","login":{"username","password":md5(pwd:nonce),
//!   "encrypt_type":1,"md5_encrypt_type":"1"}}` → `stok`
//! - 会话：`POST /stok=<url_encode(stok)>/ds`
//! - 表格查询：`{"method":"get","<file>":{"table":["<section>"]}}`
//! - do 动作：`{"method":"do","<file>":{"<action>":{...}}}`
//!
//! 实证端点：absolute_move / get_ptz_status / set_preset / goto_preset /
//! device_info.info / video.stream / system.system / motion_detection.sound_alarm_info
//!
//! 参照：HomeAssistant-Tapo-Control 的能力菜单（PTZ/预置位/侦测/系统），
//! AIVX 对齐到 IPC 私有 RPC（Tapo 443 协议不适用于此设备——无 443 端口，ADR-032）。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::Capabilities;

/// IPC 会话（stok）。
pub struct IpcSession {
    http: reqwest::blocking::Client,
    base: String,
    stok: String,
}

/// 登录流程（md5_encrypt_type=1：password = md5(密码:nonce)）。
///
/// 错误码语义（探测实证）：
/// - -40401: 需要登录（挑战数据随附）
/// - -40106: 动作/键名不存在
/// - -40210: 段不存在
/// - -64302: 参数名/格式错误
pub fn login(host: &str, username: &str, password: &str) -> Result<IpcSession, String> {
    let http = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| e.to_string())?;
    let base = format!("http://{}", host);

    // 1. 挑战
    let chal: Value = http
        .post(format!("{}/passport/auth", base))
        .json(&json!({"method": "get"}))
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let nonce = chal
        .pointer("/data/nonce")
        .and_then(|v| v.as_str())
        .ok_or("challenge missing nonce")?;

    // 2. md5(密码:nonce) —— ADR-029 同款 md-5 crate 用法
    let digest: [u8; 16] = {
        use md5::Digest as _;
        let mut hasher = md5::Md5::new();
        hasher.update(format!("{}:{}", password, nonce).as_bytes());
        hasher.finalize().into()
    };
    let passmd5 = digest
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();

    // 3. 登录
    let login_resp: Value = http
        .post(&base)
        .json(&json!({
            "method": "do",
            "login": {
                "username": username,
                "password": passmd5,
                "encrypt_type": 1,
                "md5_encrypt_type": "1",
            }
        }))
        .send()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let stok = login_resp
        .get("stok")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("login failed: {}", login_resp))?
        .to_string();

    Ok(IpcSession { http, base, stok })
}

impl IpcSession {
    fn ds(&self, body: Value) -> Result<Value, String> {
        let url = format!("{}/stok={}/ds", self.base, url_encode(&self.stok));
        let resp: Value = self
            .http
            .post(url)
            .json(&body)
            .send()
            .map_err(|e| e.to_string())?
            .json()
            .map_err(|e| e.to_string())?;
        let code = resp
            .get("error_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if code != 0 {
            return Err(format!("ipc error {}", code));
        }
        Ok(resp)
    }

    /// 表格查询：`{file: {"table": [section]}}`。
    pub fn table(&self, file: &str, section: &str) -> Result<Value, String> {
        self.ds(json!({"method": "get", file: {"table": [section]}}))
    }

    /// do 动作。
    pub fn action(&self, file: &str, action: &str, params: Value) -> Result<Value, String> {
        self.ds(json!({"method": "do", file: {action: params}}))
    }

    // ── 实证端点封装 ─────────────────────────────────────

    /// 设备信息（TL-IPC6109-A4 等）。
    pub fn device_info(&self) -> Result<IpcDeviceInfo, String> {
        let v = self.table("device_info", "info")?;
        let row = v
            .pointer("/device_info/info/0/info")
            .cloned()
            .unwrap_or(Value::Null);
        Ok(IpcDeviceInfo {
            device_model: str_field(&row, "device_model"),
            device_name: str_field(&row, "device_name"),
            manufacturer: str_field(&row, "manufacturer_name"),
            sw_version: str_field(&row, "sw_version"),
            hw_version: str_field(&row, "hw_version"),
        })
    }

    /// PTZ 状态（position_pan/tilt + status_pan/tilt）。
    pub fn ptz_status(&self) -> Result<IpcPtzStatus, String> {
        let v = self.action("ptz", "get_ptz_status", json!({}))?;
        let row = v.pointer("/ptz/status").cloned().unwrap_or(Value::Null);
        Ok(IpcPtzStatus {
            position_pan: str_field(&row, "position_pan"),
            position_tilt: str_field(&row, "position_tilt"),
            status_pan: str_field(&row, "status_pan"),
            status_tilt: str_field(&row, "status_tilt"),
        })
    }

    /// PTZ 绝对移动（实证：position_pan/position_tilt，度数小数）。
    pub fn ptz_absolute_move(&self, pan: f64, tilt: f64) -> Result<(), String> {
        self.action(
            "ptz",
            "absolute_move",
            json!({"position_pan": pan.to_string(), "position_tilt": tilt.to_string()}),
        )
        .map(|_| ())
    }

    /// 保存预置位（返回分配的 id）。
    pub fn save_preset(&self, name: &str) -> Result<u32, String> {
        let v = self.action("preset", "set_preset", json!({"name": name}))?;
        v.get("id")
            .and_then(|x| {
                x.as_str()
                    .and_then(|s| s.parse().ok())
                    .or_else(|| x.as_u64().map(|n| n as u32))
            })
            .ok_or_else(|| format!("set_preset no id: {}", v))
    }

    /// 转到预置位。
    pub fn goto_preset(&self, id: u32) -> Result<(), String> {
        self.action("preset", "goto_preset", json!({"id": id.to_string()}))
            .map(|_| ())
    }

    /// 码流信息（分辨率/编码/帧率/码率）。
    pub fn video_stream(&self) -> Result<Value, String> {
        self.table("video", "stream")
    }

    /// 系统信息（别名/时区）。
    pub fn system(&self) -> Result<Value, String> {
        self.table("system", "system")
    }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| url_decode(s))
        .unwrap_or_default()
}

/// URL 解码（IPC 返回 %20 等编码的中文/空格）。
fn url_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 最小 URL 编码（stok 含特殊字符必须编码——实证含 `)` `!` `*`）。
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 设备信息（device_info.info）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IpcDeviceInfo {
    pub device_model: String,
    pub device_name: String,
    pub manufacturer: String,
    pub sw_version: String,
    pub hw_version: String,
}

/// PTZ 状态（get_ptz_status）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IpcPtzStatus {
    pub position_pan: String,
    pub position_tilt: String,
    pub status_pan: String,
    pub status_tilt: String,
}

/// 一台 IPC 的管理句柄（host + 凭据 → 按需登录缓存 stok）。
///
/// stok 有时效；`call` 失败码 -40401 时重登一次再试（自动续会话）。
pub struct IpcDevice {
    pub host: String,
    pub username: String,
    pub password: String,
    session: std::sync::Mutex<Option<IpcSession>>,
}

impl IpcDevice {
    pub fn new(host: &str, username: &str, password: &str) -> Self {
        Self {
            host: host.to_string(),
            username: username.to_string(),
            password: password.to_string(),
            session: std::sync::Mutex::new(None),
        }
    }

    /// 取有效会话（无则登录；-40401 自动重登一次）。
    fn with_session<T>(&self, f: impl Fn(&IpcSession) -> Result<T, String>) -> Result<T, String> {
        let mut guard = self.session.lock().unwrap();
        if guard.is_none() {
            *guard = Some(login(&self.host, &self.username, &self.password)?);
        }
        let sess = guard.as_ref().unwrap();
        match f(sess) {
            Ok(v) => Ok(v),
            Err(e) if e.contains("ipc error -40401") => {
                // 会话过期——重登重试一次
                *guard = Some(login(&self.host, &self.username, &self.password)?);
                f(guard.as_ref().unwrap())
            }
            Err(e) => Err(e),
        }
    }

    pub fn device_info(&self) -> Result<IpcDeviceInfo, String> {
        self.with_session(|s| s.device_info())
    }

    pub fn ptz_status(&self) -> Result<IpcPtzStatus, String> {
        self.with_session(|s| s.ptz_status())
    }

    pub fn ptz_absolute_move(&self, pan: f64, tilt: f64) -> Result<(), String> {
        self.with_session(|s| s.ptz_absolute_move(pan, tilt))
    }

    pub fn save_preset(&self, name: &str) -> Result<u32, String> {
        self.with_session(|s| s.save_preset(name))
    }

    pub fn goto_preset(&self, id: u32) -> Result<(), String> {
        self.with_session(|s| s.goto_preset(id))
    }

    pub fn video_stream(&self) -> Result<Value, String> {
        self.with_session(|s| s.video_stream())
    }
}

/// IPC 设备清单（host → 句柄），控制面持有。
#[derive(Default)]
pub struct IpcRegistry {
    devices: HashMap<String, std::sync::Arc<IpcDevice>>,
}

impl IpcRegistry {
    pub fn register(&mut self, id: &str, host: &str, username: &str, password: &str) {
        self.devices.insert(
            id.to_string(),
            std::sync::Arc::new(IpcDevice::new(host, username, password)),
        );
    }

    pub fn get(&self, id: &str) -> Option<std::sync::Arc<IpcDevice>> {
        self.devices.get(id).cloned()
    }

    /// 探测能力（I10：失败是数据不是异常——capabilities 按实证端点填）。
    pub fn probe_capabilities(device: &IpcDevice) -> Capabilities {
        // IPC 6109 实证：PTZ + 预置位可用；音频/事件订阅未实证（默认 false）
        let ptz = device.ptz_status().is_ok();
        Capabilities {
            ptz,
            main_sub_streams: true,
            ..Default::default()
        }
    }
}
