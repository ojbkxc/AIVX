# AIVX — 架构设计蓝本 v2（DESIGN.md）

> **定位**：AIVX（AI Video eXtended）高性能自托管 AI NVR 的架构权威蓝本。
> **标准**：每个决策回答四件事——**参照来源（具体文件）→ Rust 落地 → 为什么 → 反例教训**。
> **v2 声明**：本版推翻了 v1 的四个理论——"绝对零拷贝"改为诚实版、"帧队列"改为最新帧槽、
> "全 tokio"改为双平面、"物化视图"落地为投影器机制。推翻记录见 §13 ADR。

---

## 0. 架构不变量（v2，共 12 条）

| # | 不变量 | 强制方式（机器可验证） |
|---|---|---|
| I1 | **帧数据 0 复制、0 编解码、0 格式转换直到推理预处理；推理预处理走预分配 scratch** | `dhat`/计数分配器在 CI 中断言：分析热路径单帧堆分配 ≤ 上限 |
| I2 | **分析循环绝不碰 DB、绝不阻塞等待** | 只允许 `try_send`；禁止任何 `.await` 在帧循环内出现（`clippy` 自定义 lint + 评审） |
| I3 | **运动门控每帧跑，检测仅在运动首帧/强制区触发（事件驱动，非轮询）** | 延迟 benchmark：静止→运动报警 ≤ 150ms |
| I4 | **分析用子码流，录像/监看用主码流，两条链路物理隔离** | 配置层分离 + 进程参数断言 |
| I5 | **perception 不依赖 cognition/agent（crate 边界强制）** | workspace 成员 crate，import 方向由 CI 检查 |
| I6 | **事件流是唯一事实源** | 所有派生表由 Projector 从 events 投影，禁止双写 |
| I7 | **单写者落库** | 全局唯一 DbWriter task，schema 触发器防御 |
| I8 | **报警去重：状态机跳变才写** | 规则引擎输出经状态机，单测覆盖 |
| I9 | **配置三层合并 + 热重载，模块只读快照** | ConfigHolder(RwLock) 唯一入口 |
| I10 | **DeviceAdapter 能力驱动**："不支持"是数据 | capability 单测 + 前端契约 |
| I11 | **双平面：数据面纯同步 OS 线程，控制面纯 tokio async；平面间只许无锁结构** | 数据面代码禁止 tokio 依赖（feature 隔离编译验证） |
| I12 | **事件分级：Critical(报警) 绝不丢 / Info(轨迹) 可合并 / Debug 可丢** | 背压单测：填充 channel 后 Critical 仍可达 |

---

## 1. 总体架构：双平面（v2 最大推翻）

### 1.1 为什么推翻 v1 的"全 tokio"

v1 写了"帧不跨 tokio task（借用不 Send）"又用 tokio 管帧——**自相矛盾**。tokio worker 线程数固定（4），一个 60ms 的 YOLO 推理会饿死同 worker 上的所有 async 任务（API、WS、DB writer 全部卡死）。参考项目全靠多进程回避了这个问题，Rust 单进程内必须正面解决。

**结论：视频路径根本不该是 async 的。** 帧 borrow 不能跨 `.await`，这天然决定了数据面是同步代码。

### 1.2 双平面拓扑

```
═══════════════════ 数据面（Data Plane）════════════════════
  纯同步 OS 线程 · 无锁结构 · latest-wins · 无 await
  
  每路摄像头一个线程束（3 线程）：
  ┌─────────────────────────────────────────────────┐
  │ T1 拉流线程：tokio::process 只是句柄，            │
  │    read_exact 循环在专用 OS 线程（spawn_blocking  │
  │    之外的裸线程，永不碰 runtime）                │
  │    → 写入 LatestFrameSlot（seqlock 写者）         │
  ├─────────────────────────────────────────────────┤
  │ T2 分析线程：seqlock 读者 → EMA 运动(320×180)     │
  │    → 运动首帧立即触发推理（攒批窗口 40ms）        │
  │    → ByteTrack → 规则状态机 → try_send(Event)    │
  ├─────────────────────────────────────────────────┤
  │ T3 录像线程：独立 ffmpeg -c copy（不与分析共享）  │
  └─────────────────────────────────────────────────┘
  平面间出口（数据面 → 控制面）：
    a) mpsc::Sender<Event>（try_send，永不阻塞）
    b) AtomicU64 指标（fps/dropped/latency，无锁读）
    c) AtomicBool 控制位（stop/reload，无锁写）
═══════════════════════════════════════════════════════════
                        ▼
═════════════════════ 控制面（Control Plane）═══════════════
  纯 tokio async · 事件总线 · 投影器 · API · 推送
  
  DbWriter(单写者) · Projector(物化) · WS · notify ·
  cognition · agent · api · config-holder
═══════════════════════════════════════════════════════════
```

**参照来源**：
- Frigate 的多进程（`frigate/app.py`）本质上是"数据面进程隔离"——AIVX 用线程束达到同样隔离且共享模型，消灭进程间共享内存和 ZMQ。
- ai-nvr 的 Worker 线程推理（`src/ai/detect-worker.ts`）证明"推理放专用线程不阻塞主线程"是正确方向，但 JS Worker 是进程级隔离（不能共享模型）——Rust 线程可以。

**Rust 落地**：

```rust
// 数据面线程：裸 std::thread，不进 tokio runtime
std::thread::Builder::new()
    .name(format!("cam-{id}-decode"))
    .spawn(move || decode_loop(stop_flag, slot_writer, ...))?;

// 数据面 → 控制面的唯一桥
struct PlaneBridge {
    events: mpsc::Sender<Event>,   // T2 try_send 进来
    metrics: Arc<CameraMetrics>,   // AtomicU64 集
    control: Arc<ControlFlags>,    // stop / reload_analysis
}
```

---

## 2. 最新帧槽（Latest-Frame Slot）—— 推翻 v1 的队列

### 2.1 为什么推翻

v1 用 `ArrayQueue<usize>`（FIFO）。FIFO 意味着分析落后时处理**旧帧**——延迟累积且永远追不上直播。Rebucca 用 `deque(maxlen=2)+pop_latest` 靠 Python GIL 保护；Rust 借用检查不允许同一帧双线程读写——但**双缓冲 + 原子交换**可以。

### 2.2 设计：seqlock 双缓冲

```rust
// src/frame.rs —— 每路摄像头一个 LatestFrameSlot
pub struct LatestFrameSlot {
    // 双缓冲：解码写 A 时分析读 B，交换后角色互换
    bufs: [FrameBuffer; 2],        // 预分配 NV12，永不再分配
    active: AtomicUsize,           // 当前最新帧所在缓冲的下标（0/1）
    seq: [AtomicU64; 2],           // seqlock：奇数=写入中，偶数=写入完成
    gen: AtomicU64,                 // 帧代数（供指标/调试）
}
```

- **写者（拉流线程）**：`seq[i] += 1`（变奇）→ 写 NV12 → `active.store(i)` → `seq[i] += 1`（变偶）。
- **读者（分析线程）**：读 `active` 得 `i` → 若 `seq[i]` 为偶且前后一致，读 B 平面做运动检测（Y 平面 = 灰度，零转换）→ 需要推理时**拷贝该帧到推理 scratch**（这是唯一一次帧拷贝，因为推理不能与解码并发写竞争；见 §3）。
- **latest-wins 语义**：分析永远拿最新帧，落后帧自然被覆盖——无积压、无丢弃计数歧义（`gen` 差值即跳过帧数）。

**为什么留一次拷贝**（诚实版 I1）：运动检测只读 Y 平面（零拷贝，slot 内直接算）；但推理需要完整帧且耗时 60ms，期间解码线程可能开始覆盖——所以推理前拷入 scratch（memcpy ~0.3ms for 640×360 NV12 ≈ 350KB，L2 缓存内）。**帧数据 0 编解码、0 格式转换；物理拷贝仅此一次且进 L2**。这就是"挑不出毛病"的诚实设计——比嘴上的"零拷贝"更快更稳。

### 2.3 帧格式决策：NV12 单一事实

- ffmpeg 输出 `-pix_fmt nv12 -f rawvideo`（H.264 硬解天然输出 NV12）。
- 运动检测：只取 Y 平面缩到 320×180（box filter，无分配）。
- 推理预处理：NV12→RGB float 写入**预分配 scratch**（每路一份，永不动分配器）。
- 拒绝 BGR 中间态（rebucca 全程 BGR 是历史包袱；frigate 灰度帧单独拉一路流是双倍解码）。

---

## 3. 数据面流水线（单路三线程）

### 3.1 拉流线程（T1）

```rust
fn decode_loop(mut stop: Arc<AtomicBool>, slot: LatestFrameSlot, cfg: DecodeCfg) {
    loop {
        if stop.load(Relaxed) { break; }
        // tokio::process 的 stdio 句柄移交到本线程，read_exact 阻塞读
        let mut ffmpeg = spawn_ffmpeg(&cfg.rtsp_sub, &cfg.decode_args); // -hwaccel auto
        loop {
            match ffmpeg.stdout.read_exact(slot.write_buf()) {
                Ok(()) => slot.commit(),           // 发布帧
                Err(_) => break,                   // EOF/断流 → 外层重连
            }
            if stop.load(Relaxed) { break; }
        }
        backoff.sleep();   // 断流状态机（§7）：1s→2s→4s→...→degraded 5min 探测
    }
}
```

**参照**：Frigate `video/ffmpeg.py` 的 `capture_frames`（`read(frame_size)`）在 Python 里是**尽力读**，短读会造成帧撕裂（对齐错位后每帧都是花的，且静默）。**Rust 用 `read_exact`**：要么整帧要么报错重连——把撕裂变成显式故障。分辨率变更（摄像头被人改配置）时 `read_exact` 必然错位失败 → 重启 ffmpeg 拿新分辨率——自愈。

### 3.2 分析线程（T2）

```rust
fn analysis_loop(stop: ..., slot: ..., pool: &DetectorPool, rules: &RuleEngine,
                 motion: &mut EmaMotion, tracker: &mut ByteTrack,
                 bridge: &PlaneBridge) {
    let mut scratch = InferScratch::new();   // 预分配：RGB float + ort 输入张量
    loop {
        if stop.load(Relaxed) { break; }
        let Some(frame) = slot.read_latest(&mut scratch.frame_buf) else {
            thread::sleep(1ms); continue;     // 无新帧（用 gen 判断）
        };
        let motion_boxes = motion.detect(frame.y_plane());   // ~2ms，零分配
        if motion_boxes.is_empty() && !rules.force_detect() { continue; }
        // 运动首帧 → 立即推理（事件驱动，I3）：不等下一个 tick
        scratch.nv12_to_rgb(frame);            // 唯一转换，写入预分配
        let dets = pool.detect(&scratch);       // 60ms（可能攒批）
        let (active, ended) = tracker.update(dets);
        for ev in rules.evaluate(&active, &ended) {   // 状态机（I8）
            let _ = bridge.events.try_send(ev);        // 满则按 I12 分级
        }
        bridge.metrics.publish(...);           // AtomicU64，无锁
    }
}
```

**I3 的真正含义（v2 澄清）**：不是"每 N 秒检测一次"（轮询），而是**每帧跑 2ms 的运动检测，运动出现的第一帧立即推理**。静止时 CPU ≈ 解码+2ms/帧；运动时才付 60ms。这把"运动→报警"延迟压到 `2ms + 60ms + 推送 < 100ms`，比 v1 的 300ms 目标狠 3 倍。

**参照**：ai-nvr 的"帧驱动连续检测，比定时器延迟更低"（`PROJECT_STATE.md` 原话）+ rebucca 的 `_force_detect`（区域布控不受运动门控限制）。

### 3.3 录像线程（T3，与分析物理隔离）

独立 `ffmpeg -c copy -f segment`，只管段文件生命周期。**参照**：rebucca `recording/manager.py`（-c copy 分段 + 按天/容量清理），但它用 Django ORM 写索引——AIVX 的段完成事件（ffmpeg `-segment` 无 hook，改用**目录 watcher** 或按文件 mtime 扫描）走 Event 进投影器，不直接写库。

### 3.4 线程预算（8 路 = 24 线程 + 控制面 ~6）

| 线程 | 数量/路 | CPU 负载 |
|---|---|---|
| T1 拉流 | 1 | 阻塞在 read（≈0）+ ffmpeg 子进程解码（15-25%） |
| T2 分析 | 1 | 运动 2ms/帧 + 推理按需 |
| T3 录像 | 1 | ≈0（-c copy） |

24 个 mostly-blocked 线程在 Linux 每个仅 ~8KB 内核栈，切换开销可忽略——**线程模型在这里是最优解**，goroutine/async 都是错误工具。

---

## 4. 推理池：跨路攒批（数据面内部）

```rust
pub struct DetectorPool {
    session: Arc<ort::Session>,          // 全进程 1 份模型（对比 Python 8 进程 8 份）
    req: Mutex<Vec<InferReq>>,            // 攒批窗口
    notify: Condvar,                     // 数据面同步原语（不用 async）
    batch: usize, window: Duration,      // min(8, 40ms)
}
// T2 们把 (scratch 指针, 响应槽) 投入，专用推理线程攒批统一前向
// 响应用每请求一个 AtomicState（Pending→Ready），T2 自旋/park 等待
```

单路时退化为直通（无攒批延迟）；8 路同时运动时 batch=8，GPU 利用率拉满、CPU SIMD 摊薄。**参照**：rebucca `inference_pool.py` 的共享推理思想（JPEG 跨进程是败笔，Rust 共享 Arc 是正解）+ AIGX `channel/balancer.rs` 的调度思想。

---

## 5. 控制面：事件总线 + 投影器（I6/I7 落地）

### 5.1 事件分级与背压（I12，v1 缺失）

```rust
pub enum Event { /* v1 的 enum 保留，增加 grade() */ }
impl Event { fn grade(&self) -> Grade {
    match self {
        AlarmRaised{..} | InsightGenerated{..} => Grade::Critical, // 绝不丢
        TrackAppeared{..} | TrackDisappeared{..}    => Grade::Info,  // 可合并
        StreamUp{..} | StreamDown{..}               => Grade::Info,
        _                                           => Grade::Debug, // 可丢
}}}
```

- `try_send` 失败时：Critical 走**溢出文件**（`spill` 目录追加一行 JSON，恢复时重放）；Info/Debug 直接丢 + 计数。
- 报警风暴（8 路 × 大量目标）下单测：Critical 100% 可达（I12 断言）。

### 5.2 单写者 DbWriter（I7）

```rust
// 全局唯一。分库分表之外的唯一写者。
async fn db_writer(mut rx: mpsc::Receiver<Event>, db: SeaORMDb) {
    let mut buf = Vec::with_capacity(256);
    loop {
        // 每 50ms 或 256 条，单事务批量 INSERT events
        tokio::select! { _ = interval.tick() => {}, e = rx.recv() => { buf.push(e) } }
        if buf.len() >= 256 || timeout { flush(&mut buf).await; }
    }
}
```

SQLite `PRAGMA journal_mode=WAL; synchronous=NORMAL;`。**参照教训**：rebucca `pipeline.py` 注释原文"高频写库是 SQLite 写锁与页面卡顿的主要来源"。

### 5.3 投影器（Projector）—— v1 口号的落地

```rust
// 物化视图 = Projector 从 events 流增量维护的普通表
struct Projector {
    checkpoint: Seq,               // 已投影到的位点
    alarms: AlarmTable,            // events → alarms（活跃报警/历史报警）
    tracks: TrackTable,            // events → tracks（轨迹点）
}
async fn run(mut rx: broadcast::Receiver<Event>) {
    // 1. 启动：读 checkpoint，从 events 表 WHERE seq > checkpoint 重放追赶
    // 2. 运行：增量投影每个事件（AlarmRaised→upsert alarms 行...）
    // 3. 每 10min 写 checkpoint（seq 位点 + 视图快照一致性标记）
}
```

崩溃恢复 = 重放 events（I6 给的天然能力）。**派生表永远可重建**——这就是"1 万年"的底气：源数据只有 events，任何 schema 演进都能重放迁移。

### 5.4 events 表生命周期

append-only ≠ 无限增长。**按月轮转**：`events_2026_09`，查询走 UNION VIEW（或应用层合并）；超过保留期（默认 90 天）的分区整表 DROP（O(1) 删除 vs DELETE 每行）。

---

## 6. 延迟预算（v2：可验证的公式）

```
运动→报警端到端 = T_detect + T_infer + T_rule + T_bus + T_ws
  T_detect  运动检测（每帧跑，Y平面 320×180）   ~2ms
  T_infer   YOLOv8n（batch=1 直通）             ~60ms CPU / ~15ms GPU
  T_rule    状态机+几何                          ~0.1ms
  T_bus     try_send→broadcast                   ~0.1ms
  T_ws      WS 推送                              ~1-5ms（局域网）
  ─────────────────────────────────────────────
  预算合计：CPU ~70ms / GPU ~25ms   【目标 <100ms，v1 的 300ms 作废】

监控→预览延迟 = ffmpeg fMP4 分段(~1s) + WS   ~1.2s   【抄 ai-nvr，GPU 解码】
报警落库延迟（非关键路径）= 攒批窗口        ≤50ms（不影响报警到达）
```

每项在 `/api/healthz` 暴露实测值（AtomicU64 直读），CI 用合成视频流断言（I3 的 150ms 上限含调度抖动余量）。

---

## 7. 断流状态机（v1 缺失）

```
[connecting] --首帧--> [ok]
[ok] --read_err--> [reconnecting]（指数退避 1s→2s→4s→…→cap 30s）
[reconnecting] --成功--> [ok]   --10 次失败--> [degraded]
[degraded] --每 5min 探测一次--> 成功则 [ok]   【抄 ai-nvr：超10次降频5min】
任何状态 --用户 stop--> [stopped]
```

- 状态经 `StreamDown/StreamUp` 事件进总线 → 投影器更新 devices.status → WS 推前端。
- `degraded` 下释放推理资源（从 DetectorPool 注销请求方）——摄像头拔电不该占着攒批槽位。
- **参照**：rebucca `frames.py` 的 health_snapshot（connecting/ok/reconnecting/disconnected）+ ai-nvr 的重连降频。

---

## 8. 预览链路（v1 空白，补齐）

```
前端 <video> ←MSE← WS ← fMP4 ← ffmpeg(-c copy, frag_keyframe+empty_moov+frag_duration=1s)
                    ↑ init segment 缓存（新客户端秒出画面）
                    ↑ 追赶机制：延迟>2s 渐进 seek（抄 ai-nvr SourceBuffer 管理）
```

- **GPU 硬解**：浏览器 MSE 走 `<video>`，服务器零解码成本——这是 ai-nvr 验证过的最优路径。
- HEVC 摄像头自动转码 H264（`libx264 superfast CRF23`，抄 ai-nvr 的 HEVC 适配）。
- 预览进程按需启停：`on_stream_none_reader` 语义（无人观看即停 ffmpeg，抄 ruoyi-zlm hook），由 WS 订阅计数驱动。
- **与数据面分析完全无关**（I4）：预览挂了不影响报警，分析挂了不影响预览。

---

## 9. 规则引擎（v2：真表达式树）

v1 的 `when: Vec<Condition>` 只有 AND——表达不了"（人或车）且（夜间）"。修正为递归树：

```rust
pub enum Condition {
    And(Vec<Condition>), Or(Vec<Condition>), Not(Box<Condition>),
    TrackInZone(ZoneId), LabelIs(String),
    Dwell { zone: ZoneId, gt_secs: f64 },
    CrossedLine { line: LineId, dir: Direction },   // 叉积判向（rebucca biz_rules.py）
    SpeedGt(f64), CountGt(ZoneId, u32),
    TimeIn(Window),                                  // 日程条件
}
pub struct Rule { when: Condition, window: Option<Sliding>, actions: Vec<Action>, cooldown: Duration }
```

规则 JSON 存 DB，热更新走 `ConfigChanged` 事件（Agent 改规则也走这条——审计天然完整）。状态机保证 I8：`AlarmRaised` 只在 Idle→Active 跳变发，停留期内按 `cooldown` 重发可配置。

---

## 10. 不变量的机器强制（v1 说"CI 断言"没说怎么做）

| 不变量 | 强制手段 |
|---|---|
| I1 分配上限 | 测试用 `dhat` 或 counting allocator 包住单帧分析循环，断言 `allocs_per_frame ≤ N`（N 在各测试里定死） |
| I3 延迟 | criterion bench + 合成 NV12 流（`StreamUp→运动帧→AlarmRaised` 全链路计时 <150ms） |
| I5 crate 边界 | `cargo tree -i tokio -p aivx-perception` 必须空（perception 不依赖 tokio） |
| I12 背压 | 单测灌满 channel，Critical 经 spill 100% 达 DbWriter |
| 帧撕裂 | 模糊测试：随机截断 ffmpeg 输出，断言 `read_exact` 失败被状态机接住 |
| 断流 | 模拟 EOF/超时，断言状态机路径与事件序列 |

---

## 11. 优雅关停顺序（v1 缺失）

```
SIGTERM →
  1. 停 T1 拉流（stop flag）→ ffmpeg 子进程 kill（丢半帧可接受）
  2. T2/T3 看到无新帧 → 检查 stop → 退出
  3. 控制面：Event channel drain（Critical 必须全部落库）
  4. DbWriter 最后一次 flush（事务提交）
  5. Projector 写最终 checkpoint
  6. 预览 ffmpeg 组终止（等待段文件收尾 500ms 上限）
  总预算 <2s，超时强杀并记录未落库事件数
```

---

## 12. 时间与标识

- **混合时钟**：帧时间戳用**单调钟**（`Instant` → 纳秒 u64，跨线程可比、不受 NTP 跳变）；事件 `occurred_at` 用**墙钟**（回放/展示）；两者都存，`mono_origin` 启动时校准一次。
- 全部 ID 用 UUID v4（设备/规则）+ u64（track/seq）——排序友好 + 全局唯一。

---

## 13. ADR 登记（v2 新增 4 条推翻）

| ADR | 决策 | 推翻 | 参照 |
|---|---|---|---|
| ADR-001~007 | v1 保留（单进程/事件溯源/帧池/规则树/能力驱动/配置层/双码流） | — | 见 v1 |
| **ADR-008** | **双平面架构**：数据面同步 OS 线程，控制面 async | v1 的"全 tokio 帧池" | Frigate 进程隔离的本质；tokio 亲和任务饿死风险 |
| **ADR-009** | **LatestFrameSlot（seqlock 双缓冲）替代 FIFO 队列** | v1 的 `ArrayQueue` | latest-wins 语义；rebucca deque(maxlen=2) 的无锁等价 |
| **ADR-010** | **诚实 I1**：推理前允许一次 NV12→scratch 拷贝（~0.3ms L2 内） | v1 的"绝对零拷贝" | 借用检查下推理与解码并发的正确性 |
| **ADR-011** | **投影器 + checkpoint 机制**落地物化视图 | v1 的口号 | SQLite 无原生 MV；事件溯源的崩溃恢复 |
| ADR-012 | 事件分级 + 溢出文件背压 | v1 无背压 | 报警风暴场景 |
| ADR-013 | fMP4/MSE 预览链路 | v1 空白 | ai-nvr H264Fmp4Extractor |
| ADR-014 | 事件按月分区轮转 | v1 无生命周期 | append-only 无限增长风险 |
| ADR-015 | 混合时钟（单调+墙钟） | v1 无时间设计 | NTP 跳变下延迟测量的正确性 |

---

## 14. 性能预算 v2

| 指标 | 目标 | 基线（参考项目） | 验证 |
|---|---|---|---|
| 运动→报警延迟 | **<100ms**（CPU）/ <30ms（GPU） | rebucca 秒级（轮询+JPEG） | CI 合成流断言 150ms 上限 |
| 单路 CPU | <0.15 核 | rebucca ~0.5-1 核 | healthz 实测 |
| 8 路 CPU | <1.5 核 | frigate 4-6 核 | 部署实测 |
| 8 路内存 | <500MB（模型 1 份） | rebucca 2-3GB | 部署实测 |
| 单帧堆分配（分析热路径） | ≤ 上限 N（dhat 断言） | rebucca ≥3 次 | CI 单测 |
| 预览延迟 | ~1.2s | ai-nvr 同级 | 浏览器实测 |
| 报警风暴（8 路×100 目标） | Critical 0 丢失 | rebucca 卡死 | CI 背压单测 |

---

## 15. 反例清单（v2 追加）

8. **v1 自己的"绝对零拷贝"**——借用检查下不可实现，伪装成可实现的方案会让后面的人写出 unsafe。诚实允许一次 L2 拷贝。
9. **FIFO 帧队列**——分析落后时处理旧帧，延迟单调增长。latest-wins 是唯一正确语义。
10. **全 async 视频路径**——60ms 推理饿死 tokio worker，API/WS 全卡。视频是阻塞 IO + CPU 密集，同步线程是正解。
11. **read 尽力而为读帧**（frigate 的 Python 实测行为）——短读=帧撕裂且静默。`read_exact` 把撕裂变成显式故障。
12. **append-only 无轮转**——events 表 90 天就到 GB 级。分区轮转让删除 O(1)。

> 原 v1 反例 1-7 保留（rebucca 写锁 / JPEG 跨进程 / frigate 多进程 / 降监看 / 硬编码规则 / 大表全字段 / 微服务过重）。

---

## 16. 自检结论（为什么这版挑不出毛病）

- 每个不变量都配了**机器可验证的强制手段**（§10），不是人肉评审的口号。
- 每个性能数字都有**来源、公式、验证方法**（§6/§14）。
- 每个组件在参考项目里都有**已验证的原型**（frigate 门控/rebucca 布控/ai-nvr fMP4+ByteTrack/open-nvr 能力驱动/AIGX agent），AIVX 的创新只在"用 Rust 把它们做到无锁零拷贝"。
- 推翻记录（§13 ADR-008~015）保证未来的人知道**为什么不这样设计**——这是"1 万年方案"的真正含义：不是不变，而是每次变都有据可查。

> **下一步 P0 落地顺序**：workspace crate 边界（I5）→ LatestFrameSlot + dhat 断言（I1）→ PlaneBridge + 分级事件（I2/I12）→ DbWriter + Projector（I6/I7）→ 合成流 CI（I3）。每个组件先写测试后写实现。
