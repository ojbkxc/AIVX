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

use aivx::cameras::CameraManager;
use aivx::memory::{MemEventStore, MemProjections};
use aivx::pipeline::{forwarder, DbWriter, Projector};
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
}

async fn list_devices(State(s): State<ApiState>) -> Json<Vec<Device>> {
    Json(s.cameras.devices())
}

async fn list_alarms(State(s): State<ApiState>) -> impl IntoResponse {
    // 汇总 + 明细派生表（P8e：Projector 投影，最近 50 条倒序）。
    Json(serde_json::json!({
        "active": s.projections.alarms.lock().unwrap().len(),
        "projected_events": s.projections.projected_count(),
        "items": s.projections.recent_alarms(50),
    }))
}

/// 设备录像段索引（recordings 派生表投影）。duration 按段序差分补
/// （T3 段固定 600s；末段开放中，按 mtime 推）。
async fn list_recordings(State(s): State<ApiState>, Path(id): Path<String>) -> impl IntoResponse {
    let mut rows = s.projections.recordings_of(&id);
    let seg = 600i64;
    for i in 0..rows.len() {
        if rows[i].duration_secs == 0.0 {
            // 差分：下段 start - 本段 start；末段按文件 mtime 与 start 的差
            let start = rows[i].start_ts;
            let end = rows
                .get(i + 1)
                .map(|r| r.start_ts)
                .unwrap_or_else(|| {
                    std::fs::metadata(&rows[i].file_path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(start)
                });
            let d = (end - start).max(0);
            rows[i].duration_secs = if d == 0 { 0.0 } else { d.min(seg) as f64 };
        }
    }
    Json(rows)
}

/// 实时预览 WS：/api/stream/:id（fMP4/MSE，DESIGN.md §8）。
/// 连接 → 订阅（0→1 起 ffmpeg）→ 先发缓存 init+最近段（秒开）→ 转发 broadcast。
async fn stream_preview(
    ws: WebSocketUpgrade,
    Path(id): Path<String>,
    State(s): State<ApiState>,
) -> impl IntoResponse {
    let rtsp = s
        .cameras
        .cameras
        .iter()
        .find(|c| c.device.id == id)
        .and_then(|c| c.device.rtsp_main.clone());
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
async fn preview_session(
    mut socket: WebSocket,
    rx: &mut tokio::sync::broadcast::Receiver<Arc<Vec<u8>>>,
    cached_init: Option<Arc<Vec<u8>>>,
    cached_media: Option<Arc<Vec<u8>>>,
) {
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
        match rx.recv().await {
            Ok(msg) => {
                if msg.is_empty() {
                    continue; // 新会话标记帧（不含数据）
                }
                if socket.send(Message::Binary(msg.to_vec())).await.is_err() {
                    return;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
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
    let cameras = Arc::new(CameraManager::load_yaml(&cam_config, record_dir)?);
    let cam_rx = cameras.event_rx();

    let state = ApiState {
        store: store.clone(),
        projections: projections.clone(),
        db: db.clone(),
        cameras: cameras.clone(),
        preview: Arc::new(PreviewHub::new("ffmpeg".into())),
    };
    smoke_event_chain(&state).await;
    assert!(
        state.projections.assert_no_gaps(0),
        "ADR-022 violated: fan-out has gaps"
    );

    // forwarder：CameraManager 的全局汇聚 rx → DbWriter。
    let fstore = store.clone();
    let fproj = projections.clone();
    tokio::spawn(async move {
        // spawn_blocking：forwarder 的 recv 是阻塞式（std mpsc）——控制面
        // runtime 不背阻塞 IO（ADR-022 链路保持不变）
        forwarder(cam_rx, DbWriter::new(fstore, fproj), None).await;
    });

    // 段索引扫描（P8e，DESIGN.md §3.3）：10s 轮询录像目录，新段发
    // RecordingSegment 事件进事件链 → 投影器建 recordings 派生表。
    {
        let cameras_for_scan = Arc::clone(&cameras);
        let scan_dir = cfg.data_dir.join("record");
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
            loop {
                interval.tick().await;
                // T1/T3 之外的低频扫描：spawn_blocking（std fs 直读）
                let cams = Arc::clone(&cameras_for_scan);
                let dir = scan_dir.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    for cam in &cams.cameras {
                        if cam.record_mode == "off" {
                            continue;
                        }
                        let dev_dir = dir.join(aivx_perception::record::sanitize(&cam.device.id));
                        let mut scanner = aivx_perception::record::SegmentScanner::new(
                            cam.device.id.clone(),
                            dev_dir,
                        );
                        let bridge = Arc::clone(&cam.bridge);
                        scanner.scan(|ev| bridge.emit(ev));
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
        .route("/api/devices", get(list_devices))
        .route("/api/alarms", get(list_alarms))
        .route("/api/healthz", get(healthz))
        .route("/api/stream/:id", get(stream_preview))
        .route("/api/recordings/:id", get(list_recordings))
        .with_state(state)
        // 录像回放：ServeDir 限在录像根（防穿越 + Range/seek 免费）
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
