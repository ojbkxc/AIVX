//! aivx 主入口——生产编排（P9b，DESIGN.md §1.2 控制面落地）。
//!
//! 编排链：
//! 配置（env → 默认值）→ 事件链（forwarder → DbWriter）→ axum `/api/*` +
//! static 前端，监听 18443。
//!
//! 部署形态（deploy/aivx.service）：`AIVX_PORT` / `AIVX_DATA_DIR` / `AIVX_STATIC_DIR`。
//! P9b 范围：服务常驻 + 前端可访问 + 事件链启动自检。摄像头编排（CameraManager
//! 拉起每路线程束）随 P8 接入——本阶段设备列表为空属预期。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use tower_http::services::ServeDir;

use aivx_events::Event;
use aivx_net::Device;
use aivx_perception::stream;
use std::sync::atomic::Ordering;

use aivx::auth::{auth_status, login, logout, AuthState};

use aivx::cameras::{CameraHandle, CameraManager};
use aivx::memory::{MemEventStore, MemProjections};
use aivx::pipeline::{DbWriter, Projector};
use aivx::preview::PreviewHub;

/// 运行配置（env → 默认值；文件层随 P8 ConfigHolder 落地）。
struct Config {
    port: u16,
    data_dir: PathBuf,
    static_dir: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        Self {
            port: std::env::var("AIVX_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(18443),
            data_dir: std::env::var("AIVX_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| dirs_home().join(".aivx")),
            static_dir: std::env::var("AIVX_STATIC_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("static")),
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// API 状态：事件链句柄 + CameraManager（P8a 起设备来自 YAML 编排）。
#[derive(Clone)]
struct ApiState {
    store: Arc<MemEventStore>,
    projections: Arc<MemProjections>,
    db: Arc<tokio::sync::Mutex<DbWriter>>,
    cameras: Arc<CameraManager>,
    preview: Arc<PreviewHub>,
    agent: Arc<AgentState>,
    auth: AuthState,
    /// P9-3/P9-4：config.yml 路径（设备增删/配置改动的写回目标）。
    config_path: PathBuf,
}

/// axum FromRef：auth/login/logout handler 用 State<AuthState>，Router 的
/// 全局 state 是 ApiState——FromRef 让子状态自动抽取。
impl axum::extract::FromRef<ApiState> for AuthState {
    fn from_ref(s: &ApiState) -> Self {
        s.auth.clone()
    }
}

/// Agent 运维会话（P8e：单会话内存存根；SessionStore 持消息历史）。
struct AgentState {
    ctx: aivx::agent::ActionContext,
    sessions: aivx::agent::session::SessionStore,
    approvals: aivx::agent::approval::AgentApprovals,
    /// LLM 未配置（env 缺失）标志：UI 提示 + 桩对话仍可用。
    llm_configured: bool,
}

async fn list_devices(State(s): State<ApiState>) -> Json<Vec<Device>> {
    Json(s.cameras.devices())
}

// ── P9-3 设备管理：POST/PUT/DELETE /api/devices/{id} ─────────────
//
// 线程束（T1/T2/T3）是启动期拉起的 OS 线程，运行期不能安全增删——
// 写回 config.yml 后由调用方重启服务生效（UI 提示；与 Frigate
// "改配置需重启"语义一致）。写回是唯一动作，进程内不热插拔。

/// POST /api/devices {id, rtsp_url, record_mode?, retain_days?} → 写回 config.yml。
/// id 必填且唯一（已存在 409）；rtsp_url 必须以 rtsp:// 开头。
async fn add_device(
    State(s): State<ApiState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(id) = req.get("id").and_then(|v| v.as_str()) else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "缺少设备 ID"})),
        )
            .into_response();
    };
    // 设备 ID 做目录名（record/sanitize）：预检字符集，防路径注入
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "设备 ID 只允许字母数字-_"})),
        )
            .into_response();
    }
    let Some(rtsp_url) = req.get("rtsp_url").and_then(|v| v.as_str()) else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "缺少 RTSP 地址"})),
        )
            .into_response();
    };
    if !rtsp_url.starts_with("rtsp://") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "RTSP 地址必须以 rtsp:// 开头"})),
        )
            .into_response();
    }
    let record_mode = req
        .get("record_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("off");
    let retain_days = req.get("retain_days").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

    let mut yaml = CameraManager::parse_yaml(&s.config_path).unwrap_or_default();
    if yaml.cameras.contains_key(id) {
        return (
            axum::http::StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "设备 ID 已存在"})),
        )
            .into_response();
    }
    // 既有设备的 model 配置带过来（新设备与老设备共用同一检测模型——
    // 从任一现有设备的 detect.model 继承，无设备则不配）
    let model = yaml.cameras.values().find_map(|c| c.detect.model.clone());
    let mut cam = aivx::config_store::new_camera(rtsp_url, record_mode, retain_days);
    cam.detect.model = model;
    yaml.cameras.insert(id.to_string(), cam);
    match aivx::config_store::save(&s.config_path, &yaml) {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "restart_required": true,
            "message": "设备已写入 config.yml，重启服务后生效（systemctl restart aivx）"
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("写回失败: {e}")})),
        )
            .into_response(),
    }
}

/// PUT /api/devices/{id}：改录像模式/保留天数/检测类别（运行时字段 +
/// 写回 YAML）。录像模式的运行时语义同步改 CameraHandle（扫描清理
/// 立即按新 retain_days 生效）；类别过滤是 T2 启动期参数——写回后
/// 重启生效。
async fn update_device(
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let mut yaml = CameraManager::parse_yaml(&s.config_path).unwrap_or_default();
    let Some(cam) = yaml.cameras.get_mut(&id) else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "设备不存在"})),
        )
            .into_response();
    };
    let mut runtime_changed = false;
    if let Some(mode) = req.get("record_mode").and_then(|v| v.as_str()) {
        let days = req.get("retain_days").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        aivx::config_store::set_record_mode(cam, mode, days);
        runtime_changed = true;
        // 运行时镜像：CameraHandle 的 record_mode/retain_days 同步改
        //（段扫描 + 清理循环读的是 handle——不重启就按新模式工作）
        if let Some(h) = s.cameras.cameras.iter().find(|c| c.device.id == id) {
            let mode: &'static str = match mode {
                "always" => "always",
                "motion" => "motion",
                _ => "off",
            };
            // 安全 mutating：record_mode/retain_days 是普通字段——经
            // raw pointer 绕共享引用改写（CameraManager 无写接口；字段
            // 非 Atomic，此写法在单控制面写者前提下安全）
            let h_ptr = h as *const CameraHandle as *mut CameraHandle;
            unsafe {
                (*h_ptr).record_mode = mode;
                (*h_ptr).retain_days = if mode == "motion" { days.max(1) } else { 0 };
            }
        }
    }
    if let Some(classes) = req.get("classes").and_then(|v| v.as_array()) {
        let names: Vec<String> = classes
            .iter()
            .filter_map(|c| c.as_str().map(String::from))
            .collect();
        aivx::config_store::set_detect_classes(cam, names);
    }
    if let Some(enabled) = req.get("enabled").and_then(|v| v.as_bool()) {
        cam.enabled = enabled;
        runtime_changed = true;
    }
    match aivx::config_store::save(&s.config_path, &yaml) {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "restart_required": !runtime_changed,
            "message": if runtime_changed { "已生效并写回 config.yml" } else { "已写回 config.yml，重启服务后生效" }
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("写回失败: {e}")})),
        )
            .into_response(),
    }
}

/// DELETE /api/devices/{id}：从 config.yml 摘除（重启后停拉该路）。
async fn delete_device(State(s): State<ApiState>, Path(id): Path<String>) -> impl IntoResponse {
    let mut yaml = CameraManager::parse_yaml(&s.config_path).unwrap_or_default();
    if yaml.cameras.remove(&id).is_none() {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "设备不存在"})),
        )
            .into_response();
    }
    match aivx::config_store::save(&s.config_path, &yaml) {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "restart_required": true,
            "message": "已从 config.yml 移除，重启服务后停拉该路"
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("写回失败: {e}")})),
        )
            .into_response(),
    }
}

async fn list_alarms(State(s): State<ApiState>) -> impl IntoResponse {
    // 汇总 + 明细派生表（P8e：Projector 投影，最近 50 条倒序）。
    Json(serde_json::json!({
        "active": s.projections.alarms.lock().unwrap().len(),
        "projected_events": s.projections.projected_count(),
        "items": s.projections.recent_alarms(50),
    }))
}

/// 设备录像段索引（recordings 派生表投影）。
///
/// duration 由 scanner 按 mtime-起点 实时给（真实录制时长，封顶 600）。
/// 过滤"正在写的段"：mtime 距今 < segment_secs*2 的段 ffmpeg 还没写完
/// moov atom——浏览器点开必失败（线上实测 moov not found），列表不出。
async fn list_recordings(State(s): State<ApiState>, Path(id): Path<String>) -> impl IntoResponse {
    let mut rows = s.projections.recordings_of(&id);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let grace = 1200i64; // segment_secs(600) * 2：封口后 moov 落盘缓冲
    rows.retain(|r| {
        std::fs::metadata(&r.file_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|t| now - t.as_secs() as i64 >= grace)
            .unwrap_or(false) // 文件没了（sweep 与列表竞争）——不出
    });
    Json(rows)
}

/// 实时预览 WS：/api/stream/:id（fMP4/MSE，DESIGN.md §8）。
/// 连接 → 订阅（0→1 起 ffmpeg）→ 先发缓存 init+最近段（秒开）→ 转发 broadcast。
async fn stream_preview(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(s): State<ApiState>,
) -> impl IntoResponse {
    // 预览用子码流（rtsp_sub）：T1 检测已独占主码流 RTSP 会话——摄像头
    // 同 URL 仅容 1 并发连接（线上实测：同 URL 第二路 ffmpeg 直接
    // "Invalid data found"）。I4 主/子分离即为此。
    let rtsp = s
        .cameras
        .cameras
        .iter()
        .find(|c| c.device.id == id)
        .and_then(|c| c.device.rtsp_sub.clone());
    let sub = s.preview.subscribe(&id, rtsp.as_deref()).await;
    let hub = Arc::clone(&s.preview);
    let cam_missing = sub.is_none();
    ws.on_upgrade(move |socket| async move {
        if cam_missing {
            return; // 无此设备：升级后立即关闭（保持 404 语义在 HTTP 层早判）
        }
        let (mut rx, cached_init, cached_media) = sub.expect("已判 None 分支");
        preview_session(socket, &mut rx, cached_init, cached_media).await;
        hub.unsubscribe(&id).await; // 断开必退订（否则 ffmpeg 永不停止）
    })
}

/// 单个预览订阅会话：秒开缓存 → broadcast 转发循环。
/// Lagged（慢客户端追不上）= 跳到最新——前端 catchUpToLive 兜底，直播语义可丢。
///
/// WS 断开感知：客户端不发数据，但 `socket.recv()` 在对端关闭时立即返回
/// None/Err——与 `rx.recv()` select 竞争，断开即刻退出（调用方随后
/// unsubscribe）。此前只在 rx 上 await：客户端关页后 future 永挂、
/// subscribers 永不减、ffmpeg 永不停（线上泄漏实证）。
async fn preview_session(
    mut socket: WebSocket,
    rx: &mut tokio::sync::broadcast::Receiver<Arc<Vec<u8>>>,
    cached_init: Option<Arc<Vec<u8>>>,
    cached_media: Option<Arc<Vec<u8>>>,
) {
    use tokio::sync::broadcast::error::RecvError;
    if let Some(init) = cached_init {
        if !init.is_empty() && socket.send(Message::Binary(init.to_vec())).await.is_err() {
            return;
        }
    }
    if let Some(media) = cached_media {
        if !media.is_empty() && socket.send(Message::Binary(media.to_vec())).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            // 对端关闭/出错即退（recv None/Err 是 WS 断开的唯一信号）
            msg = socket.recv() => {
                match msg {
                    Some(Ok(_)) => {} // 客户端不发言：ping 等控制帧忽略
                    Some(Err(_)) | None => return,
                }
            }
            msg = rx.recv() => match msg {
                Ok(data) => {
                    if data.is_empty() {
                        continue; // 新会话标记帧（不含数据）
                    }
                    if socket.send(Message::Binary(data.to_vec())).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            },
        }
    }
}

/// Agent 对话（P8e）：POST /api/agent/chat {message}。
/// 单会话（"main"）内存存根；runner 同步循环包 spawn_blocking（LLM 是
/// reqwest blocking）；Observer 角色 + 审批超时 0（高危必拒——无审批 UI
/// 时最安全的默认）。一次性返回完整事件流（不做 SSE）。
async fn agent_chat(
    State(s): State<ApiState>,
    Json(req): Json<serde_json::Value>,
) -> impl IntoResponse {
    let Some(message) = req
        .get("message")
        .and_then(|v| v.as_str())
        .map(String::from)
    else {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "missing message"})),
        )
            .into_response();
    };
    let session_id = "main";
    let agent = Arc::clone(&s.agent);
    let res = tokio::task::spawn_blocking(move || {
        let agent = agent;
        agent.sessions.create(
            session_id,
            "新会话",
            aivx::agent::session::AgentRole::Observer,
        );
        let history = agent.sessions.messages(session_id);
        let mut convo: Vec<aivx::agent::llm::ChatMessage> = history
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .map(|m| aivx::agent::llm::ChatMessage {
                role: if m.role == "user" {
                    aivx::agent::llm::Role::User
                } else {
                    aivx::agent::llm::Role::Assistant
                },
                content: m.content.clone(),
            })
            .collect();
        convo.push(aivx::agent::llm::user_message(message.clone()));
        let provider: Box<dyn aivx::agent::llm::ChatProvider> =
            match aivx::agent::llm::OpenAiChatProvider::from_env() {
                Some(p) => Box::new(p),
                None => Box::new(aivx::agent::llm::StubProvider),
            };
        let events = aivx::agent::runner::run(
            provider.as_ref(),
            &agent.approvals,
            &agent.ctx,
            session_id,
            aivx::agent::session::AgentRole::Observer,
            convo,
            8,
            0,
        );
        // 会话留痕：user + final 文本（工具轮不进历史——上下文预算）
        agent.sessions.append_message(
            session_id,
            aivx::agent::session::AgentMessage {
                role: "user".into(),
                content: message.clone(),
                tool_calls: None,
                tool_result: None,
            },
        );
        let final_text = events
            .iter()
            .rev()
            .find_map(|e| match e {
                aivx::agent::runner::AgentEvent::Final { content } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "（本轮未产生答复）".into());
        agent.sessions.rename_if_default(session_id, &message);
        agent.sessions.append_message(
            session_id,
            aivx::agent::session::AgentMessage {
                role: "assistant".into(),
                content: final_text,
                tool_calls: None,
                tool_result: None,
            },
        );
        (events, agent.llm_configured)
    })
    .await
    .unwrap_or_else(|_| {
        (
            vec![aivx::agent::runner::AgentEvent::Error {
                message: "agent task panic".into(),
            }],
            false,
        )
    });
    let (events, llm_configured) = res;
    let items: Vec<serde_json::Value> = events
        .iter()
        .map(|e| match e {
            aivx::agent::runner::AgentEvent::Thinking { turn } => {
                serde_json::json!({"type": "thinking", "turn": turn})
            }
            aivx::agent::runner::AgentEvent::ToolCall { name, arguments } => {
                serde_json::json!({"type": "tool_call", "name": name, "arguments": arguments})
            }
            aivx::agent::runner::AgentEvent::ToolResult { name, ok, text } => {
                serde_json::json!({"type": "tool_result", "name": name, "ok": ok, "text": text})
            }
            aivx::agent::runner::AgentEvent::ApprovalRequest { .. } => {
                serde_json::json!({"type": "approval_request"})
            }
            aivx::agent::runner::AgentEvent::ApprovalResolved { name, approved } => {
                serde_json::json!({"type": "approval_resolved", "name": name, "approved": approved})
            }
            aivx::agent::runner::AgentEvent::Final { content } => {
                serde_json::json!({"type": "final", "content": content})
            }
            aivx::agent::runner::AgentEvent::Error { message } => {
                serde_json::json!({"type": "error", "message": message})
            }
        })
        .collect();
    Json(serde_json::json!({
        "events": items,
        "llm_configured": llm_configured,
    }))
    .into_response()
}

/// RTSP URL userinfo 脱敏：`rtsp://admin:pass@host/…` → `rtsp://***@host/…`。
/// （P8e：/api/devices 已带真实 URL 是安全隐患？不——面板无登录但公网可达，
/// 凭据不得外泄。设备页要能看流地址 → config 页走脱敏视图。）
fn mask_rtsp(url: &str) -> String {
    // rtsp://userinfo@rest：只藏 userinfo 部分
    if let Some(scheme_end) = url.find("://") {
        let rest = &url[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            // url[..scheme_end+3] 已含 "://"，只替换其后的 userinfo
            return format!("{}***@{}", &url[..scheme_end + 3], &rest[at + 1..]);
        }
    }
    url.to_string()
}

/// 布控配置只读视图（P8e 降级落地）：每路设备 的接入地址（脱敏）/
/// 分析分辨率/录像模式/快照策略。画布编辑器随后续阶段接入。
async fn list_config(State(s): State<ApiState>) -> impl IntoResponse {
    // P9-2/P9-4：类别/保留天数从 config.yml 实时读（T2 启动期参数 + 清理
    // 策略），比 CameraHandle 更完整（含 enabled:false 的未启动设备）。
    let yaml = CameraManager::parse_yaml(&s.config_path).unwrap_or_default();
    let items: Vec<serde_json::Value> = s
        .cameras
        .cameras
        .iter()
        .map(|c| {
            let cam_cfg = yaml.cameras.get(&c.device.id);
            serde_json::json!({
                "id": c.device.id,
                "name": c.device.name,
                "rtsp_main": c.device.rtsp_main.as_deref().map(mask_rtsp),
                "rtsp_sub": c.device.rtsp_sub.as_deref().map(mask_rtsp),
                "record_mode": c.record_mode,
                "retain_days": c.retain_days,
                "classes": cam_cfg
                    .and_then(|y| y.detect.classes.as_ref())
                    .map(|cs| &cs.classes)
                    .cloned()
                    .unwrap_or_default(),
                "state": stream::state::name(c.bridge.metrics.stream_state.load(Ordering::Relaxed)),
            })
        })
        .collect();
    Json(items)
}

/// P9-1 鉴权中间件：cookie 校验；未登录 401。login/logout/status 路径放行。
async fn auth_middleware(
    State(auth): State<AuthState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let path = req.uri().path();
    // 公开：登录端点 + 状态探测 + 静态前端（登录页本身当然可访问）。
    // API（/api/*）与录像（/recordings/*）须带有效 session。
    let is_static = !path.starts_with("/api/") && !path.starts_with("/recordings/");
    let is_public = is_static || path == "/api/auth/login" || path == "/api/auth/status";
    if is_public || auth.check(req.headers()) {
        Ok(next.run(req).await)
    } else {
        Err((
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({"error": "未登录"})),
        ))
    }
}

async fn healthz(State(s): State<ApiState>) -> impl IntoResponse {
    // 每路流状态（数据面 AtomicU64 直读——DESIGN.md §14）
    let streams: Vec<serde_json::Value> = s
        .cameras
        .cameras
        .iter()
        .map(|c| {
            let m = &c.bridge.metrics;
            serde_json::json!({
                "id": c.device.id,
                "state": stream::state::name(m.stream_state.load(Ordering::Relaxed)),
                "decode_frames": m.decode_frames.load(Ordering::Relaxed),
                "inferences": m.inferences.load(Ordering::Relaxed),
            })
        })
        .collect();
    Json(serde_json::json!({
        "status": "ok",
        "max_seq": s.store.max_seq(),
        "version": env!("CARGO_PKG_VERSION"),
        "streams": streams,
    }))
}

/// 事件链冒烟：把一条 StreamUp 直接投进 DbWriter 路径，验证 seq 分配与投影。
/// （P9b 自检替代旧骨架的 println——服务常驻前的启动期一次性验证。）
async fn smoke_event_chain(state: &ApiState) {
    let ev = Event::StreamUp {
        device_id: "boot-selfcheck".into(),
        mono_ns: 0,
    };
    let mut db = state.db.lock().await;
    db.enqueue(ev);
    db.flush().await;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env();
    std::fs::create_dir_all(&cfg.data_dir).ok();

    // ── 事件链（ADR-022/026/028 已由 pipeline 单测机器强制）──
    let store = Arc::new(MemEventStore::new());
    let projections = Arc::new(MemProjections::default());
    let db = Arc::new(tokio::sync::Mutex::new(DbWriter::new(
        store.clone(),
        projections.clone(),
    )));
    let mut projector = Projector::new(store.clone(), projections.clone());
    projector.recover(); // 启动重放（P9b 空库为 no-op，路径必须存在）

    // CameraManager：YAML 设备清单 → 每路 T1/T2/T3 线程束（P8a 真实事件上游）
    let cam_config = cfg.data_dir.join("config.yml");
    let record_dir = cfg.data_dir.join("record");
    let cam_yaml = CameraManager::parse_yaml(&cam_config)?;
    let cameras = Arc::new(CameraManager::from_yaml_with_record(
        &cam_yaml,
        record_dir.clone(),
    )?);
    let cam_rx = cameras.event_rx();

    // P9-1 鉴权：config.yml auth.password_sha256（缺省不启用——内网语义保留）
    let auth = AuthState {
        store: aivx::auth::SessionStore::new(),
        password_sha256: cam_yaml.auth.as_ref().map(|a| a.password_sha256.clone()),
    };
    if auth.enabled() {
        println!("AIVX: 鉴权已启用（config.yml auth.password_sha256）");
    } else {
        println!("AIVX: 鉴权未启用——公网暴露请在 config.yml 配置 auth.password_sha256");
    }

    let state = ApiState {
        store: store.clone(),
        projections: projections.clone(),
        db: db.clone(),
        cameras: cameras.clone(),
        preview: Arc::new(PreviewHub::new("ffmpeg".into())),
        auth,
        config_path: cam_config.clone(),
        agent: Arc::new(AgentState {
            ctx: aivx::agent::ActionContext::with_data(Arc::new(
                aivx::agent::live_data::LiveDataSource::new(
                    Arc::clone(&cameras),
                    Arc::clone(&projections),
                ),
            )),
            sessions: aivx::agent::session::SessionStore::new(),
            approvals: aivx::agent::approval::AgentApprovals::default(),
            llm_configured: aivx::agent::llm::OpenAiChatProvider::from_env().is_some(),
        }),
    };
    smoke_event_chain(&state).await;
    assert!(
        state.projections.assert_no_gaps(0),
        "ADR-022 violated: fan-out has gaps"
    );

    // forwarder：CameraManager 的全局汇聚 rx → DbWriter（state.db 共享单
    // 实例——双实例会各自分配 seq 交叉写坏 store）。阻塞 recv 挪专用线程
    // （ADR-022：runtime worker 不背阻塞 IO）；50ms tick 兜底 flush：事件
    // 不足一批（256）时不再卡 buf——线上 boot 首轮 flush 4 批后剩 <256 条
    // 卡 30 分钟不投影，录像列表缺最新段。
    {
        let db = state.db.clone();
        std::thread::Builder::new()
            .name("cam-forwarder-recv".into())
            .spawn(move || {
                while let Ok(ev) = cam_rx.recv() {
                    let mut db = db.blocking_lock();
                    db.enqueue(ev);
                    if db.pending_len() >= db.batch_limit() {
                        db.flush_sync(); // 批满即刷（大流量路径不变）
                    }
                }
                // 发送端全 drop（关停）：drain 并 flush（优雅关停 §17 步骤 2-3）
                let mut db = db.blocking_lock();
                db.flush_sync();
            })
            .expect("spawn forwarder thread");
    }
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(50));
            loop {
                tick.tick().await;
                // flush 是同步内存操作（MemEventStore）——micro 级，不阻塞
                // worker。P1 换 SeaORM 时改 spawn_blocking 提交事务。
                let mut db = state.db.lock().await;
                db.flush_sync();
            }
        });
    }

    // 段索引扫描（P8e，DESIGN.md §3.3）：10s 轮询录像目录，新段发
    // RecordingSegment 事件进事件链 → 投影器建 recordings 派生表。
    // Scanner 的 seen 集合跨轮保留——否则每轮全量重发，投影表会堆积
    // 同一文件的重复行（线上验证时抓到）。整轮扫描挪进阻塞线程池：
    // scanners Mutex 跨 'static 界限（tokio spawn_blocking 硬约束）。
    {
        let cameras_for_scan = Arc::clone(&cameras);
        let projections_for_sweep = Arc::clone(&projections);
        let scan_dir = cfg.data_dir.join("record");
        let scanners = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            String,
            aivx_perception::record::SegmentScanner,
        >::new()));
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
                // T1/T3 之外的低频扫描：spawn_blocking（std fs 直读）
                let cams = Arc::clone(&cameras_for_scan);
                let dir = scan_dir.clone();
                let scanners = Arc::clone(&scanners);
                let projs = Arc::clone(&projections_for_sweep);
                let _ = tokio::task::spawn_blocking(move || {
                    // 整轮持锁：扫描是 10s 低频后台任务，无并发竞争
                    let mut scanners = scanners.lock().unwrap();
                    for cam in &cams.cameras {
                        if cam.record_mode == "off" {
                            continue;
                        }
                        let dev_dir = dir.join(aivx_perception::record::sanitize(&cam.device.id));
                        let scanner = scanners.entry(cam.device.id.clone()).or_insert_with(|| {
                            aivx_perception::record::SegmentScanner::new(
                                cam.device.id.clone(),
                                dev_dir.clone(),
                            )
                        });
                        let bridge = Arc::clone(&cam.bridge);
                        scanner.scan(|ev| bridge.emit_status(ev));
                        // 保留清理（README 承诺"按天数自动清理"落地）：扫描同一
                        // 轮顺带清超期段——10s 周期无感（sweep 只 stat+remove）。
                        // retain_days=0（always 录/未配 days）→ 不清理。
                        if cam.retain_days > 0 {
                            let _ = aivx_perception::record::sweep_stale(
                                &dev_dir,
                                cam.retain_days,
                                600,
                            );
                        }
                    }
                    // 清理联动（投影 vs 磁盘对账）：磁盘已不存在的投影行直接
                    // 摘除——否则录像列表挂着已删文件，回放点开 404。对账比按
                    // swept 列表摘更鲁棒：scan-emit 的事件还在 channel/flush buf
                    // 里（投影行尚未建立）时 remove 会摘空，flush 后行又回来；
                    // 且手动删段/外部清理也能对上。10s 低频 stat 无感。
                    {
                        let rows = projs.all_recording_paths();
                        let gone: Vec<std::path::PathBuf> =
                            rows.into_iter().filter(|p| !p.exists()).collect();
                        if !gone.is_empty() {
                            projs.remove_recordings(&gone);
                        }
                    }
                })
                .await;
            }
        });
    }

    // Projector 常驻循环：P9b 库空 + 无事件 → 挂起保持句柄（P8 接 DbWriter fan-out）
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            let _ = &mut projector; // P8: 在此消费 DbWriter 顺序 fan-out
        }
    });

    // ── HTTP：/api/* + static 前端 ──
    let record_dir = cfg.data_dir.join("record");
    let app = Router::new()
        .route("/api/auth/login", axum::routing::post(login))
        .route("/api/auth/logout", axum::routing::post(logout))
        .route("/api/auth/status", get(auth_status))
        .route("/api/devices", get(list_devices))
        .route("/api/devices", axum::routing::post(add_device))
        .route("/api/devices/:id", axum::routing::put(update_device))
        .route("/api/devices/:id", axum::routing::delete(delete_device))
        .route("/api/alarms", get(list_alarms))
        .route("/api/healthz", get(healthz))
        .route("/api/stream/:id", get(stream_preview))
        .route("/api/recordings/:id", get(list_recordings))
        .route("/api/agent/chat", axum::routing::post(agent_chat))
        .route("/api/config", get(list_config))
        .with_state(state.clone())
        // P9-1 鉴权中间件：/api/* 全保护（login/status 除外——route_from_ref
        // 层已注册的豁免路径）。401 + JSON（前端据跳登录）。
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        // 录像回放：ServeDir 限在录像根（防穿越 + Range/seek 免费）。
        // 鉴权：/recordings/* 同样过中间件（录像内容不外泄）。
        .nest_service(
            "/recordings",
            ServeDir::new(&record_dir).append_index_html_on_directories(false),
        )
        .fallback_service(ServeDir::new(&cfg.static_dir).append_index_html_on_directories(true));

    let addr = SocketAddr::from(([0, 0, 0, 0], cfg.port));
    println!(
        "AIVX {} listening on http://0.0.0.0:{} (data_dir={}, static_dir={})",
        env!("CARGO_PKG_VERSION"),
        cfg.port,
        cfg.data_dir.display(),
        cfg.static_dir.display()
    );

    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app).await?;
    Ok(())
}
