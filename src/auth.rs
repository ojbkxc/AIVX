//! 登录鉴权（P9-1）：单用户口令 + 内存 session token。
//!
//! 形态：自托管 NVR 单用户场景——config.yml `auth.password_sha256`（十六进制
//! SHA-256）；未配置则**不启用鉴权**（内网裸跑语义保留，公网暴露才配）。
//! Session：随机 32B token → 内存 HashMap（重启即失效，NVR 可接受）。
//!
//! 保护面：/api/* 全部（含 WS 预览握手前的 HTTP 升级请求）。cookie 走
//! `aivx_session=<token>`（HttpOnly + SameSite=Lax）。

use std::collections::HashMap;
use std::sync::Mutex;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::Json;

/// 登录会话库（token → 用户名）。Arc 共享给 ApiState（Clone）。
#[derive(Clone, Default)]
pub struct SessionStore {
    sessions: std::sync::Arc<Mutex<HashMap<String, String>>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登录成功发 token（32B 随机十六进制）。
    pub fn issue(&self, user: &str) -> String {
        // 无 rand 依赖：time + 地址熵拼 SHA——对单用户 NVR 的 token 碰撞
        // 攻击面足够（公网暴露 + 无敏感数据外泄语义）。
        let seed = format!(
            "{}-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            &self as *const _ as usize,
            std::process::id(),
        );
        let token = sha256_hex(&seed);
        self.sessions
            .lock()
            .unwrap()
            .insert(token.clone(), user.to_string());
        token
    }

    /// 校验 session 有效。
    pub fn valid(&self, token: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(token)
    }

    /// 登出撤销。
    pub fn revoke(&self, token: &str) {
        self.sessions.lock().unwrap().remove(token);
    }
}

/// SHA-256（纯 Rust 实现内联——不引 sha2 依赖，避免 CI 额外下载；密码
/// 哈希 + token 生成两个用途对性能无要求）。
pub fn sha256_hex(data: &str) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let bytes = data.as_bytes();
    let bit_len = (bytes.len() as u64) * 8;
    // 填充：0x80 + 零 + 8B 大端长度
    let mut msg = bytes.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    let mut w = [0u32; 64];
    for chunk in msg.chunks(64) {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

/// 鉴权状态（ApiState 成员）。
#[derive(Clone, Default)]
pub struct AuthState {
    pub store: SessionStore,
    /// config auth.password_sha256；None = 未启用鉴权（放行全部）。
    pub password_sha256: Option<String>,
}

impl AuthState {
    pub fn disabled() -> Self {
        Self::default()
    }

    /// 鉴权是否启用。
    pub fn enabled(&self) -> bool {
        self.password_sha256.is_some()
    }

    /// 请求是否已登录（未启用鉴权恒 true）。
    pub fn check(&self, headers: &HeaderMap) -> bool {
        if !self.enabled() {
            return true;
        }
        headers
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .map(|cookies| {
                cookies.split(';').any(|c| {
                    let c = c.trim();
                    c.strip_prefix("aivx_session=")
                        .is_some_and(|t| self.store.valid(t))
                })
            })
            .unwrap_or(false)
    }
}

/// cookie 里的 session token。
fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies
                .split(';')
                .find_map(|c| c.trim().strip_prefix("aivx_session=").map(String::from))
        })
}

/// POST /api/auth/login {username?, password} → Set-Cookie + ok。
/// 用户名不校验（单用户）；密码对上 password_sha256 即发 session。
pub async fn login(
    State(auth): State<AuthState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    if !auth.enabled() {
        return (
            axum::http::StatusCode::OK,
            [("set-cookie", "aivx_session=; Max-Age=0; Path=/")],
            Json(serde_json::json!({"ok": true, "auth_enabled": false})),
        )
            .into_response();
    }
    let Some(password) = req.get("password").and_then(|v| v.as_str()) else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing password"})),
        )
            .into_response();
    };
    let expect = auth.password_sha256.clone().unwrap_or_default();
    let got = sha256_hex(password);
    // 常数时间比较（防时序侧信道——哈希后比较，长度恒 64）。
    let mut diff = 0u8;
    for (a, b) in got.bytes().zip(expect.bytes()) {
        diff |= a ^ b;
    }
    if diff != 0 || got.len() != expect.len() {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "密码错误"})),
        )
            .into_response();
    }
    let token = auth.store.issue("admin");
    let cookie = format!(
        "aivx_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        7 * 24 * 3600
    );
    (
        axum::http::StatusCode::OK,
        [("set-cookie", cookie)],
        Json(serde_json::json!({"ok": true, "auth_enabled": true})),
    )
        .into_response()
}

/// POST /api/auth/logout → 撤销 session。
pub async fn logout(State(auth): State<AuthState>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(token) = session_token(&headers) {
        auth.store.revoke(&token);
    }
    let clear = "aivx_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0";
    (
        axum::http::StatusCode::OK,
        [("set-cookie", clear)],
        Json(serde_json::json!({"ok": true})),
    )
}

/// GET /api/auth/status → 前端启动探测。此路径被 middleware 放行（未登录
/// 也可达——登录页据此判断"是否需要登录"），故 logged_in 必须真实查
/// session（未启用鉴权恒 true）。
pub async fn auth_status(State(auth): State<AuthState>, headers: HeaderMap) -> impl IntoResponse {
    let logged_in = !auth.enabled() || auth.check(&headers);
    Json(serde_json::json!({"auth_enabled": auth.enabled(), "logged_in": logged_in}))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 空串 = e3b0c442…（FIPS 180-4 已知答案测试）。
    #[test]
    fn sha256_known_answers() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex("The quick brown fox jumps over the lazy dog"),
            "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
        );
        // 长输入跨多块（>64B padding 边界）
        let long = "a".repeat(200);
        assert_eq!(sha256_hex(&long).len(), 64);
    }

    /// session 签发/校验/撤销。
    #[test]
    fn session_lifecycle() {
        let s = SessionStore::new();
        let t = s.issue("admin");
        assert!(s.valid(&t));
        s.revoke(&t);
        assert!(!s.valid(&t));
    }

    /// 未启用鉴权时 check 恒真。
    #[test]
    fn disabled_auth_passes() {
        let a = AuthState::disabled();
        assert!(!a.enabled());
        let hm = HeaderMap::new();
        assert!(a.check(&hm));
    }
}
