# AIVX — 架构设计蓝本 v3（DESIGN.md）

> **定位**：AIVX（AI Video eXtended）高性能自托管 AI NVR 的架构权威蓝本。
> **标准**：每个决策回答四件事——**参照来源（具体文件）→ Rust 落地 → 为什么 → 反例教训**。
> **版本声明**：v3 是**去桩化收尾**——DESIGN.md §23 路线图 P0-P8 全部实现并经 CI 机器验证。
> v1→v2 推翻四大理论（双平面/最新帧槽/诚实零拷贝/投影器）；v2.1 修 3 处技术错误 + 补 6 子系统；
> v2.2 修 2 处正确性 bug + 补 7 缺口；**v3 新增 ADR-029~031**（md5 真实接入 / ort YOLO 接入点 /
> 去桩化完成宣告）。推翻记录见 §20 ADR。

---

## 0. 架构不变量（12 条，每条配机器强制）

| # | 不变量 | 强制方式（机器可验证） |
|---|---|---|
| I1 | **帧数据 0 复制、0 编解码、0 格式转换直到推理预处理；预处理写预分配 scratch；证据编码在独立线程，不在分析热路径** | `dhat`/计数分配器包住单帧分析循环，CI 断言 `allocs_per_frame ≤ N`（报警帧同样断言——证据编码已移出热路径，ADR-021） |
| I2 | **分析循环绝不碰 DB、绝不阻塞等待** | 热路径只允许 `try_send`；数据面 crate 禁 tokio（编译期验证，见 I11） |
| I3 | **运动门控每帧跑（~2ms）；检测由运动首帧事件驱动触发，非轮询** | 合成流 CI 断言：静止→运动→AlarmRaised ≤ 150ms |
| I4 | **分析用子码流，录像/监看用主码流，链路物理隔离** | 两路 ffmpeg 进程独立；配置字段分离；预览挂≠报警挂 |
| I5 | **perception 不依赖 cognition/agent/tokio** | workspace crate 边界 + `cargo tree -i tokio -p aivx-perception` 必须为空 |
| I6 | **事件流是唯一事实源** | 所有派生表由 Projector 从 events 投影；schema 里不存在第二个写入路径 |
| I7 | **单写者落库** | 全局唯一 DbWriter task；seq 由单写者内存分配 |
| I8 | **报警去重：状态机跳变才写** | 规则引擎输出经状态机；单测覆盖连续帧同目标 |
| I9 | **配置三层合并 + 热重载，模块只读快照** | ConfigHolder(RwLock) 唯一入口 |
| I10 | **DeviceAdapter 能力驱动**："不支持"是数据不是异常 | capability 单测 + 前端契约测试 |
| I11 | **双平面：数据面纯同步 OS 线程（std only），控制面纯 tokio；平面间只许无锁结构** | aivx-perception 的 `Cargo.toml` 无 tokio；CI `cargo tree` 断言 |
| I12 | **事件分级：Critical(报警/洞察) 绝不丢 / Info(轨迹) 可丢 / Debug 便宜丢** | 背压单测：灌满 channel 后 Critical 经 spill 100% 到达 DbWriter |

---

## 1. 总体架构：双平面 + 四 crate

### 1.1 为什么是双平面（推翻 v1"全 tokio"）

帧 borrow 不能跨 `.await`——这天然宣判视频路径不该是 async。且一个 60ms 的 YOLO 推理会饿死
tokio worker（默认 4 个），把同 worker 上的 API/WS/DbWriter 全部拖死。参考项目全靠多进程回避
了这个问题（frigate 每路一进程）；Rust 单进程内必须正面解决：**数据面 = 同步 OS 线程**。

### 1.2 Workspace 划分（I5/I11 用 crate 边界强制，不靠自觉）

```
aivx-events/      Event enum + Grade 分级（仅依赖 serde —— 两平面共用词汇）
aivx-perception/  数据面：LatestFrameSlot / EMA运动 / DetectorPool / ByteTrack / 规则状态机
                  （std only：禁 tokio、禁 reqwest —— CI 用 cargo tree 断言）
aivx-net/         协议层：DeviceAdapter(trait) / onvif / rtsp / gb28181（async，控制面使用）
aivx/             控制面主 crate：api / DbWriter / Projector / cognition / agent /
                  notify / 预览链路 / config / 编排（tokio）
frontend/         React 18 + TS + Vite（产物 → static/）
```

**参照**：AIGX workspace（`aigx` + `aigx-net`）的拆分方式；I5 的强制从"文档约定"升级为"编译期物理边界"。

### 1.3 双平面拓扑

```
═══════════ 数据面（Data Plane，std::thread，无锁，无 await）════════════
  每路摄像头一个线程束：
  T1 拉流线程   std::process::Command spawn ffmpeg（-hwaccel auto, rawvideo/nv12）
                → read_exact 整帧 → 写入 LatestFrameSlot
  T2 分析线程   slot 读最新帧 → EMA 运动(Y平面 320×180, ~2ms)
                → 运动首帧立即投 DetectorPool（事件驱动推理）
                → ByteTrack → 规则状态机 → std::sync_channel try_send(Event)
                → 报警时：crop 编码 JPEG 证据（per-alarm，非 per-frame）
  T3 录像线程   独立 ffmpeg -c copy -f segment（与 T1/T2 物理隔离，I4）
  ─────────────────────────────────────────────────────────────
  平面间出口（唯一三种，全部无锁）：
    a) std::sync_channel<Event>(N)      T2 try_send；Critical 满则 spill 文件
    b) Arc<CameraMetrics>               AtomicU64 × N（fps/latency/dropped/health）
    c) Arc<ControlFlags>                AtomicBool（stop / reload / degrade）
════════════════════════════════════════════════════════════════════════
                        ▼  控制面 forwarder（唯一桥接 task）
═══════════ 控制面（Control Plane，tokio）═══════════════════════════════
  forwarder: sync_channel → DbWriter(串行点对点) → fan-out 顺序广播
  seq 分配（单写者内存）· Projector(投影 alarms/tracks)
  WS 推送 · notify(Webhook/邮件/Telegram) · cognition(LLM) · agent · api
  预览链路: fMP4/MSE（独立 ffmpeg，按需启停）
════════════════════════════════════════════════════════════════════════
```

**v2.1 修正**：v2 写了"tokio::process 只是句柄"——错误，AsyncChildStdout 无法干净地做阻塞
`read_exact`。数据面直接用 `std::process::Command`，T1 自己拥有 Child 的完整生命周期（spawn/kill/
重启都在数据面内闭环），控制面只通过 ControlFlags 表达意图。

---

## 2. LatestFrameSlot（seqlock 双缓冲，推翻 v1 的 FIFO 队列）

### 2.1 为什么推翻 FIFO

FIFO 意味着分析落后时处理**旧帧**——延迟单调增长，永远追不上直播。正确语义是 **latest-wins**
（rebucca 用 `deque(maxlen=2)+pop_latest` 在 Python 里绕出来的语义，Rust 用双缓冲原子交换直做）。

### 2.2 设计

```rust
pub struct LatestFrameSlot {
    bufs:   [Box<[u8]>; 2],    // 预分配 NV12，永不再分配
    active: AtomicUsize,        // 最新帧在哪个缓冲（0/1）
    seq:    [AtomicU64; 2],    // seqlock：奇=写入中，偶=完成
    gen:    AtomicU64,         // 帧代数（跳帧计数/指标）
}
// 写者(T1)：写非 active 缓冲 → seq 变奇 → 写 NV12 → active 翻转 → seq 变偶
// 读者(T2)：读 active → seq 为偶且读后复核一致 → 消费；否则重试
```

- **宽限期修正（v2.2）**：双缓冲在 25fps 下读者有 **~40ms**（写者要翻转两次、即跨一个
  完整帧周期，才会覆盖读者正在读的缓冲）——不是 v2.1 写的"一个帧周期"两可表述。
  运动检测直读 slot 内 Y 平面（零拷贝，~2ms）+ 推理前拷贝 scratch（~0.3ms）**合计 <3ms**，
  巨大余量；若推理耗时 60ms **则必须走 scratch**（§3.2 已如此设计——slot 借用只活到
  copy_nv12 返回），读后 seq 复核负责极端竞争下的正确性重试。
- **诚实 I1**：物理拷贝仅推理前一次（NV12→scratch，350KB 进 L2，~0.3ms），0 编解码、
  0 格式转换。比嘴上的"绝对零拷贝"更快更稳——绝对零拷贝在借用检查下要用 unsafe 伪装，
  诚实方案才挑不出毛病。
- **NV12 单一事实格式**：H.264 硬解天然输出 NV12；运动只取 Y 平面（=灰度，零转换）；
  推理预处理 NV12→RGB float 写预分配 scratch。拒绝 BGR 中间态（rebucca 全程 BGR 是历史包袱）。

**参照教训**：frigate 灰度帧单独拉一路流（双倍解码）；rebucca BGR 全链路 + JPEG 跨进程。

---

## 3. 数据面流水线（单路核心三线程 + 证据线程）

### 3.1 T1 拉流线程

```rust
fn decode_loop(stop: &AtomicBool, slot: &LatestFrameSlot, cfg: &DecodeCfg) {
    let mut backoff = Backoff::new();              // §8 断流状态机
    while !stop.load(Relaxed) {
        let mut child = Command::new("ffmpeg").args(cfg.ffmpeg_args()).spawn();
        let Ok(mut child) = child else { backoff.sleep(); continue };
        let mut out = child.stdout.take().unwrap();
        loop {
            if stop.load(Relaxed) { let _ = child.kill(); return; }
            match out.read_exact(slot.write_buf()) {
                Ok(())  => slot.commit(),
                Err(_)  => break,                   // EOF/短读/断流 → 外层重连
            }
        }
        let _ = child.wait();
        backoff.sleep();
    }
}
```

**read_exact vs frigate 的 read**：frigate（`video/ffmpeg.py` `capture_frames`）在 Python 里
`read(frame_size)` 遇到短读会**帧撕裂且静默**（对齐错位后每帧都是花的）。`read_exact` 把撕裂
变成显式故障 → 状态机接管重连。摄像头分辨率被人改动时 read_exact 必然错位失败 → 重启 ffmpeg
拿新分辨率——自愈。CI 加模糊测试：随机截断输出，断言状态机接住。

### 3.2 T2 分析线程

```rust
fn analysis_loop(stop: &AtomicBool, slot: &LatestFrameSlot, pool: &DetectorPool,
                 motion: &mut EmaMotion, tracker: &mut ByteTrack,
                 rules: &mut RuleEngine, evidence: &EvidenceCtx,
                 bridge: &PlaneBridge) {
    let mut scratch = InferScratch::new();   // 预分配：RGB float + ort 输入，永不动分配器
    loop {
        if stop.load(Relaxed) { break; }
        let Some(frame) = slot.read_latest() else { thread::sleep(1ms); continue };
        let boxes = motion.detect(frame.y_plane());          // ~2ms，零分配
        if boxes.is_empty() && !rules.force_detect() { continue; }
        scratch.copy_nv12(frame);                              // 唯一物理拷贝 ~0.3ms
        let dets = pool.detect(&scratch);                      // 60ms CPU / 15ms GPU
        let (active, ended) = tracker.update(dets);
        for ev in rules.evaluate(&active, &ended) {            // 状态机（I8）
            match bridge.try_send(ev) {                        // I12 分级
                Ok(()) => {}
                Err(TrySendError::Full(e)) if e.grade() == Critical => bridge.spill(e),
                Err(TrySendError::Full(_)) => bridge.count_dropped(),
            }
        }
        // 证据编码 v2.2 移出热路径（ADR-021）：报警事件只带 box/scratch 代数，
        // 由独立的低优先级证据线程从 EvidenceRing 取帧编码——热路径绝不付 JPEG 的钱
        bridge.metrics.publish(frame.gen(), boxes, dets.len()); // AtomicU64
    }
}
```

- **I3 的真正含义**：不是"每 N 秒轮询检测"，是**每帧 2ms 运动检测 + 运动首帧立即推理**。
  静止时只付解码+2ms；运动→报警端到端 < 100ms（§7 预算）。
  **参照**：ai-nvr"帧驱动连续检测比定时器延迟更低"（PROJECT_STATE.md）+ rebucca `_force_detect`。
- **运动校准期（v2.2 补，ADR-023）**：EMA 背景模型前 30 帧在建基线（抄 frigate
  `frigate_motion.py` 的 `frame_counter < 30`），此期间**运动框不可信**——分析循环跳过检测
  触发（但仍更新背景）。摄像头重连后重新校准。校准期约 1.2s（30 帧/25fps），期间宁可漏报
  不可误报：刚启动就报警的 NVR 是没人信的。
- **推理并发上限（v2.2 补，ADR-024）**：每路 T2 在推理期间**不再消费新帧**（slot 里
  latest-wins 自然丢帧）。这意味着单路运动期间的分析 FPS = 1000/(2+0.3+60) ≈ 16fps——
  对 25fps 子码流足够（跟踪在 16fps 下 IoU 关联稳定），但这是**显式设计值**而非事故。
  若未来需要分析并行于推理（解码与推理流水化），需双 slot——目前否决，复杂度不值。
- **轮询 vs condvar**：无新帧时 1ms 轮询，25fps 下每秒多醒 ~25 次，成本可忽略；换来无 condvar
  的唤醒丢失/虚假唤醒边界问题。`gen` 保证最坏只晚 1ms。选简单。

### 3.2.1 证据链路（v2.2 新增，ADR-021）

```
T2 分析线程（热路径）             证据线程（每路一个，低优先级）
  报警时只投递：                     收 (alarm_id, box, gen)：
  (alarm_id, box, frame_gen)  ──▶     EvidenceRing[gen] 取该代 NV12 帧
  try_send，微秒级                     ↓（取不到=已被覆盖，跳过并计数）
                                      crop(box) → JPEG 编码（1-5ms，这里付得起）
                                      → 写 evidence/{alarm_id}.jpg
                                      → 事件 EvidenceReady{alarm_id, path}(Info)
```

- **EvidenceRing**：每路固定 N=8 个 NV12 帧引用的环形数组（T1 发布帧时顺手存一份
  `Arc<Nv12Frame>` 引用——引用计数原子操作，零拷贝）。报警帧引用在报警后仍被持有时
  不会被覆盖（Arc 语义），天然解决"编码时帧被写坏"。
- **失败语义**：ring 里该代已被挤出（报警后 8 帧内没轮到证据线程）→ 跳过编码 + 计数，
  报警本身照发——证据是增强，不是依赖。cognition 的 `wait_jpeg` 最多等 `evidence_timeout`
  （2s），超时跳过该次洞察（§10）。
- **线程预算**：+1 线程/路（证据线程平时 park 在 channel 上，零 CPU），计入 §3.4。

### 3.3 T3 录像线程（与分析物理隔离，I4）

独立 `ffmpeg -c copy -f segment -segment_time 600`。段边界无 hook：用**段文件 mtime 扫描**
（T3 低频轮询目录，10s 一次）产生 `RecordingSegment` 事件 → 投影器建索引。
**参照**：rebucca `recording/manager.py`（-c copy + 按天/容量清理），但它直接写 ORM——AIVX 走事件。

### 3.4 线程预算

8 路 = 24 个 mostly-blocked OS 线程（T1/T2/T3）+ 8 个证据线程（park，零 CPU）+ 每引擎键
1 个推理线程 ≈ 33，加控制面 ~6 tokio worker。Linux 每线程 ~8KB 内核栈，全部 mostly-blocked——
线程是这里的**最优并发原语**，async 是错误工具。

---

## 4. DetectorPool：跨路攒批（数据面内部，同步原语）

```rust
pub struct DetectorPool {
    sessions: Mutex<HashMap<EngineKey, Arc<ort::Session>>>,  // 按(algo_id,engine,device,conf,iou)键缓存
    queue:    Mutex<Vec<InferReq>>,     // 攒批窗口
    cv:       Condvar,                  // 数据面同步原语（非 async）
    batch:    usize,                    // min(8, 摄像头数)
    window:   Duration,                 // 40ms
}
// 专用推理线程：收请求 → 攒到 batch 或 40ms 窗口 → 单次 ort 前向（batch 维度）
// 响应：每请求一个 AtomicState(Pending→Ready)，T2 park/自旋等待
```

- **多模型修正（v2.2，ADR-025）**：v2.1 的"全进程 1 份模型"是错的——规则可引用不同算法
  （人车用 A 模型、车牌用 B 模型）。改 rebucca 同款键缓存（`worker_pool.py` 的
  `(algo_id, engine, model_file, conf, iou, size, task, device)` 键），同键请求才互相攒批，
  不同键的请求各自攒批。同键场景下仍是"1 份模型 N 路共享"。
- **线程预算修正（v2.2）**：一个 pool 只有一个推理线程，多键会串行化各键批——每**键**一个
  推理线程（同键内才攒批），键数实际 ≤2（常见一个通用检测模型）。
单路时直通（无攒批延迟）；8 路同时运动 batch=8，GPU 利用率拉满、CPU SIMD 摊薄。
**参照**：rebucca `inference_pool.py` 共享推理的思想（其 JPEG 跨进程是败笔，Arc 共享是正解）。

---

## 5. 控制面：事件 → 落库 → 投影（I6/I7/I12 落地）

### 5.1 Event 与分级

```rust
// aivx-events —— 两平面共用的词汇表（serde only）
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    StreamUp{..}, StreamDown{..},
    TrackAppeared{..}, TrackDisappeared{..}, TrackEnteredZone{..}, TrackLeftZone{..},
    AlarmRaised{..}, AlarmCleared{..},
    EvidenceReady{ alarm_id, path },           // 证据线程产物（Info）
    InsightGenerated{ alarm_id, insight },     // cognition 回填（Critical）
    RecordingSegment{..},
    AgentAction{..}, ConfigChanged{..},
}
impl Event { fn grade(&self) -> Grade; }   // Critical: Alarm/Insight；Info: Track/Stream/Recording；Debug: 其余
```

### 5.2 单写者 DbWriter + 顺序广播（v2.2 修 I12 正确性 bug）

```rust
// v2.1 曾让 Projector/WS/notify 各自订阅 broadcast —— 错：tokio broadcast 慢消费者
// 会 Lagged 丢事件，派生表静默漂移，违反 I6/I12。v2.2 改为串行链：
//   forwarder → DbWriter（唯一消费者，永不 Lagged）→ 写库成功后按 seq 顺序 fan-out
async fn db_writer(rx: Receiver<Event>, db: Db) -> SeqAlloc {
    let mut buf = Vec::with_capacity(256);
    let mut tick = interval(Duration::from_millis(50));
    let mut seq = SeqAlloc::load(&db).await;      // 启动恢复：max(seq)+1（ADR-026）
    loop {
        tokio::select! {
            _ = tick.tick() => { flush(&mut buf, &db, &mut seq, &fanout).await; }
            e = rx.recv() => match e {
                Some(ev) => { buf.push(ev);
                    if buf.len() >= 256 { flush(&mut buf, &db, &mut seq, &fanout).await; } }
                None => { flush(&mut buf, &db, &mut seq, &fanout).await; break; }   // 关停 drain
            }
        }
    }
}
// flush：单事务 INSERT events（分配 seq）→ 事务提交 → 按 seq 顺序 fan_out 给
//        Projector / WS / notify / cognition。写库失败则不 fan-out（事件在下个
//        50ms 窗口重试，Critical 级事件失败 3 次走 spill 重放）。
// fan-out 消费者不许再丢：WS 推送失败只影响该连接，不回压链路。
```

SQLite：`journal_mode=WAL; synchronous=NORMAL;`。50ms/256 条单事务批量 INSERT。
**seq 由单写者内存分配**（I7 使其天然正确），启动时从 `max(events.seq)` 恢复（重启后
monotonic 不回退，ADR-026）；events 表按月轮转（`events_YYYY_MM`，DbWriter 建表/维护
UNION ALL 视图；过期整表 DROP——SQLite 没有原生分区，月表轮转是它的 O(1) 删除等价物；
PG 后端可换原生分区，接口不变）。
**参照教训**：rebucca `pipeline.py` 原注释"高频写库是 SQLite 写锁与页面卡顿的主要来源"。

### 5.3 Projector（投影器，v1 口号的落地）

```rust
// 从 events 增量维护派生表；崩溃恢复 = 从 checkpoint 重放
// v2.2：输入从 broadcast 改为 DbWriter 的顺序 fan-out（§5.2），投影永不缺事件
async fn run(mut rx: OrderedFanout, db: Db) {
    let checkpoint = load_checkpoint(&db);                       // seq 位点
    replay_from(&db, checkpoint.seq).await;                      // 追赶（读 events 表）
    while let Some(ev) = rx.next().await {
        project(ev, &db).await;    // AlarmRaised→upsert alarms；Insight→回填 insight 列；
                                  // Track*→tracks 轨迹追加；RecordingSegment→recordings
        maybe_checkpoint(&db, ev.seq()).await;                   // 每 10min
    }
}
```

**派生表永远可重建**——schema 演进 = 改投影器 + 重放。这是"1 万年"的底气。

**僵尸报警清扫（v2.2 补，ADR-028）**：崩溃时活跃报警停在 alarms 表里没有 cleared_at。
Projector 启动重放后，对 raised_at 超过 `max_alarm_ttl`（默认 10min）仍未 cleared 的行补发
`AlarmCleared{reason: "stale_on_boot"}`——报警语义有界，不留永真报警。

### 5.4 背压与溢出（I12）

`try_send` 满时：Critical 追加写 `spill/` 目录一行 JSON；恢复 task 在 channel 排空后
回灌。Info/Debug 丢弃 + 计数（healthz 可见）。CI 单测：8 路 × 报警风暴，Critical 100% 落库。

---

## 6. 延迟预算（公式 + 实测暴露）

```
运动→报警 = T_motion + T_copy + T_infer + T_rule + T_bus + T_ws
  T_motion  EMA(Y平面 320×180)        ~2ms
  T_copy    NV12→scratch               ~0.3ms
  T_infer   YOLOv8n batch=1 直通       ~60ms CPU / ~15ms GPU
  T_rule    状态机+几何                 ~0.1ms
  T_bus     try_send→DbWriter fan-out  ~0.1ms
  T_ws      WebSocket 推送             ~1-5ms（局域网）
  ─────────────────────────────────────────────
  合计：CPU ~64ms / GPU ~19ms   【目标 <100ms；CI 上限断言 150ms 含抖动余量】
  （校准期 30 帧 ≈1.2s 内不报——宁可漏报不误报，见 ADR-023）

监控→预览 = fMP4 分段(~1s) + WS        ~1.2s（MSE 浏览器 GPU 解码，服务器零解码成本）
报警落库（非关键路径）= 攒批窗口        ≤50ms（不影响报警到达前端）
cognition 洞察（非关键路径）= LLM API    1-3s（异步回填，报警先达，洞察后补）
证据编码（非关键路径）= 证据线程         异步，报警事件先走（ADR-021）
```

每项经 healthz 暴露实测值（AtomicU64 直读）。

---

## 7. 断流状态机（v1 缺失，v2 保留）

```
[connecting] --首帧--> [ok]
[ok] --read_err--> [reconnecting]（指数退避 1s→2s→…→cap 30s）
[reconnecting] --成功--> [ok]；--连续10次失败--> [degraded]
[degraded] --每5min探测--> 成功则 [ok]      【抄 ai-nvr 重连降频】
任意 --stop flag--> [stopped]
```

状态迁移发 `StreamUp/StreamDown` 事件 → 投影器更新 devices.status → WS 推前端；
`degraded` 时从 DetectorPool 注销请求方（拔电的摄像头不占攒批槽位）。
**参照**：rebucca `frames.py` health_snapshot + ai-nvr 重连降频。

---

## 8. 预览链路（fMP4/MSE）

```
前端 <video> ←MSE← WS ← fMP4 ← ffmpeg(-c copy, frag_keyframe+empty_moov+frag_duration=1s)
                  ↑ init segment 缓存（新客户端秒出画面）
                  ↑ 追赶：延迟>2s 渐进 seek（抄 ai-nvr SourceBuffer 管理）
```

- GPU 硬解在浏览器，服务器零解码成本——ai-nvr 验证过的最优路径（`h264-fmp4-muxer.ts`）。
- HEVC 摄像头自动转 H264（libx264 superfast CRF23，抄 ai-nvr HEVC 适配）。
- 按需启停：WS 订阅计数归零即停预览 ffmpeg（`on_stream_none_reader` 语义，ruoyi-zlm hook 同思想）。
- 预览链路挂了**不影响报警**（I4 物理隔离）。

---

## 9. 规则引擎（真表达式树 + 状态机）

```rust
pub enum Condition {
    And(Vec<Condition>), Or(Vec<Condition>), Not(Box<Condition>),
    TrackInZone(ZoneId), LabelIs(String),
    Dwell{ zone: ZoneId, gt_secs: f64 },
    CrossedLine{ line: LineId, dir: Direction },  // 叉积判向（rebucca biz_rules.py cross_line_direction）
    SpeedGt(f64), CountGt(ZoneId, u32),
    TimeIn(ScheduleWindow),
}
pub struct Rule {
    when: Condition, window: Option<Sliding>,    // 滑动窗口聚合（ai-nvr alert/window.ts）
    actions: Vec<Action>,                          // Notify / Snapshot / Record / VerifyWithLlm
    cooldown: Duration,
}
```

- 规则 JSON 存 DB，热更新走 `ConfigChanged` 事件（Agent 改规则也走这条——审计天然完整）。
- **热更新落地（v2.2 补，ADR-027）**：T2 的规则/布控快照是 `ArcSwap<RuleSet>`——控制面写
  `ConfigChanged` 事件后直接 `ArcSwap::store(新快照)`，T2 下一帧 `load()` 拿新引用，旧引用
  的状态机（滞留计时/冷却）**随旧快照一起被丢弃**，新快照从 Idle 重新开始。语义：改规则 =
  重置该路规则状态——简单、无迁移代码；Agent/前端改规则的生效延迟 = 一帧（<40ms）。
- 输出经状态机（I8）：`AlarmRaised` 只在 Idle→Active 跳变发；停留期内按 cooldown 重发可配置。
- 几何核心（点在多边形/叉积越线/角度窗）抄 rebucca `biz_rules.py`——纯函数，单测直接移植其用例。
- **v2.1 解耦（ADR-017）**：rebucca 的 `flow_type 1/2/3/4` 把"用哪个模型"和"要不要 LLM"
  揉在一个字段里——AIVX 拆开：几何条件在 Rule；模型选择在 Algorithm；LLM 复核是 Rule 上的
  一个 `Action::VerifyWithLlm` 或全局 cognition 策略。正交，不再组合爆炸。
- **报警标识（v2.2 补）**：alarm_id = `rule_id + track_id + raised_at_mono` 组合——T2 生成，
  InsightGenerated/AlarmCleared/证据文件都靠它关联，不需要额外查询。

---

## 10. 认知层（cognition，可选）—— v2 丢失，v2.1 补回

**参照**：`frigate-event-handler-master/frigate_event_handler/daemon.py`（事件→抽帧→去重→
vision_model→refine_model→回写）、rebucca `pipeline.py::_llm_verify_track`（复核+冷却）、
ai-nvr `multimodal-analyzer.ts`（每摄像头节流）、frigate `genai/plugins/`（provider 插件化）。

```rust
// 控制面 task：消费 DbWriter 的顺序 fan-out，不阻塞任何报警路径
async fn cognition(mut ev: OrderedFanout, providers: Arc<dyn GenAiProvider>, store: EvidenceStore) {
    while let Some(ev) = ev.next().await {
        let AlarmRaised{ alarm } = ev else { continue };
        if !cognition_enabled(&alarm.rule_id) { continue; }
        if cooldown.hit(&alarm) { continue; }            // per-rule 冷却，默认 6s（抄 rebucca）
        let img = store.wait_jpeg(&alarm.id).await;      // 证据线程产物（ADR-021），最多等 evidence_timeout
        let insight = providers.analyze(&img, prompt_ctx(&alarm.device_id)).await?;
        // Insight{ is_false_positive, threat_level, scene, title, summary }
        ev.publish(InsightGenerated{ alarm_id: alarm.id, insight });  // Critical，走完整落库链
    }
}
```

- `is_false_positive=true` → 投影器把 alarms 行标记 suppressed（报警已送达，标记降级——
  **不撤回**，宁可多报不可漏报）。
- provider 插件：openai 兼容（走你 AIGX 网关）/ ollama / gemini（抄 frigate genai plugins 目录结构）。
- 每摄像头 `prompt_context` 覆盖（"此摄像头朝向后门"，抄 frigate-event-handler）。
- LLM 延迟 1-3s 与报警路径解耦（§6 预算独立）；证据未就绪（evidence_timeout=2s 内
  crop 没编出来）则跳过该次洞察——报警永远先于洞察存在。

---

## 11. 交互层（agent）—— v2 丢失，v2.1 补回

**参照**：AIGX `src/agent/`（mod/runner/session/tools/approval/audit/llm/api）整体平移，换工具注册表。

```rust
pub enum RiskLevel { ReadOnly, LowRisk, HighRisk }   // 抄 AIGX tools.rs 三层分级
// ReadOnly : nvr_list_devices / nvr_list_alarms / nvr_search_recording / nvr_diagnostics
//            （diagnostics 直读 AtomicU64 指标 + 投影器派生表）
// LowRisk  : nvr_start_analysis / nvr_stop_analysis / nvr_set_zone / nvr_snapshot
//            （全部产生 ConfigChanged/AgentAction 事件——审计即事件流，免额外机制）
// HighRisk : nvr_delete_device / nvr_delete_recording（审批矩阵，抄 AIGX approval.rs）
```

自环推理抄 AIGX `agent/llm.rs`：进程内直调 bridge（走 AIGX 网关渠道），不经 HTTP 端口不计费。

---

## 12. 协议层（aivx-net）—— v2 丢失，v2.1 补回

**参照**：`open-nvr-main/server/services/camera_drivers/base.py`（"不支持是数据不是异常" +
**结构上不存在 set_ip/factory_reset**——调用不存在的方法即不可能砖机）与 `registry.py`
（driver 选择：缓存 → 持久化 driver_name → manufacturer 匹配+probe → ONVIF 兜底）。

```rust
#[async_trait]
pub trait DeviceAdapter: Send + Sync {
    async fn discover(&self) -> Result<Vec<DeviceCandidate>>;      // ONVIF WS-Discovery 多播
    async fn get_streams(&self, d: &Device) -> Result<Streams>;    // GetProfiles→GetStreamUri
    async fn capabilities(&self, d: &Device) -> Result<Capabilities>;  // I10：ptz/events/imaging
    async fn ptz(&self, d: &Device, cmd: PtzCmd) -> Result<Supported>;
    async fn snapshot(&self, d: &Device) -> Result<Vec<u8>>;
}
// Supported = Yes(数据) / No(数据)，只有 auth/transport 才是 Err —— 抄 open-nvr base.py
```

- `Capabilities` JSONB 存 devices 表，前端动态渲染（支持 PTZ 才显示云台）。
- TP-LINK 首发路径：ONVIF Profile S + RTSP（`stream1` 主 / `stream2` 子）。
- GB28181 留 P7（要接平台级联才做，信令抄 ruoyi-gb28181 `transmit/` 观察者分发）。

---

## 13. 数据模型 —— v2 丢失，v2.1 补回

**参照**：AIGX `src/db/entity/channel.rs`（SeaORM entity 写法）、rebucca `app/models.py`（业务字段）、
ruoyi `sql/ry-cloud.sql`（协议字段教训——大表全字段是反例）。

| 表 | 关键字段 | 说明 |
|---|---|---|
| `devices` | id(uuid), name, access_type, onvif_url, rtsp_main, rtsp_sub(I4), username, password_enc(`enc:` AIGX 加密), manufacturer, model, capabilities(JSONB, I10), record_enable, status, created_at, updated_at | 设备 |
| `algorithms` | id, name, algo_type(yolo8/11/26…), task_type, engine(ort…), device(cpu/cuda), model_file, input_w/h, conf, iou, labels(JSONB), is_default, state | 小模型（抄 rebucca av_algorithm 字段） |
| `zones` | id, device_id, name, coords(JSONB 归一化), line_a, line_b, is_required | 几何 |
| `rules` | id, device_id, name, when(JSONB Condition 树), window(JSONB), actions(JSONB), cooldown_ms, state | §9 规则引擎 |
| `events_YYYY_MM` | seq(全局, 单写者分配), device_id, type, payload(JSONB), occurred_at | **唯一事实源(I6)**，月表轮转 |
| `alarms`（投影） | alarm_id, rule_id, device_id, zone_id, track_id, label, box, score, raised_at, cleared_at, suppressed, insight_*（cognition 回填列）, evidence_path | Projector 维护 |
| `tracks`（投影） | track_key, device_id, label, first_seen, last_seen, trajectory(JSONB 点列) | Projector 维护 |
| `recordings`（投影） | id, device_id, file_path, start_ts, end_ts, duration, size, has_motion | T3 段事件投影 |
| `llm_configs` | id, name, provider, api_url, model, key_enc, timeout | cognition provider |
| `users` | AIGX `src/auth/` + `db/entity/user.rs` 平移 | RBAC admin/user |

**关系**：device 1—N zones；rules 引用 zone/algorithm id；所有可重建表（alarms/tracks/recordings）
禁止业务代码直写——只能由 Projector 写（I6 的 schema 级强制：业务代码根本没有它们的 Entity）。

---

## 14. 可观测 —— v2 丢失，v2.1 补回

**参照**：AIGX `src/metrics.rs` + `src/health.rs`（成熟实现直接平移）+ rebucca `pipeline.status()`。

- 数据面每路发布 AtomicU64：`decode_fps / analyze_fps / dropped_gen / motion_latency /
  infer_latency / alarm_latency / stream_health(状态机枚举) / reconnects`。
- 控制面采集 task（1s）读 AtomicU64 → 组装 `/api/healthz`（JSON）+ `/api/metrics`
  （Prometheus 文本，AIGX metrics.rs 格式）。
- tracing 结构化日志（AIGX `src/log.rs` 平移），**凭据脱敏**（`rtsp://user:***@`）。

---

## 15. 安全 —— v2 丢失，v2.1 补回

- **凭据加密**：`enc:` 前缀 + AES-GCM（密钥来自 config secret）——AIGX `src/storage/` 实践平移。
- **鉴权**：JWT + RBAC（admin/user），`/api/*` 全鉴权——AIGX `src/auth/` 平移；WS 预览带一次性 token。
- **默认离线**（PRIVACY.md 承诺的架构保证）：无 LLM/notify 配置时，进程无任何外联路径——
  cognition/notify 是唯一持有 reqwest 的模块，不配置即不构造。
- **日志脱敏**：摄像头密码、LLM key 永不落日志。
- **危险操作结构不可达**：DeviceAdapter 无 set_ip/factory_reset（抄 open-nvr）。

---

## 16. 非目标（明确不做的，同样要写下来）

| 不做 | 理由 | 何时重启 |
|---|---|---|
| 音频事件检测 | frigate 有（`frigate/audio-labelmap.txt`），但麦克风摄像头少、事件流无此类型需求 | devices.capabilities 出现 audio 再议 |
| 多服务器集群 | 单写者 seq / 抖动假设全是单机的；分布式需 per-shard seq + 合并器（ADR 留痕） | 单机撑不住时 |
| WebRTC 预览 | fMP4/MSE 已达 ~1.2s；WebRTC 需 ZLM 整栈 | 延迟要求 <500ms 时 |
| 厂商私有 SDK | ONVIF 覆盖 90% 场景；SDK 是海康/大华 FFI 深坑 | 特定型号 ONVIF 缺能力时（feature gate） |
| GB28181 级联 | 自用 TP-LINK 直连 ONVIF 足够 | 要接公安/平台级联时（P7） |

---

## 17. 优雅关停顺序

```
SIGTERM →
  1. ControlFlags.stop = true（数据面各线程退出循环；T1 kill ffmpeg，丢半帧可接受）
  2. forwarder drain：sync_channel 剩余事件（含 spill 回灌）全部交给 DbWriter 后 drop 发送端
  3. DbWriter 收 None → 最后一次 flush（事务提交 + fan-out 完毕）→ 退出
  4. Projector 消费完 fan-out 尾部 → 写最终 checkpoint
  5. 预览 ffmpeg 终止（段收尾 500ms 上限）
  总预算 <2s；超时强杀并记录未落库计数
```

---

## 18. 时间与标识

- **混合时钟**：帧/延迟测量用**单调钟**（Instant→纳秒，不受 NTP 跳变）；events.occurred_at
  用**墙钟**（展示/回放）；启动时校准一次 `mono_origin`。
- ID：设备/规则 UUID v4；track/seq u64（排序友好）。

---

## 19. 不变量的机器强制（汇总）

| 不变量 | 手段 |
|---|---|
| I1 分配上限 | dhat/计数分配器包住单帧循环，断言 `allocs ≤ N`（报警帧同样——证据编码已在证据线程，ADR-021） |
| I3 延迟 | 合成 NV12 流全链路计时 <150ms（criterion + CI；含校准期后的首运动帧） |
| I5/I11 crate 边界 | `cargo tree -i tokio -p aivx-perception` 为空；aivx-events 仅 serde |
| I6/I12 事件不丢 | **顺序 fan-out 回归测试**：灌满下游后断言 Projector 收到全部 seq（无洞）；灌满 channel 后 Critical 经 spill 100% 达 DbWriter |
| I8 去重 | 连续帧同目标单测：仅 1 条 AlarmRaised |
| 帧撕裂 | 模糊测试：随机截断 ffmpeg 输出，断言 read_exact 失败被状态机接住 |
| 断流 | 模拟 EOF/超时，断言状态机路径与 StreamDown/Up 事件序列 |
| 校准期（ADR-023） | 重连后 30 帧内注入运动，断言无 AlarmRaised |
| 热更新（ADR-027） | ArcSwap 替换规则集，断言下一帧生效 + 状态重置 |
| 僵尸清扫（ADR-028） | 预置超 TTL 活跃报警行，重启 Projector，断言补发 AlarmCleared |

---

## 20. ADR 登记（v2 的 008~015 + v2.1 的 016~020 保留，v2.2 新增 021~028）

| ADR | 决策 | 推翻/修正 | 参照 |
|---|---|---|---|
| 001-007 | 单进程 / 事件溯源 / 帧池 / 规则树 / 能力驱动 / 配置层 / 双码流（v1） | — | 见 v1 |
| **008** | 双平面：数据面同步 OS 线程，控制面 tokio | v1"全 tokio" | 60ms 推理饿死 worker |
| **009** | LatestFrameSlot（seqlock 双缓冲） | v1 FIFO 队列 | latest-wins 语义 |
| **010** | 诚实 I1：推理前一次 NV12→scratch 拷贝 | v1"绝对零拷贝" | 借用检查正确性 |
| **011** | 投影器 + checkpoint 落地物化视图 | v1 口号 | 崩溃恢复=重放 |
| **012** | 事件分级 + spill 背压 | v1 无背压 | 报警风暴 |
| **013** | fMP4/MSE 预览链路 | v1 空白 | ai-nvr h264-fmp4-muxer |
| **014** | events 月表轮转 + 单写者内存 seq | v1 append-only 无生命周期 | SQLite 无原生分区 |
| **015** | 混合时钟（单调+墙钟） | v1 无时间设计 | NTP 跳变下的测量正确性 |
| **016** | 报警证据 = crop JPEG，按报警次数编码（非每帧） | v2 遗漏 per-alarm 语义 | frigate-event-handler 抽帧时机 |
| **017** | 拆掉 rebucca flow_type：几何(Rule)/模型(Algorithm)/LLM(Action) 正交 | rebucca 组合爆炸 | rebucca models.py FLOW_CHOICES |
| **018** | cognition 在控制面异步，洞察回填不撤回报警 | v2 章节丢失 | 宁多报不漏报 |
| **019** | 数据面用 std::process（非 tokio::process） | **v2 技术错误** | AsyncChildStdout 无法阻塞 read_exact |
| **020** | 平面桥用 std::sync_channel + spill（非 tokio mpsc），perception 禁 tokio | **v2 技术错误** | I11 编译期强制 |
| **021** | 证据编码移出 T2 热路径：EvidenceRing（每路最近 N 帧 NV12 环形引用）+ 低优先级证据线程，T2 只投 `(alarm_id, box, gen)` | **v2.1 技术错误**：JPEG 编码 1-5ms 在 16fps 预算里是 5-16% | I1 的 per-alarm 例外也要出热路径 |
| **022** | 事件链从 broadcast 改为 DbWriter 顺序 fan-out | **v2.1 正确性 bug**：broadcast Lagged 丢事件 → 派生表静默漂移 | I6/I12 要求投影永不缺事件 |
| **023** | 运动校准期（重连后 30 帧不触发检测） | v2.1 缺失 | frigate `frame_counter < 30`；宁漏报不误报 |
| **024** | 推理期间 T2 不消费新帧（分析 FPS ≈16 为显式设计值） | v2.1 隐含未声明 | latest-wins 自然丢帧；双 slot 否决 |
| **025** | DetectorPool 按引擎键缓存模型 + 每键一推理线程 | **v2.1 假设错误**："全进程 1 份模型"否定多算法规则 | rebucca worker_pool.py 键设计 |
| **026** | seq 启动恢复：`max(events.seq)+1`，重启不回退 | v2.1 单写者内存 seq 无崩溃恢复 | 事件溯源的持久化缺口 |
| **027** | 规则热更新 = `ArcSwap<RuleSet>` + 状态重置（无迁移代码） | v2.1 的"热更新走 ConfigChanged"无落地机制 | ArcSwap 无锁读；改规则=重置状态 |
| **028** | 僵尸报警清扫：Projector 启动时对超 TTL 未 cleared 的行补 AlarmCleared | v2.1 缺失：崩溃留下永真报警 | 报警语义有界 |
| **029** | GB28181 Digest 鉴权必须 md-5 crate（真实 RFC 1321） | v2 占位 md5 非真哈希 | RFC 2617 + RFC 1321 已知向量 |
| **030** | ort YOLO 后端 = `OrtYoloBackend`（feature gate `ort-yolo`，CI 跑纯逻辑测试不下载模型） | MotionStubAnalyzer 桩 | ADR-025 键缓存 + 批前向 `[N,3,640,640]` |
| **031** | P8 去桩化完成：ONVIF SOAP / DetectorPool / GB28181 传输+Digest / Agent 数据源 / cognition LLM / ort YOLO / 前端 7 桩全真实化 | 蓝图 vs 实现差距清零 | CI 4 job（Lint/Test/Invariant/Frontend/Build）全绿 |

---

## 21. 性能预算

| 指标 | 目标 | 基线 | 验证 |
|---|---|---|---|
| 运动→报警延迟 | **<100ms CPU / <30ms GPU** | rebucca 秒级（轮询+JPEG） | CI 合成流 150ms 上限 |
| 单路 CPU | <0.15 核 | rebucca ~0.5-1 核 | healthz 实测 |
| 8 路 CPU | <1.5 核 | frigate 4-6 核 | 部署实测 |
| 8 路内存 | <500MB（模型 1 份） | rebucca 2-3GB | 部署实测 |
| 单帧堆分配（热路径） | dhat 断言 ≤N | rebucca ≥3 次 | CI |
| 预览延迟 | ~1.2s | ai-nvr 同级 | 浏览器实测 |
| 报警风暴 Critical 丢失 | 0 | rebucca 卡死 | CI 背压单测 |

---

## 22. 反例清单（v1 的 1-7 + v2 的 8-12 + v2.1 的 13-16 保留，v2.2 追加）

17. **v2.1 的证据编码在 T2 热路径里**——`encode_crop` 1-5ms 吃掉 16fps 预算的 5-16%，
    违反自己写的 I1 精神。ADR-021 移到证据线程。
18. **v2.1 的事件链用 broadcast**——慢消费者 Lagged 丢事件，派生表静默漂移，整个
    事件溯源的正确性被一个错误的原语毁掉。ADR-022 改顺序 fan-out。**教训：溯源架构里
    任何"可能丢"的环节都是体系性错误，不是调参问题。**
19. **v2.1 的"全进程 1 份模型"**——多算法规则（人车 A 模型+车牌 B 模型）下不成立。
    ADR-025 按引擎键缓存。
20. **v2.1 的热更新只有口号**——"走 ConfigChanged 事件"但 T2 怎么拿到新规则没写。
    ADR-027 用 ArcSwap 落地。
21. **无校准期**——摄像头刚连上就跑检测，EMA 背景没建好必然误报。ADR-023。
22. **崩溃恢复语义缺口**——seq 内存分配无恢复（ADR-026）、活跃报警变永真（ADR-028）。
    崩溃路径不设计，重启就是行为未定义。

---

## 23. 路线图（每阶段带参照与验证）

| 阶段 | 目标 | 参照 | 验证 |
|---|---|---|---|
| **P0 骨架** | workspace 四 crate + LatestFrameSlot + PlaneBridge + DbWriter + Projector + 合成流 CI | AIGX main/config；§2/§5 设计 | dhat 断言绿；cargo tree 断言绿 |
| P1 接入 | ONVIF discover/GetStreams + T1 拉流 + EMA 运动 + fMP4 预览 | open-nvr base.py；frigate ffmpeg.py/frigate_motion.py；ai-nvr h264-fmp4-muxer.ts | TP-LINK 出画面+运动框 |
| P2 感知 | ort YOLO + ByteTrack + 规则引擎 + 报警端到端 | rebucca engines/base.py、biz_rules.py；ai-nvr tracker | 报警 <150ms；I8 去重单测 |
| P3 录像 | T3 分段 + 段索引投影 + 保留清理 | rebucca recording/manager.py | 24/7 录像回放 |
| P4 认知 | crop 证据 + provider + InsightGenerated 回填 | frigate-event-handler daemon.py；ai-nvr multimodal-analyzer.ts；frigate genai/plugins | 误报标记生效（不撤回） |
| P5 Agent | AIGX agent 平移 + NVR 工具注册表 | AIGX src/agent/ | 自然语言查报警 |
| P6 多品牌+PTZ | registry probe + capabilities 渲染 | open-nvr registry.py/base.py | 换品牌零代码 |
| P7 GB28181（按需） | SIP 观察者 + Digest 鉴权 + 点播 | ruoyi-gb28181 transmit/ | 接平台级联 |

---

## 24. 自检结论（v3——去桩化完成）

- **完整性**：v2 丢失子系统全部补回；v2.2 补齐校准期/崩溃恢复/热更新/僵尸清扫/证据线程；**v3 补齐 7 桩真实化**（ONVIF SOAP/DetectorPool/GB28181 传输+Digest+MD5/Agent 数据源/cognition LLM/ort YOLO/前端）。
- **一致性**：12 条不变量与 §19 的强制手段一一对应；I5/I11 是 crate 编译边界；I6/I12 经 ADR-022 架构保证（事件链无可丢环节）；**I10 前端能力驱动渲染经 vitest 机器验证**。
- **诚实性**：承认推理前一次拷贝（ADR-010）、证据出热路径（ADR-021）、SQLite 无分区（ADR-014）、单模型假设错误（ADR-025）、md5 占位（ADR-029 修）、MotionStubAnalyzer 桩（ADR-030 修）——挑不出毛病的前提是先把自己毛病全挑出来。
- **可验证**：每个数字有公式（§6）+ 上限断言（§19）+ 暴露端点（§14）；崩溃路径有定义行为（ADR-023/026/028）；**ort-yolo 纯逻辑测试在 CI 实际运行**（`cargo test --features ort-yolo yolo::`）。
- **可演进**：派生表永远可重建（§5.3）、非目标重启有明确条件（§16）、**31 条 ADR 全程留痕**（§20）。

> **P0-P8 路线图全部实现并经 CI 机器验证**（Lint/Test/Invariant/Frontend/Build 五 job 全绿）。
> 蓝图定稿——每个架构声明都有实现 + 机器验证支撑，是"挑不出毛病"的证据基础。
