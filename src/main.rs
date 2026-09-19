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

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use tower_http::services::ServeDir;

use aivx_events::Event;
use aivx_net::Device;
use aivx_perception::stream;
use std::sync::atomic::Ordering;

use aivx::memory::{MemEventStore, MemProjections};
use aivx::pipeline::{forwarder, DbWriter, Projector};

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
    cameras: Arc<cameras::CameraManager>,
}

async fn list_devices(State(s): State<ApiState>) -> Json<Vec<Device>> {
    Json(s.cameras.devices())
}

async fn list_alarms(State(s): State<ApiState>) -> impl IntoResponse {
    // 活跃报警数 + 最近事件数——P9b 用投影计数，P8 换派生表查询。
    Json(serde_json::json!({
        "active": s.projections.alarms.lock().unwrap().len(),
        "projected_events": s.projections.projected_count(),
    }))
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
async fn main() {
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
    let cameras = Arc::new(cameras::CameraManager::load_yaml(&cam_config, record_dir)?);
    let cam_rx = cameras.event_rx();

    let state = ApiState {
        store: store.clone(),
        projections: projections.clone(),
        db: db.clone(),
        cameras: cameras.clone(),
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

    // Projector 常驻循环：P9b 库空 + 无事件 → 挂起保持句柄（P8 接 DbWriter fan-out）
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            let _ = &mut projector; // P8: 在此消费 DbWriter 顺序 fan-out
        }
    });

    // ── HTTP：/api/* + static 前端 ──
    let app = Router::new()
        .route("/api/devices", get(list_devices))
        .route("/api/alarms", get(list_alarms))
        .route("/api/healthz", get(healthz))
        .with_state(state)
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
    axum::serve(listener, app).await.expect("serve");
}
