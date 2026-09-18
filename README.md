# AIVX — AI Video eXtended

<div align="center">

**高性能 · 自托管 · 三层 AI 的网络视频录像机**

[![Rust](https://img.shields.io/badge/Rust-stable-orange)](https://www.rust-lang.org)
[![React](https://img.shields.io/badge/React-18+-61DAFB)](https://react.dev)
[![License](https://img.shields.io/badge/License-Free_Personal_Use-blue)](#许可证)
[![Version](https://img.shields.io/badge/Version-0.1.0-blue)](#快速开始)

[English](#english) · [中文](#中文)

</div>

---

> **你的摄像头，你的 AI，你的硬件，你的控制。**
>
> AIVX 是一款用 Rust 编写的高性能自托管 AI NVR：连接你的 ONVIF/RTSP 摄像头，
> 在本地跑 YOLO 目标检测，让 AI 描述*到底发生了什么*——没有云、没有厂商、没有
> 按摄像头计的 SaaS 账单。从一台 TP-LINK 摄像头到多品牌摄像头集群都适用。

---

## 项目简介

AIVX（AI Video eXtended）是高性能自托管 AI 网络视频录像机，延续 [AIGX](https://github.com/ojbkxc/AIGX)
（Rust AI 网关）的技术栈与工程纪律，融合业界成熟监控项目的架构思想（Frigate 的运动门控、
Rebucca 的布控规则、ai-nvr 的 AI 功能设计、OpenNVR 的 AI 插件化思想、RuoYi 的协议接入分层）。

### 三层 AI 架构

AIVX 的 AI 不是"接个大模型"这么简单，而是三层递进，每一层都可独立启停：

```
┌─────────────────────────────────────────────┐
│ 第三层：交互 AI（Agent 运维）                │  ← 抄 AIGX agent，换 NVR 工具
│  自然语言运维：查报警/改布控/查回放          │
├─────────────────────────────────────────────┤
│ 第二层：认知 AI（LLM 复核 + 语义描述）       │  ← 抄 frigate-event-handler + ai-nvr
│  误报过滤 / 威胁分级 / 报警文案              │
├─────────────────────────────────────────────┤
│ 第一层：感知 AI（本地小模型检测）            │  ← 抄 rebucca + ai-nvr
│  YOLO 目标检测 + 多目标跟踪 + 运动门控      │
└─────────────────────────────────────────────┘
```

- **感知层**（`perception`）：YOLO（`ort` ONNX Runtime）目标检测 + ByteTrack/IoU 跟踪 + EMA 运动门控。**完全本地，默认开启，无外部依赖。**
- **认知层**（`cognition`）：事件结束 → 裁剪关键帧 → 送 OpenAI 兼容 LLM → 误报过滤 / 威胁分级 / 语义描述。**可选，需配置 API。**
- **交互层**（`agent`）：自然语言问摄像头——"昨晚 11 点后院有什么异常？"、"把 2 号摄像头改成只报人"。**可选，复用你 AIGX 的渠道。**

---

## 核心特性

### 视频接入（多品牌）

- **ONVIF 发现**：局域网自动扫描摄像头（WS-Discovery），跨品牌通用（TP-LINK / 海康 / 大华 / 宇视...）
- **RTSP 拉流**：主码流录像 + 子码流分析分离
- **GB28181 信令**（规划中）：SIP 点播 / 回放 / 级联
- **厂商驱动抽象**：`DeviceAdapter` trait，ONVIF 通用 + 未来厂商 SDK 可选

### 智能分析（感知层）

- **运动门控**：EMA 背景建模，无运动时跳过检测（省 95% 检测 CPU）
- **目标检测**：YOLOv8n（`ort`），CPU/GPU 均可
- **多目标跟踪**：ByteTrack（IoU + min_hits 防幽灵 ID）
- **布控规则**：区域入侵 / 越线 / 越线计数 / 方向 / 密度 / 滞留（可配置规则引擎）
- **报警快照**：检测框 + 区域多边形标注，节流存储

### 认知与交互（可选）

- **误报复核**：小模型命中 → LLM 判定真伪 → 通过才报警
- **语义描述**：事件生成自然语言文案（"凌晨 2 点有可疑人员在后院徘徊 5 分钟"）
- **AI 运维 Agent**：自然语言查报警 / 改布控 / 诊断断流 / 汇总日报

### 录像与回放

- **24/7 录像**：ffmpeg `-c copy` 分段直存（零转码，CPU 开销≈0）
- **保留策略**：按天数 / 容量上限自动清理
- **录像索引**：SQLite 记录分段元数据，前端时间轴回放

### 通知与监控

- **报警推送**：Webhook / 邮件 / Telegram（`NotifyChannel` trait，可扩展）
- **WebSocket 实时**：检测框 / 报警 / 事件推送
- **状态监控**：每路摄像头流健康 / FPS / 分析延迟

### 架构特性

- **Rust 后端**：Axum 0.7 + Tokio 异步运行时，单二进制交付
- **React 前端**：React 18 + TypeScript + Vite + Tailwind，管理后台 + 实时预览
- **存储灵活**：默认 SQLite（零配置）；可选 SeaORM 接入 PostgreSQL
- **多平台**：Linux / Windows / macOS，AMD64 / ARM64
- **零依赖部署**：单二进制 + 静态前端，无需外部运行时（除 ffmpeg / ZLMediaKit 可选）

---

## 快速开始

> **开发状态**：当前为 0.1 骨架阶段，快速开始为规划路径。

### 从源码构建

```bash
# 后端（默认 SQLite，零外部依赖）
cargo build --release
./target/release/aivx

# 前端（产物输出到 ../static，由后端同目录托管）
cd frontend
npm install && npm run build
```

### 环境依赖

- **ffmpeg**（必须）——拉流 / 录像。PATH 可访问即可。
- **ZLMediaKit**（可选）——低延迟流媒体转发（WebRTC/WS-FLV），不装则用 ffmpeg 直连。
- **ONNX 模型**（可选，感知层）——YOLOv8n `.onnx` 文件，放入模型目录。

### 配置

首次启动生成 `config.toml`（默认 `~/.aivx/config.toml`）：

```toml
[server]
host = "127.0.0.1"
port = 8080

[data]
dir = "~/.aivx"          # 数据库 + 录像
record_dir = "~/.aivx/record"

[detect]
enabled = true
model = "yolov8n.onnx"
conf_threshold = 0.4
analyze_fps = 5

[llm]                    # 可选：认知层
enabled = false
api_url = "https://api.openai.com/v1"
model = "gpt-4o-mini"

[agent]                  # 可选：交互层
enabled = false
```

---

## 架构

```
aivx/
├─ src/
│  ├─ api/           管理面 /api/*（axum 0.7）
│  ├─ config/        配置加载 + 热重载
│  ├─ perception/    感知层：detect / track / motion / rules
│  ├─ cognition/    认知层：LLM 复核 + 语义描述
│  ├─ agent/         交互层：AI 运维 Agent（抄 AIGX agent）
│  ├─ notify/        报警推送（Webhook/邮件/Telegram）
│  ├─ storage/       录像索引 / 快照 / 保留策略
│  ├─ db/            SeaORM entity / migration
│  └─ ...
├─ aivx-net/         协议层：onvif / rtsp / gb28181（独立 crate）
├─ frontend/         React + TS + Vite
├─ static/           前端构建产物（提交入库）
└─ .github/workflows/  CI（抄 AIGX）
```

**性能设计**（详见 `DESIGN.md`）：
- 帧路径目标 0 拷贝（预分配帧池 + 借用）
- 运动门控在检测前，无运动不跑 YOLO
- 子码流分析 + 主码流录像
- 报警写库走异步批量 channel，分析循环不碰 DB

---

## 路线图

| 阶段 | 目标 | 参考 |
|---|---|---|
| P1 骨架 | workspace + config + SeaORM + axum CRUD | AIGX |
| P2 拉流+运动 | ONVIF 发现 + RTSP + EMA 运动 + WS 推帧 | frigate / rebucca |
| P3 检测+跟踪 | ort YOLO + ByteTrack + 框推送 | rebucca / ai-nvr |
| P4 布控+报警 | 规则引擎 + 快照 + 报警 + 推送 | rebucca / ai-nvr |
| P5 录像 | ffmpeg 分段 + 保留策略 + 回放 | rebucca |
| P6 认知层 | LLM 误报复核 + 语义描述 | frigate-event-handler / ai-nvr |
| P7 Agent | AI 运维 Agent（复用 AIGX） | AIGX |
| P8 GB28181 | SIP 信令 + 点播 | ruoyi |

---

## 参考项目

AIVX 站在这些开源项目的肩膀上：

| 项目 | 借鉴 |
|---|---|
| [AIGX](https://github.com/ojbkxc/AIGX) | Rust 骨架 / Agent 框架 / 工程纪律 |
| [Frigate](https://github.com/blakeblackshear/frigate) | 运动门控 / 检测器插件 / 目标状态机 |
| [Rebucca](https://gitee.com/Vanishi/rebucca) | 布控规则 / 推理池 / 流水线 |
| [ai-nvr](https://github.com/2234839/ai-nvr) | CLIP 语义标签 / ByteTrack / 告警引擎 |
| [OpenNVR](https://github.com/open-nvr/open-nvr) | AI 插件化思想 / camera driver 抽象 |
| [RuoYi-QS-NVR](https://github.com/2929004360/ruoyi-qs-nvr) | 协议接入分层 / ZLM hook / GB28181 |

---

## 许可证

本项目源代码对**个人与非商业用途免费开放**（学习、研究、自用部署与修改）。
**任何商业用途**——包括对外售卖、SaaS 化运营、集成到商业产品或提供付费服务——
均须事先取得作者书面授权。详见 [PRIVACY.md](./PRIVACY.md) 了解数据处理规范。

---

## English

AIVX (AI Video eXtended) is a high-performance, self-hosted AI network video recorder written in Rust.

> **Your cameras. Your AI. Your hardware. Your control.**

A self-hosted, performance-first AI NVR that connects your ONVIF/RTSP cameras, runs local YOLO detection, and lets AI describe *what actually happened* — no cloud, no vendor, no per-camera SaaS bill.

### Three-Layer AI

- **Perception** (`perception`) — YOLO detection + multi-object tracking + motion gating. Fully local, on by default.
- **Cognition** (`cognition`) — optional LLM false-positive review + semantic description of events.
- **Interaction** (`agent`) — an AI ops agent that answers natural-language questions about your cameras.

### Highlights

- Rust (Axum 0.7 + Tokio), single binary, SQLite default (PostgreSQL optional)
- React 18 + TypeScript + Vite frontend
- ONVIF discovery + RTSP, multi-brand camera support
- Zero-copy frame path, motion gating, substream analysis
- Configurable rule engine (area / line-cross / direction / density / dwell)
- 24/7 recording with retention policy
- Webhook / email / Telegram alert push
- AI ops agent (reusing your AIGX channels)

### Quick Start

> Development status: currently 0.1 scaffold stage.

```bash
cargo build --release
./target/release/aivx
cd frontend && npm install && npm run build
```

Requires `ffmpeg` on PATH. Optional: ZLMediaKit, ONNX YOLO model, LLM API config.

### License

Free for personal and non-commercial use (learning, research, self-hosting, modification).
Any commercial use requires prior written authorization from the author. See [PRIVACY.md](./PRIVACY.md).