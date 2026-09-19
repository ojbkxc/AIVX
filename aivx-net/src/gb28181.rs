//! GB28181 SIP 信令（DESIGN.md §12 / P7）——对齐 ruoyi-gb28181 `transmit/`。
//!
//! 抄 ruoyi 的 SIP 观察者分发：
//! - `SIPProcessorObserver` 用 map 按 method 分发 Request/Response
//! - INVITE（点播/回放）→ 解析 SDP → 起流 → 200 OK 带 SDP
//! - BYE（停播）、MESSAGE（Keepalive/Catalog 等，二级按 CmdType 分发）
//!
//! P7 骨架：消息解析 + 观察者分发 + SIP 方法枚举 + 事务状态。真实 UDP/TCP
//! 传输 + Digest 鉴权在 P8（需引入 SIP 栈）。

use std::collections::HashMap;

/// SIP 方法（GB28181 用到的子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SipMethod {
    Register,
    Invite,
    Bye,
    Message,
    Ack,
    Options,
}

impl SipMethod {
    /// 从请求行解析（"INVITE sip:xxx@yyy SIP/2.0"）。
    pub fn from_request_line(line: &str) -> Option<SipMethod> {
        let method = line.split_whitespace().next()?;
        match method {
            "REGISTER" => Some(SipMethod::Register),
            "INVITE" => Some(SipMethod::Invite),
            "BYE" => Some(SipMethod::Bye),
            "MESSAGE" => Some(SipMethod::Message),
            "ACK" => Some(SipMethod::Ack),
            "OPTIONS" => Some(SipMethod::Options),
            _ => None,
        }
    }
}

/// SIP 请求（P7 骨架：只解析关键头）。
#[derive(Debug, Clone)]
pub struct SipRequest {
    pub method: SipMethod,
    /// Request-URI user（To/From 头里的 user = 国标编号）。
    pub from_user: String,
    pub to_user: String,
    /// CSeq 序号（事务关联）。
    pub cseq: u32,
    /// 消息体（INVITE 的 SDP / MESSAGE 的 XML）。
    pub body: String,
    /// Call-ID（会话标识）。
    pub call_id: String,
}

/// GB28181 MESSAGE 的 CmdType（XML 里的业务命令，二级分发）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmdType {
    Keepalive,
    Catalog,
    DeviceInfo,
    RecordInfo,
    Unknown,
}

impl CmdType {
    /// 从 XML 提取 CmdType（简化正则；P8 用 xml 解析）。
    pub fn from_xml(body: &str) -> CmdType {
        if body.contains("CmdType") && body.contains("Keepalive") {
            CmdType::Keepalive
        } else if body.contains("Catalog") {
            CmdType::Catalog
        } else if body.contains("DeviceInfo") {
            CmdType::DeviceInfo
        } else if body.contains("RecordInfo") {
            CmdType::RecordInfo
        } else {
            CmdType::Unknown
        }
    }
}

/// SIP 处理器 trait（观察者模式，抄 ruoyi ISIPRequestProcessor）。
pub trait SipProcessor: Send + Sync {
    fn method(&self) -> SipMethod;
    fn process(&self, req: &SipRequest) -> Result<String, String>;
}

/// 观察者：按 method 分发（抄 ruoyi SIPProcessorObserver）。
pub struct SipObserver {
    handlers: HashMap<SipMethod, Box<dyn SipProcessor>>,
}

impl Default for SipObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl SipObserver {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn register(&mut self, processor: Box<dyn SipProcessor>) {
        let method = processor.method();
        self.handlers.insert(method, processor);
    }

    /// 分发请求。返回响应文本（200 OK 行 + body）或错误。
    pub fn dispatch(&self, req: &SipRequest) -> Result<String, String> {
        match self.handlers.get(&req.method) {
            Some(p) => p.process(req),
            None => Err(format!("无处理器: {:?}", req.method)),
        }
    }

    pub fn has_handler(&self, method: SipMethod) -> bool {
        self.handlers.contains_key(&method)
    }
}

/// INVITE 处理器（点播/回放）——抄 ruoyi InviteRequestProcessor。
///
/// P7 骨架：解析 SDP 的 o= 行取平台 ID + 判断是否回放（s=play/playback），
/// 返回 200 OK 带 SDP（真实起流在 P8 接 ZLM）。
pub struct InviteProcessor {
    /// 本地 SIP 域（构造 200 OK 的 From 头）。
    pub local_domain: String,
}

impl SipProcessor for InviteProcessor {
    fn method(&self) -> SipMethod {
        SipMethod::Invite
    }

    fn process(&self, req: &SipRequest) -> Result<String, String> {
        // 从 SDP 的 o= 行取对端平台 ID（ruoyi：sdpPlatformId）
        let platform_id = extract_o_line_user(&req.body).unwrap_or_else(|| req.from_user.clone());
        // 判断回放：s=playback 或含 start/end 时间
        let is_playback = req.body.contains("s=playback") || req.body.contains("start=");
        let session = if is_playback { "playback" } else { "live" };
        Ok(format!(
            "SIP/2.0 200 OK\r\nFrom: <sip:{}@{}>\r\nTo: <sip:{}@{}>\r\nCall-ID: {}\r\nCSeq: {} INVITE\r\nContent-Type: application/sdp\r\n\r\nv=0\r\no={} 0 0 IN IP4 {}\r\ns={}\r\nc=IN IP4 0.0.0.0\r\nm=video 0 RTP/AVP 96",
            req.from_user, self.local_domain, req.to_user, self.local_domain,
            req.call_id, req.cseq, platform_id, self.local_domain, session
        ))
    }
}

/// MESSAGE 处理器（Keepalive/Catalog 等，二级按 CmdType 分发）——抄 ruoyi MessageRequestProcessor。
pub struct MessageProcessor;

impl SipProcessor for MessageProcessor {
    fn method(&self) -> SipMethod {
        SipMethod::Message
    }

    fn process(&self, req: &SipRequest) -> Result<String, String> {
        let cmd = CmdType::from_xml(&req.body);
        match cmd {
            CmdType::Keepalive => Ok("SIP/2.0 200 OK\r\n\r\n".to_string()),
            CmdType::Catalog => Ok("SIP/2.0 200 OK\r\n\r\n".to_string()),
            CmdType::Unknown => Err("未知 CmdType".into()),
            _ => Ok("SIP/2.0 200 OK\r\n\r\n".to_string()),
        }
    }
}

/// 从 SDP 的 o= 行提取用户名（ruoyi：`o=34020000001320000001 0 0 IN IP4 ...`）。
fn extract_o_line_user(sdp: &str) -> Option<String> {
    for line in sdp.lines() {
        if let Some(rest) = line.strip_prefix("o=") {
            return rest.split_whitespace().next().map(String::from);
        }
    }
    None
}

/// 解析 SIP 请求（P7 骨架：处理请求行 + From/To/CSeq/Call-ID + 空行后的 body）。
pub fn parse_request(raw: &str) -> Option<SipRequest> {
    let mut lines = raw.lines();
    let request_line = lines.next()?;
    let method = SipMethod::from_request_line(request_line)?;

    let mut from_user = String::new();
    let mut to_user = String::new();
    let mut cseq = 0u32;
    let mut call_id = String::new();
    let mut body = String::new();
    let mut in_body = false;

    for line in lines {
        if line.is_empty() {
            in_body = true;
            continue;
        }
        if in_body {
            body.push_str(line);
            body.push('\n');
            continue;
        }
        let lower = line.to_lowercase();
        if let Some(from) = line.strip_prefix("From:") {
            from_user = extract_sip_user(from);
        } else if let Some(to) = line.strip_prefix("To:") {
            to_user = extract_sip_user(to);
        } else if let Some(c) = lower.strip_prefix("cseq:") {
            cseq = c
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("0")
                .parse()
                .unwrap_or(0);
        } else if let Some(ci) = line.strip_prefix("Call-ID:") {
            call_id = ci.trim().to_string();
        }
    }

    Some(SipRequest {
        method,
        from_user,
        to_user,
        cseq,
        body,
        call_id,
    })
}

/// 从头字段提取 SIP user（`From: <sip:3402xxx@domain>` 或 `From: "name" <sip:...>`）。
fn extract_sip_user(header: &str) -> String {
    let header = header.trim();
    if let Some(start) = header.find("sip:") {
        let after = &header[start + 4..];
        let end = after.find(['@', '>', ';']).unwrap_or(after.len());
        return after[..end].to_string();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 解析 REGISTER 请求。
    #[test]
    fn parse_register_request() {
        let raw = "REGISTER sip:34020000002000000001@192.168.1.10 SIP/2.0\r\n\
                   Via: SIP/2.0/UDP 192.168.1.64:5060\r\n\
                   From: <sip:34020000001320000001@192.168.1.64>\r\n\
                   To: <sip:34020000002000000001@192.168.1.10>\r\n\
                   Call-ID: abc123\r\n\
                   CSeq: 1 REGISTER\r\n\r\n";
        let req = parse_request(raw).unwrap();
        assert_eq!(req.method, SipMethod::Register);
        assert_eq!(req.from_user, "34020000001320000001");
        assert_eq!(req.to_user, "34020000002000000001");
        assert_eq!(req.call_id, "abc123");
        assert_eq!(req.cseq, 1);
    }

    /// INVITE 点播 → 200 OK 带 SDP，o= 行平台 ID 正确。
    #[test]
    fn invite_processor_returns_sdp() {
        let proc = InviteProcessor {
            local_domain: "192.168.1.10".into(),
        };
        let sdp = "v=0\r\no=34020000001320000001 0 0 IN IP4 192.168.1.64\r\ns=play\r\nc=IN IP4 192.168.1.64\r\nm=video 554 RTP/AVP 96";
        let raw = format!(
            "INVITE sip:34020000002000000001@192.168.1.10 SIP/2.0\r\n\
             From: <sip:34020000001320000001@192.168.1.64>\r\n\
             To: <sip:34020000002000000001@192.168.1.10>\r\n\
             Call-ID: inv1\r\n\
             CSeq: 2 INVITE\r\n\
             Content-Type: application/sdp\r\n\r\n{sdp}"
        );
        let req = parse_request(&raw).unwrap();
        let resp = proc.process(&req).unwrap();
        assert!(resp.starts_with("SIP/2.0 200 OK"));
        assert!(resp.contains("o=34020000001320000001"));
        assert!(resp.contains("s=play"), "点播应 s=play");
        assert!(resp.contains("m=video"));
    }

    /// INVITE 回放 → 200 OK s=playback。
    #[test]
    fn invite_playback_returns_playback_sdp() {
        let proc = InviteProcessor {
            local_domain: "192.168.1.10".into(),
        };
        let sdp = "v=0\r\no=34020000001320000001 0 0 IN IP4 192.168.1.64\r\ns=playback\r\nc=IN IP4 192.168.1.64\r\nm=video 0 RTP/AVP 96\r\na=start=20260919080000\r\na=end=20260919083000";
        let raw = format!(
            "INVITE sip:34020000002000000001@192.168.1.10 SIP/2.0\r\n\
             From: <sip:34020000001320000001@192.168.1.64>\r\n\
             To: <sip:34020000002000000001@192.168.1.10>\r\n\
             Call-ID: inv2\r\n\
             CSeq: 3 INVITE\r\n\r\n{sdp}"
        );
        let req = parse_request(&raw).unwrap();
        let resp = proc.process(&req).unwrap();
        assert!(resp.contains("s=playback"), "回放应 s=playback");
    }

    /// MESSAGE Keepalive → 200 OK（心跳保活）。
    #[test]
    fn message_keepalive_returns_ok() {
        let proc = MessageProcessor;
        let xml = "<?xml version=\"1.0\"?><Notify><CmdType>Keepalive</CmdType><SN>1</SN></Notify>";
        let raw = format!(
            "MESSAGE sip:34020000002000000001@192.168.1.10 SIP/2.0\r\n\
             From: <sip:34020000001320000001@192.168.1.64>\r\n\
             To: <sip:34020000002000000001@192.168.1.10>\r\n\
             Call-ID: msg1\r\n\
             CSeq: 4 MESSAGE\r\n\r\n{xml}"
        );
        let req = parse_request(&raw).unwrap();
        assert_eq!(CmdType::from_xml(&req.body), CmdType::Keepalive);
        let resp = proc.process(&req).unwrap();
        assert!(resp.starts_with("SIP/2.0 200 OK"));
    }

    /// 观察者分发：INVITE 有处理器，REGISTER 无（骨架阶段）。
    #[test]
    fn observer_dispatch() {
        let mut obs = SipObserver::new();
        obs.register(Box::new(InviteProcessor {
            local_domain: "192.168.1.10".into(),
        }));
        obs.register(Box::new(MessageProcessor));
        assert!(obs.has_handler(SipMethod::Invite));
        assert!(obs.has_handler(SipMethod::Message));
        assert!(
            !obs.has_handler(SipMethod::Register),
            "骨架阶段 REGISTER 未注册"
        );
    }
}
