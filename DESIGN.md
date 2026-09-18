# AIVX — 架构设计蓝本（DESIGN.md）

> **定位**：AIVX（AI Video eXtended）高性能自托管 AI NVR 的架构权威蓝本。
> **标准**：每个决策必须回答四件事——**参照来源（具体文件）→ Rust 落地 → 为什么 → 反例教训**。
> **原则**：性能极致、延迟极致、设计极致。架构不变量用强约束锁死，不变量被违反即重构，而不是打补丁。

---

## 0. 架构不变量（Architectural Invariants）

这些是不可违反的强约束。任何代码评审违反其中一条，直接打回。

| # | 不变量 | 强制方式 |
|---|---|---|
| I1 | **帧路径 0 堆分配、0 拷贝、0 编解码**（直到 ort 输入前） | `FramePool` + 借用，CI benchmark 断言 |
| I2 | **分析循环绝不碰 DB** | 事件经 channel → 单写者批量落库 |
| I3 | **运动门控在检测前** | 无运动不跑 YOLO；有区域/越线布控时 `force_detect` |
| I4 | **分析用子码流，录像/监看用主码流** | 配置层面分离，禁止混淆 |
| I5 | **perception 不依赖 cognition/agent** | 模块编译隔离，无 LLM 配置也能完整运行 |
| I6 | **事件流是唯一事实源** | 所有表由事件流派生（append-only + 物化视图） |
| I7 | **单写者落库** | 全局唯一 DB writer task |
| I8 | **报警去重：状态变化才写** | 同目标同区域同规则仅在状态机跳变时触发 |
| I9 | **配置三层分离 + 热重载** | 默认值 / 文件 / env / 运行时，模块只读快照 |
| I10 | **DeviceAdapter 能力驱动** | "不支持"是数据不是异常，前端动态渲染 |

---

## 1. 总体架构

### 1.1 进程拓扑（对比参考项目）

```
┌────────────────────────────────────────────────────────────┐
│                    aivx 主进程（单二进制）                   │
│                                                            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐   │
│  │  api      │  │  agent    │  │ cognition │  │  notify   │  │
│  │  /api/*   │  │  AI 运维   │  │  LLM 复核  │  │ 推送      │  │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘   │
│  ┌────────────────────────────────────────────────────┐    │
│  │              event bus（tokio::broadcast）          │    │
│  │   Event{seq} → outbox(append-only) → 订阅者        │    │
│  │   DB writer / WS / notify / cognition / agent       │    │
│  └────────────────────────────────────────────────────┘    │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐   │
│  │ perception │  │  rules    │  │  track    │  │  motion   │  │
│  │ DetectorPool│  │ 规则引擎   │  │ ByteTrack  │  │  EMA      │  │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘   │
│  ┌────────────────────────────────────────────────────┐    │
│  │              frame pool（零拷贝帧池）                │    │
│  └────────────────────────────────────────────────────┘    │
│  ┌──────────┐  ┌──────────┐  ┌────────────────────┐        │
│  │ storage   │  │  config  │  │  db (SeaORM)        │        │
│  │ 录像/快照 │  │ 三层热重载│  │ events+物化视图     │        │
│  └──────────┘  └──────────┘  └────────────────────┘        │
└────────────────────────────────────────────────────────────┘
        │                       │
        ▼                       ▼
┌───────────────┐       ┌──────────────────────┐
│   aivx-net     │       │ ffmpeg / ZLMediaKit  │
│ onvif/rtsp/    │       │ （外部进程，托管生命周期）│
│ gb28181        │       └──────────────────────┘
└───────────────┘
```

**参照来源**：
- Frigate 的多进程拓扑（`frigate/app.py`）暴露了"多进程=不得已"的缺陷，AIVX 用单进程 + tokio task 消除它。
- Rebucca 的每路子进程 + JPEG 跨进程（`rebucca/app/analysis/remote_detector.py`）证明"跨进程传帧"是性能杀手，AIVX 完全避免。

**为什么**：Rust 无 GIL、无 GC，`tokio::task` + `Arc<Detector>` 单进程内并发，8 路共享 1 份模型，帧靠借用零拷贝，跨路批处理推理。

**Rust 落地**：

```rust
// main.rs —— 单进程编排（抄 AIGX src/main.rs 的 lib+bin 双 target）
#[tokio::main(flavor = "multi_thread", worker_threads = N)]
async fn main() {
    let cfg = ConfigHolder::load().await;          // 三层配置
    let (event_tx, _) = broadcast::channel(4096);  // 事件总线
    let pool = FramePool::new(...);                // 帧池
    let detectors = DetectorPool::new(&cfg.detect); // 共享推理
    let db_writer = DbWriter::spawn(event_rx);     // 单写者落库
    for cam in cfg.cameras() {
        tokio::spawn(camera_pipeline(cam, pool.clone(), detectors.clone(), event_tx.clone()));
    }
    axum::serve(router(...)).await?;
}
```

---

## 2. 事件溯源（Event Sourcing）—— 唯一事实源

### 2.1 为什么

**参照教训**：
- Rebucca 的事件散落在 `pipeline.py` 的 `_check_zones` 里，报警/轨迹/区域混在一个回调，靠 `build_alarm_context` 事后拼文案。事件没有唯一源，回放/补漏/跨模块一致都做不到。
- Frigate 有 Event/Timeline/ReviewSegment 三张表各管一段，事件语义分裂。
- ai-nvr 的 EventBus（`src/event-bus.ts`）类型安全但无持久化、无重放、无 seq。

### 2.2 设计

**事件流是唯一事实源（I6）**。分析线程只产生 `Event`，写入 append-only `events` 表（全局单调 seq）。其余所有表（alarm/track/recording 索引）都由事件流**物化视图**派生。

```rust
// src/event.rs —— 统一事件模型
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    // 感知层
    StreamUp { device_id: DeviceId, ts: i64 },
    StreamDown { device_id: DeviceId, reason: String, ts: i64 },
    TrackAppeared { track: Track },
    TrackDisappeared { track: Track },
    TrackEnteredZone { track: Track, zone: ZoneId },
    TrackLeftZone { track: Track, zone: ZoneId },
    // 规则引擎
    AlarmRaised { alarm: Alarm },        // 规则命中
    AlarmCleared { alarm_id: String },   // 状态机复位
    // 认知层
    InsightGenerated { alarm_id: String, insight: Insight }, // LLM 回填
    // 交互层
    AgentAction { action: AgentAction },
    // 系统
    DeviceAdded { device: Device }, DeviceRemoved { device_id: DeviceId },
    ConfigChanged { section: String, prev: Value, new: Value },
}
```

### 2.3 持久化 + 物化视图

```sql
-- 源表（append-only，单写者）
CREATE TABLE events (
  id        BIGSERIAL PRIMARY KEY,
  seq       BIGSERIAL UNIQUE NOT NULL,   -- 全局单调序号，可重放
  device_id BIGINT,
  type      TEXT NOT NULL,
  payload   JSONB NOT NULL,
  occurred_at TIMESTAMPTZ NOT NULL,
  created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_events_device_ts ON events (device_id, occurred_at DESC);

-- 物化视图（由 events 派生，供查询）
CREATE TABLE alarms ( ... );   -- 当前活跃报警
CREATE TABLE tracks  ( ... );  -- 目标轨迹（可检索）
```

**写路径**：分析线程 → `mpsc::channel(4096)` → 全局唯一 `DbWriter` 每 50ms/200 条批量 INSERT 事务（I7）。**分析循环绝不碰 DB（I2）**。

**读路径**：查询走物化视图，与写路径完全隔离。

**重放**：`events` 表按 seq 重放 → 重建任意时点的物化状态（调试 / 补漏 / 灾难恢复）。

**参照来源**：
- 抄 ai-nvr 的 EventBus 类型安全（`src/event-bus.ts`），但加上持久化 + seq + 重放。
- 抄 open-nvr 的 NATS 事件流思想（`open-nvr/nats/`），但用进程内 broadcast 替代，不引入分布式依赖。
- 抄 rebucca 的教训：它把"高频写库"当成血泪教训写进注释，AIVX 用事件溯源根治。

---

## 3. 帧路径（零拷贝）—— 性能天花板

### 3.1 为什么

**参照教训**：
- Rebucca 每帧 `cv2.VideoCapture.read()` 分配新 ndarray → `deque` 拷贝 → JPEG 编码跨进程 → JPEG 解码 → 坐标 rescale。每帧 ≥3 次堆分配、≥2 次拷贝、2 次编解码。
- Frigate 用共享内存（`frigate/video/ffmpeg.py` 的 `SharedMemoryFrameManager`），但 `frame_name` 管理复杂，跨进程仍要读写共享内存。
- ai-nvr 每帧 `sharp` 灰度化 + 推送 MJPEG。

### 3.2 设计：FramePool

```rust
// src/frame.rs —— 零拷贝帧池（I1）
pub struct FramePool {
    buffers: Box<[FrameBuffer]>,          // 预分配 NV12 缓冲（YUV420，无 BGR 转换）
    ready:   ArrayQueue<usize>,          // 生产者→消费者无锁队列（crossbeam）
    free:    ArrayQueue<usize>,          // 消费者→生产者归还队列
}
impl FramePool {
    pub fn acquire(&self) -> FrameSlot;   // 从 free 取，无则阻塞/等待
    pub fn publish(&self, slot: usize);   // 解码线程写入后发布
    pub fn recv(&self) -> Frame;          // 分析线程取，返回借用（带生命周期）
}
```

**帧生命周期**：

```
ffmpeg 子进程 stdout ──write──▶ FrameSlot[i]（预分配，0 拷贝）
                                  │ publish
                                  ▼
                          ready 队列（无锁）
                                  │ recv（借用 &Frame）
                                  ▼
                EMA 运动检测（只读借用，~2ms）
                                  │ 有运动?
                                  ▼
                DetectorPool（攒批 → ort 推理）
                                  │
                ByteTrack.update() → Track[]
                                  │
                规则引擎.match(track, zones) → Alarm?
                                  │
                Event{Alarm} → event bus
                                  │
                          分析结束，slot 归还 free 队列（0 拷贝复用）
```

**关键点**：
- 缓冲用 **NV12（YUV420）**，不转 BGR——ffmpeg 原生输出 NV12，ort 预处理可直接消费。避免 frigate/rebucca 的 BGR 中间态。
- 运动检测在**低分辨率**（320×180）跑，从 NV12 直接取 Y 平面（灰度），零额外分配。
- ort 输入预处理是**唯一一次**格式转换（NV12→RGB float），且用 `ort` 的 tensor 预分配复用，不每帧新建。

### 3.3 对比量化

| 指标 | Rebucca | Frigate | AIVX |
|---|---|---|---|
| 每帧堆分配 | ≥3 次 | 1 次（共享内存名管理） | **0 次** |
| 每帧拷贝 | ≥2 次 | 1 次（进共享内存） | **0 次** |
| 每帧编解码 | JPEG 编+解 | 0 | **0** |
| 格式 | BGR→BGR | RGB | **NV12 原生** |

### 3.4 实现注意

- `crossbeam::ArrayQueue` 无锁，但生产/消费是单对单，可用 `tokio::sync::mpsc` 替代更简单（帧是借用不跨 await 边界，用同步队列更安全）。
- **帧不跨 tokio task 移动**（借用不 Send），分析流水线内用同步函数链，不 await——保证零拷贝的同时避免借用跨 await 的复杂生命周期。若需要跨 task，用 `Arc<Frame>` 包一层（有代价，仅用于需要跨 task 的路径，如快照）。

---

## 4. 性能预算

### 4.1 目标

| 指标 | AIVX 目标 | 参考基线 |
|---|---|---|
| 单路 CPU（子码流分析） | < 0.15 核 | Rebucca ~0.5-1 核 |
| 8 路 CPU | < 1.5 核 | Frigate ~4-6 核 |
| 内存（8 路） | < 500MB（模型 1 份） | Rebucca ~2-3GB |
| 帧路径堆分配 | 0 次/帧 | Rebucca ≥3 次 |
| 检测延迟（运动→报警） | < 300ms | 取决于 analyze_fps |
| 录像 CPU | ≈0（-c copy） | Rebucca 有转码 |

### 4.2 单路预算拆解（1080P 子码流 640×360）

```
解码（子码流软解）        ~15-25%
EMA 运动检测（320×180）   ~5-8%
YOLOv8n 检测（640×640）   ~50-80ms/帧  ← 大头（仅运动时）
ByteTrack + 规则          ~1-2%
录像（-c copy）           ~2-5%（≈0 转码）
```

**关键结论**：检测是唯一大头 → 运动门控省 95% 检测；录像 `-c copy` 零转码；运动检测缩放到 320×180。

### 4.3 检测批处理

```rust
// src/perception/detect.rs —— 跨路批处理（可选，GPU 收益最大）
pub struct DetectorPool {
    session: Arc<ort::Session>,
    batch_queue: mpsc::Receiver<BatchReq>,
    batch_size: usize,        // 默认 8
    window_ms: Duration,      // 攒批窗口，默认 40ms
}
// 8 路同时有运动 → 8 张 640×640 合批 → 1 次 ort 推理
// 对比 Python 8 进程各送 8 次
```

---

## 5. 感知层（perception）

### 5.1 运动检测（EMA，抄 Frigate）

**参照来源**：`frigate-dev/frigate/motion/frigate_motion.py`（EMA 背景模型，不用 MOG2）。

```rust
// src/perception/motion.rs
pub struct EmaMotion {
    avg_frame: Vec<f32>,      // 背景模型（低分辨率）
    avg_delta: Vec<f32>,      // 运动平滑
    frame_count: u32,
}
// 前 30 帧建基线；absdiff → accumulateWeighted → threshold → dilate → 找轮廓
// 运动持续 10 帧才更新 avg_frame（避免运动目标被吸收进背景）
// 对比度拉伸（4/96 百分位）夜视降噪
```

**为什么比 MOG2 好**：Frigate 的 EMA 背景模型比 rebucca 的 OpenCV MOG2 更稳（光照渐变不误报）、更轻（纯数学，不依赖 OpenCV）。

### 5.2 目标检测（ort YOLO）

**参照来源**：`rebucca-main/rebucca/app/analysis/engines/base.py`（BaseEngine 抽象）+ `yolo_postprocess.py`（YOLO5/8/11/26 × detect/segment/classify/pose/obb 解码）。

```rust
// src/perception/detect.rs
#[async_trait]
pub trait Detector: Send + Sync {
    async fn detect(&self, frame: &Frame) -> Result<Vec<Detection>>;
    fn info(&self) -> EngineInfo;
}
pub struct OrtDetector { session: Arc<ort::Session>, /* ... */ }
// EngineFactory 按 algo.inference_engine 分发（抄 rebucca engines/factory.py）
// 输出 Detection { box, label, score, keypoints?, mask? }
```

### 5.3 跟踪（ByteTrack，抄 ai-nvr）

**参照来源**：`ai-nvr-main/src/ai/track-activity.ts` + `ai-nvr-main/src/detection/motion.ts`。ai-nvr 用 ByteTrack（IoU + min_hits=3 防幽灵 ID），比 rebucca 的简单 IoUTracker（`rebucca/app/analysis/tracker.py` 112 行）更稳。

```rust
// src/perception/track.rs
pub struct ByteTrack {
    tracks: HashMap<TrackId, Track>,
    iou_threshold: f32,   // 0.3
    min_hits: u8,         // 3：连续 N 帧命中才确认，防幽灵
    max_missed: u8,       // 8：N 帧失配则结束
}
pub struct Track { id: TrackId, label: String, box: [f32;4], score: f32, born: i64, ... }
```

### 5.4 规则引擎（数据驱动，抄 ai-nvr + rebucca）

**参照来源**：`ai-nvr-main/src/alert/engine.ts`（条件表达式 + 滑动窗口 + 动作）+ `ai-nvr-main/src/alert/window.ts` + `rebucca-main/rebucca/app/analysis/biz_rules.py`（5 种后处理的几何算法）。

```rust
// src/perception/rules.rs —— 规则 = 条件 + 窗口 + 动作（数据驱动，DB 存储）
pub struct Rule {
    id: String, name: String,
    when: Vec<Condition>,       // 条件表达式树
    window: Option<Window>,     // 滑动窗口聚合
    actions: Vec<Action>,       // notify / record / snapshot / ptz
    cooldown: Duration,         // 冷却（抄 rebucca 的 per-track/per-zone 冷却）
}
pub enum Condition {
    TrackInZone { zone: ZoneId },
    LabelIs { label: String },
    DwellGreaterThan { seconds: f64 },
    CrossedLine { line: LineId, direction: Direction },
    CountGreaterThan { n: u32 },
    // 几何核心抄 rebucca biz_rules.py：
    //   cross_line_direction（叉积判正/逆向）
    //   direction_match（atan2 角度窗）
    //   point_in_polygon（射线法）
}
```

**为什么比 rebucca 强**：rebucca 把 5 种后处理硬编码进 `_check_zones`，改规则要改代码。AIVX 规则 JSON 化，DB 存储 + 热更新，前端可视化配置。

---

## 6. 认知层（cognition）—— 可选 LLM

### 6.1 设计

**参照来源**：`frigate-event-handler-master/frigate_event_handler/daemon.py`（事件结束→抽帧→相似去重→网格→LLM→回写）+ `rebucca-main/rebucca/app/analysis/pipeline.py` 的 `_llm_verify_track`（flow2/3 复核 + 冷却）+ `ai-nvr-main/src/ai/multimodal-analyzer.ts`（触发节流）。

```rust
// src/cognition/mod.rs
pub struct CognitionService {
    providers: Arc<dyn GenAiProvider>,   // 插件化（抄 frigate genai/plugins）
    prompts: PromptRegistry,             // 集中管理（抄 frigate genai/prompts.py）
}

// 触发：事件总线订阅 AlarmRaised → 裁剪关键帧 → LLM → Insight
pub struct Insight {
    is_false_positive: bool,   // 误报判定（抄 rebucca llm_validate）
    threat_level: Level,       // 高/中/低（抄 frigate genai）
    scene: String,             // 场景描述："有人翻越北侧围墙"
    title: String,
    summary: String,
}
```

### 6.2 关键设计

- **只对报警候选调 LLM**，不对每帧调（抄 rebucca 的冷却：per-track 6s / per-zone 8s）。
- **裁剪目标区域**（`crop = frame[y1:y2, x1:x2]`），不送全图（抄 rebucca `_llm_verify_track`）。
- provider 走 **AIGX 网关**（复用你自己的渠道）。
- **provider 插件**：openai / ollama / gemini / llama_cpp / azure-openai（抄 frigate `genai/plugins/`）。

### 6.3 隔离

**I5**：cognition 是可选模块。无 LLM 配置时 perception 独立完整运行。cognition 只订阅事件总线，不反向依赖 perception。

---

## 7. 交互层（agent）—— AI 运维 Agent

**参照来源**：AIGX `src/agent/`（mod/runner/session/tools/approval/audit/llm/api），三层风险分级（`tools.rs` 的 RiskLevel）。

```rust
// src/agent/mod.rs —— 抄 AIGX src/agent/mod.rs，换工具注册表
pub enum RiskLevel { ReadOnly, LowRisk, HighRisk }
// 工具：
//   ReadOnly: nvr_list_devices / nvr_list_alarms / nvr_search_recording / nvr_diagnostics
//   LowRisk:  nvr_start_analysis / nvr_stop_analysis / nvr_set_zone / nvr_snapshot
//   HighRisk: nvr_delete_device / nvr_delete_recording
```

**关键**：Agent 用 AIGX 的渠道推理，进程内直调 bridge（抄 AIGX `agent/llm.rs` 的自环推理），不经 HTTP、不计费。

---

## 8. 协议层（aivx-net）—— DeviceAdapter

### 8.1 设计

**参照来源**：`open-nvr-main/server/services/camera_drivers/base.py`（"不支持是数据不是异常"+"无 set_ip 安全属性"）+ `registry.py`（driver 发现/选择/ONVIF 兜底）。

```rust
// aivx-net/src/adapter.rs
#[async_trait]
pub trait DeviceAdapter: Send + Sync {
    async fn discover(&self) -> Result<Vec<DeviceCandidate>>;   // ONVIF WS-Discovery
    async fn get_streams(&self, d: &Device) -> Result<Vec<Stream>>;
    async fn capabilities(&self, d: &Device) -> Result<Capabilities>; // I10
    async fn ptz(&self, d: &Device, cmd: PtzCmd) -> Result<()>;
    async fn snapshot(&self, d: &Device) -> Result<Vec<u8>>;
}
// impl OnvifAdapter（默认，跨品牌：TP-LINK/海康/大华/宇视）
// impl HikvisionAdapter / DahuaAdapter（可选 feature，ONVIF 不够才加）
// registry：manufacturer 匹配 → probe → ONVIF 兜底（抄 open-nvr registry.py）
```

### 8.2 关键决策

- **"不支持"是数据不是异常（I10）**：`capabilities()` 返回能力集，前端动态渲染（支持 PTZ 才显示云台），而不是 ruoyi 的"一张大表全字段"。
- **安全属性**：抄 open-nvr base.py 的"无 set_ip / 无 factory_reset"——结构上不存在危险方法，杜绝误操作砖机。
- **TP-LINK 起步**：ONVIF Profile S + RTSP（`rtsp://admin:pass@ip:554/stream1|stream2`）。GB28181 留给接平台级联时再加。

---

## 9. 数据模型（SeaORM entity + migration）

### 9.1 核心表

参照：`AIGX/src/db/entity/channel.rs`（entity 写法）+ rebucca `app/models.py`（业务字段）+ ruoyi `sql/ry-cloud.sql`（协议字段）。

```rust
// src/db/entity/device.rs
#[derive(DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "devices")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,                 // UUID（抄 AIGX）
    pub name: String,
    pub access_type: String,        // "onvif" / "rtsp" / "gb28181"
    pub onvif_url: Option<String>,
    pub rtsp_main: Option<String>,  // 主码流（录像/监看）
    pub rtsp_sub: Option<String>,   // 子码流（分析）—— I4
    pub username: Option<String>,
    pub password_enc: Option<String>, // enc: 前缀 + 派生密钥（抄 AIGX）
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub capabilities: Option<String>, // JSONB 能力集（I10）
    pub record_enable: bool,
    pub status: String,             // "online" / "offline"
    pub created_at: i64, pub updated_at: i64,
}
```

其他表：`algorithms / biz_algorithms / zones / rules / alarms / tracks / recordings / media_servers / users / llm_configs / events`。

### 9.2 关系

- `device 1—N zone`，`zone N—M biz_algorithm`，`biz_algorithm N—1 algorithm(small_model)`。
- `alarm` 由 `events` 派生（物化视图），`track` 同。
- `recording` 由录像文件索引（ffmpeg 段），`events` 记录段边界。

---

## 10. 可观测（observ）

**参照来源**：AIGX `src/metrics.rs` + `src/health.rs`（已成熟，直接平移）。

```rust
// src/observ/
// - tracing（结构化日志，抄 AIGX src/log.rs）
// - Prometheus metrics：每路 stream_health/fps/analyze_delay/dropped_frames，
//   规则引擎命中数，LLM 调用延迟/token
// - 健康端点：/api/healthz（含每路流水线状态，抄 rebucca pipeline.status()）
```

**每路状态**（抄 rebucca `pipeline.py` 的 `status()`）：running / stream_health / stalled_sec / analyze_fps / decode_fps / dropped_count / active_zones。

---

## 11. 进程边界（外部进程托管）

| 边界 | 决策 | 理由 |
|---|---|---|
| ffmpeg 拉流 | 外部进程（`tokio::process`） | 解码是 C 强项；Rust 托管生命周期 |
| ffmpeg 录像 | 外部进程（`-c copy`） | 零转码；Rust 只管 spawn + 段管理 |
| ZLMediaKit | 外部进程（可选） | 低延迟转发；Rust 收 hook（抄 ruoyi-zlm `ZLMHttpHookListener`） |
| YOLO 推理 | 进程内（`ort`） | 共享 Arc 省内存，批处理 |
| LLM | 进程外 HTTP（`reqwest`） | OpenAI 兼容 API |

**看门狗**：`tokio::process::Child` + 定时检查，ffmpeg 卡死自动重启（抄 frigate `CameraWatchdog` + `FrigateWatchdog`）。

---

## 12. 安全

- **凭据加密存储**（抄 AIGX `enc:` 前缀 + AES-GCM，`src/storage/`）。
- **管理面鉴权**：JWT + RBAC（admin/user），所有 `/api/*` 鉴权（抄 AIGX `src/auth/`）。
- **默认离线**：无主动配置不向互联网发送任何数据（PRIVACY.md 核心承诺）。
- **LTS 设计**：不引入不稳定的重依赖；协议层用 trait 隔离，未来可换实现。

---

## 13. 路线图（实施顺序）

| 阶段 | 目标 | 依赖的参照文件 | 里程碑验证 |
|---|---|---|---|
| P0 | workspace + config + event 骨架 + FramePool + 单写者 DB | AIGX src/main.rs/config.rs | CI 绿，帧池 benchmark 0 分配 |
| P1 | ONVIF 发现 + RTSP 拉流 + EMA 运动 + WS 推帧 | open-nvr base.py + frigate frigate_motion.py | TP-LINK 出运动框 |
| P2 | ort YOLO + ByteTrack + 规则引擎 + 报警 + 推送 | rebucca engines + ai-nvr tracker/alert | 进区域触发报警 |
| P3 | 录像 + 保留 + 回放 | rebucca recording/manager.py | 24/7 录像可回放 |
| P4 | cognition（LLM 复核 + 语义描述） | frigate-event-handler daemon.py + ai-nvr multimodal | 误报过滤生效 |
| P5 | agent（AI 运维） | AIGX src/agent/ | 自然语言查报警 |
| P6 | GB28181 信令 | ruoyi-gb28181 transmit/ | 接平台级联 |
| P7 | 多品牌 + PTZ | open-nvr camera_drivers/ | 换品牌零代码 |

**P0 是地基**：帧池 + 事件溯源 + 单写者 DB 三件套决定天花板，先定死，后面不动。

---

## 14. 决策登记（ADRs）

每个重大决策在这里留痕，含"推翻过什么"。

| ADR | 决策 | 推翻 | 参照 |
|---|---|---|---|
| ADR-001 | 单进程 + tokio task，不用多进程 | Frigate/rebucca 的多进程模型 | Rust 无 GIL |
| ADR-002 | 事件溯源 + 物化视图 | rebucca 的直接 ORM 写 | rebucca 写锁教训 |
| ADR-003 | FramePool 零拷贝 + NV12 | rebucca JPEG 跨进程 | rebucca remote_detector |
| ADR-004 | 数据驱动规则引擎 | rebucca 硬编码 5 种 | ai-nvr alert/engine |
| ADR-005 | DeviceAdapter 能力驱动 | ruoyi 大表全字段 | open-nvr base.py |
| ADR-006 | 三层配置热重载 | rebucca 手动 reload | frigate holder + AIGX config |
| ADR-007 | 子码流分析 / 主码流录像 | ai-nvr "不降监看" | ai-nvr CLAUDE.md |

---

## 15. 反例清单（血泪教训，评审时对照）

1. rebucca 高频写库 → SQLite 写锁卡页面（`pipeline.py` 注释原文）。→ AIVX 事件溯源根治。
2. rebucca JPEG 跨进程 → 每帧 2 次编解码 + 坐标 rescale。→ AIVX 帧池零拷贝。
3. frigate 多进程 → 8 路 10+ 进程内存爆炸。→ AIVX 单进程共享模型。
4. ai-nvr 降监看分辨率 → 用户体验崩塌。→ AIVX I4 铁律。
5. rebucca 硬编码规则 → 改规则要改代码。→ AIVX 数据驱动规则引擎。
6. ruoyi 一张大表全字段 → 扩展靠加列。→ AIVX 事件流 + 物化视图。
7. open-nvr 微服务太重 → 光容器 10+ 个。→ AIVX 单二进制。

---

> **AIVX 架构不变量（再次强调）**：I1 帧零拷贝 · I2 分析不碰 DB · I3 运动先于检测 ·
> I4 子码流分析主码流录像 · I5 perception 独立 · I6 事件流唯一事实源 · I7 单写者落库 ·
> I8 状态变化才报警 · I9 配置三层热重载 · I10 能力驱动。
>
> 这套设计的目标不是"能跑"，而是"能用 1 万年"——每个决策都有参照、有落地、有反例、有可验证的不变量。评审挑不出毛病的标准，就是每条不变量都有对应的强制机制和 benchmark 断言。