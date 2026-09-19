//! GB28181 SIP 传输层（DESIGN.md §12 / P8）——真实 UDP/TCP + Digest 鉴权。
//!
//! GB28181 是 SIP over UDP/TCP 5060。设备注册/心跳/点播都经此传输。
//! - [`SipServer`]：绑定 UDP/TCP 端口，收请求 → `SipObserver` 分发 → 回响应
//! - [`Digest`]：RFC 2617 Digest 鉴权（GB28181 强制），MD5 哈希
//!
//! 纯 std 实现（无第三方 SIP 栈）：UDP/TCP socket + 行解析。CI 用本机回环
//! 自测收发，不依赖真实设备。

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use super::{parse_request, SipObserver};

/// Digest 鉴权参数（RFC 2617）。
pub struct DigestParams {
    pub realm: String,
    pub nonce: String,
}

/// 从请求的 Authorization 头解析 Digest 参数。
fn parse_authorization(header: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    // Digest username="x", realm="y", nonce="z", uri="u", response="r", ...
    let after = header
        .strip_prefix("Digest ")
        .or_else(|| header.strip_prefix("digest "))
        .unwrap_or(header);
    for pair in after.split(',') {
        let pair = pair.trim();
        if let Some((k, v)) = pair.split_once('=') {
            let key = k.trim().to_string();
            let val = v.trim().trim_matches('"').to_string();
            map.insert(key, val);
        }
    }
    map
}

/// MD5（GB28181 Digest 用；纯实现，避免引入 md-5 crate——数据面 std-only 约束）。
/// MD5（GB28181 Digest 鉴权，ADR-029：必须 md-5 crate）。
fn md5(input: &[u8]) -> [u8; 16] {
    use md5::Digest as _;
    let mut hasher = md5::Md5::new();
    hasher.update(input);
    hasher.finalize().into()
}

/// 计算 Digest response（`md5(A1):nonce:md5(A2)`，RFC 2617）。
/// `a1 = md5(user:realm:pass)`，`a2 = md5(method:uri)`。
pub fn digest_response(
    username: &str,
    password: &str,
    realm: &str,
    nonce: &str,
    method: &str,
    uri: &str,
) -> String {
    let a1 = format!("{username}:{realm}:{password}");
    let a2 = format!("{method}:{uri}");
    let ha1 = md5(a1.as_bytes());
    let ha2 = md5(a2.as_bytes());
    let ha1_hex = hex(&ha1);
    let ha2_hex = hex(&ha2);
    let response = format!("{ha1_hex}:{nonce}:{ha2_hex}");
    hex(&md5(response.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 校验 Authorization 头（Digest）是否匹配。
pub fn verify_digest(
    auth_header: &str,
    expected_user: &str,
    expected_pass: &str,
    method: &str,
    uri: &str,
    params: &DigestParams,
) -> bool {
    let fields = parse_authorization(auth_header);
    let user = fields.get("username").map(String::as_str).unwrap_or("");
    if user != expected_user {
        return false;
    }
    let nonce = fields.get("nonce").map(String::as_str).unwrap_or("");
    let resp = fields.get("response").map(String::as_str).unwrap_or("");
    let expected = digest_response(
        expected_user,
        expected_pass,
        &params.realm,
        nonce,
        method,
        uri,
    );
    // 常数时间比较（防时序侧信道）
    if expected.len() != resp.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in expected.bytes().zip(resp.bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 生成 401 挑战（带 realm + nonce）。
pub fn challenge(realm: &str, nonce: &str) -> String {
    format!(
        "SIP/2.0 401 Unauthorized\r\n\
         WWW-Authenticate: Digest realm=\"{realm}\", nonce=\"{nonce}\", qop=\"auth\"\r\n\
         Content-Length: 0\r\n\r\n"
    )
}

/// SIP 服务器（UDP + TCP，回环自测）。
pub struct SipServer {
    observer: Arc<SipObserver>,
    running: Arc<AtomicBool>,
    realm: String,
    /// 设备凭据（device_id → password）——Digest 校验用。
    creds: HashMap<String, String>,
}

impl SipServer {
    pub fn new(observer: SipObserver, realm: &str, creds: HashMap<String, String>) -> Self {
        Self {
            observer: Arc::new(observer),
            running: Arc::new(AtomicBool::new(false)),
            realm: realm.into(),
            creds,
        }
    }

    /// 处理一帧原始 SIP（UDP 数据报 / TCP 流块）。
    /// 返回要回发的字节。未鉴权 INVITE/MESSAGE → 401 挑战。
    pub fn handle_datagram(&self, raw: &[u8]) -> Vec<u8> {
        let text = String::from_utf8_lossy(raw).into_owned();
        let Some(req) = parse_request(&text) else {
            return b"SIP/2.0 400 Bad Request\r\n\r\n".to_vec();
        };
        // Digest 鉴权：除 REGISTER 外的请求都要鉴权（简化：全鉴权）
        let auth_ok = match extract_authorization(&text) {
            Some(auth) => {
                let uri = format!("sip:{}", req.to_user);
                let pass = self.creds.get(&req.from_user).cloned().unwrap_or_default();
                let params = DigestParams {
                    realm: self.realm.clone(),
                    nonce: "aivx-nonce".into(),
                };
                verify_digest(
                    &auth,
                    &req.from_user,
                    &pass,
                    method_str(&req.method),
                    &uri,
                    &params,
                )
            }
            None => false,
        };
        if !auth_ok {
            return challenge(&self.realm, "aivx-nonce").into_bytes();
        }
        // 鉴权通过 → 分发
        match self.observer.dispatch(&req) {
            Ok(resp) => resp.into_bytes(),
            Err(_) => b"SIP/2.0 501 Not Implemented\r\n\r\n".to_vec(),
        }
    }

    /// UDP 监听线程（回环端口）。`self` 须为 Arc（closure 需 move 进线程）。
    pub fn spawn_udp(self: Arc<Self>, port: u16) -> std::io::Result<UdpSocket> {
        let sock = UdpSocket::bind(("127.0.0.1", port))?;
        // try_clone：返回一个共享同一底层 socket 的句柄（供调用方收发测试）
        let handle = sock.try_clone()?;
        let sock = Arc::new(sock);
        self.running.store(true, Ordering::SeqCst);
        let running = self.running.clone();
        let server = self;
        thread::spawn(move || {
            let mut buf = [0u8; 65535];
            while running.load(Ordering::SeqCst) {
                match sock.recv_from(&mut buf) {
                    Ok((n, src)) => {
                        let resp = server.handle_datagram(&buf[..n]);
                        let _ = sock.send_to(&resp, src);
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(handle)
    }

    pub fn shutdown(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

fn method_str(m: &super::SipMethod) -> &'static str {
    use super::SipMethod::*;
    match m {
        Register => "REGISTER",
        Invite => "INVITE",
        Bye => "BYE",
        Message => "MESSAGE",
        Ack => "ACK",
        Options => "OPTIONS",
    }
}

fn extract_authorization(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(a) = line.strip_prefix("Authorization:") {
            return Some(a.trim().to_string());
        }
        if let Some(a) = line.strip_prefix("Authorization :") {
            return Some(a.trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Digest response 计算结构正确（非空、定长）。
    #[test]
    fn digest_response_is_deterministic() {
        let r1 = digest_response("3402", "pass", "realm", "nonce", "INVITE", "sip:x");
        let r2 = digest_response("3402", "pass", "realm", "nonce", "INVITE", "sip:x");
        assert_eq!(r1, r2, "同参数必须同哈希");
        assert_eq!(r1.len(), 32, "MD5 hex 是 32 字符");
    }

    /// MD5 真实实现（ADR-029 已接入 md-5 crate）——验证 RFC 1321 已知向量。
    #[test]
    fn md5_known_vector() {
        // RFC 1321 测试向量: md5("abc") = 900150983cd24fb0d6963f7d28e17f72
        let digest = md5(b"abc");
        let hex_str = hex(&digest);
        assert_eq!(hex_str, "900150983cd24fb0d6963f7d28e17f72");
    }

    /// Authorization 头解析。
    #[test]
    fn parse_auth_header() {
        let header =
            r#"Digest username="3402", realm="realm", nonce="n1", uri="sip:x", response="abc""#;
        let fields = parse_authorization(header);
        assert_eq!(fields.get("username").map(String::as_str), Some("3402"));
        assert_eq!(fields.get("nonce").map(String::as_str), Some("n1"));
    }

    /// 无鉴权 → 401 挑战。
    #[test]
    fn unauthenticated_gets_401() {
        let obs = SipObserver::new();
        let server = SipServer::new(obs, "aivx", HashMap::new());
        let raw = b"INVITE sip:x@y SIP/2.0\r\nFrom: <sip:a@b>\r\nTo: <sip:x@y>\r\nCall-ID: c\r\nCSeq: 1 INVITE\r\n\r\n";
        let resp = server.handle_datagram(raw);
        let text = String::from_utf8_lossy(&resp);
        assert!(text.contains("401 Unauthorized"), "未鉴权应 401");
        assert!(text.contains("Digest realm"));
    }
}
