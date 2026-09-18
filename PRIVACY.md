# PRIVACY.md — AIVX

> AIVX 项目隐私政策与数据处理规范。
>
> 本文件同时提供[中文版](#中文版)与[English](#english)。

---

## 中文版

**最后更新：2026年9月19日**

AIVX 是一款**自托管**网络视频录像机（NVR），运行在你自己拥有的硬件上。与商业云摄像头/云 NVR 不同，**AIVX 不运营任何后端服务器，不上传你的视频到任何云**。本政策说明 AIVX 处理哪些数据、数据存放在哪里、哪些数据可能离开你的机器、以及哪些第三方可能看到它。

### 数据收集

**AIVX 不收集、不存储、不向开发者传输任何个人数据。我们不运营任何云端后端。**

- **视频流**——摄像头画面**只存储在你自己的磁盘上**（录像文件、报警快照）。它不会被发送到 AIVX 开发者，不会被发送到任何云端。视频仅在以下情况离开你的机器：
  - 你主动通过浏览器/前端观看实时画面或回放（此时画面在你的局域网或你配置的访问范围内传输）；
  - 你主动配置了报警推送（Webhook / 邮件 / Telegram 等），此时报警截图/描述会发送到你配置的接收端。
- **摄像头凭据**——RTSP 地址、ONVIF 用户名/密码、GB28181 凭据**存储在你自己的数据库里**，仅用于连接你拥有的摄像头。存储时做加密处理（见下文「数据保护」）。
- **AI 推理**——本地小模型（YOLO 目标检测）**完全在你自己的 CPU/GPU 上运行**，帧数据不出机器。可选的 LLM 语义分析/误报复核仅在**你主动配置**了 OpenAI 兼容 API 时才启用，此时仅将**裁剪出的关键帧**发送到你配置的 API 端点。
- **不收集个人身份信息（PII）**——AIVX 不收集设备标识符、广告 ID、电话号码、邮箱、账号或位置信息。无分析 SDK，无远程崩溃上报。

### 数据存储位置

所有用户数据均**位于你自己的机器上**：

| 数据 | 存储 |
| --- | --- |
| 录像文件、报警快照、缩略图 | 你配置的录像目录（默认 `data/record`） |
| 设备/算法/布控/报警索引 | 本地 SQLite（可切 PostgreSQL） |
| 配置、密钥、凭据 | 本地配置文件 + 加密存储 |

不会上传至 AIVX 运营的服务器。卸载/删除数据目录将删除所有本地数据。

### 数据离开你的机器的情形

AIVX 默认**完全离线**。以下数据流**仅在你有意配置时**才会发生，且只流向你指定的端点：

| 数据流 | 何时发生 | 发送到哪里 |
| --- | --- | --- |
| 报警推送 | 你配置了 Webhook / 邮件 / Telegram | 你配置的接收端 |
| LLM 语义分析 | 你启用了 cognition 层并配置了 API | 你配置的 OpenAI 兼容 API |
| Agent 运维对话 | 你使用了 AI 运维 Agent | 你配置的 LLM 渠道 |

**关键承诺**：在没有任何主动配置的情况下，AIVX 不向互联网发送任何数据。视频帧、摄像头凭据、报警记录在默认配置下完全不会离开你的机器。

### 数据保护

- **摄像头凭据加密存储**——RTSP 密码、ONVIF 密码等敏感字段以加密形式存储（`enc:` 前缀 + 密钥派生加密，沿用 AIGX 的实践），不以明文落盘。
- **管理面鉴权**——Web 管理面板需要登录（RBAC：admin / user），所有 `/api/*` 接口鉴权。
- **传输安全（可选）**——通过反向代理 + TLS 提供 HTTPS 访问（生产部署建议）。
- **最小权限**——AIVX 只请求连接摄像头所需的网络权限，不扫描、不上传、不遥测。

### AI 与隐私

AIVX 的三层 AI 架构在隐私上递进：

1. **感知层（本地小模型）**——YOLO 检测**完全本地**，帧数据不出机器。**这是默认**，无任何外部依赖。
2. **认知层（可选 LLM）**——仅在你配置了 OpenAI 兼容 API 时启用。发送的是**裁剪后的关键帧**（不是全帧流），且受冷却/节流限制。
3. **交互层（Agent）**——AI 运维 Agent 使用你配置的 LLM 渠道。对话内容发给该渠道，AIVX 不记录对话内容本身。

**你可以随时关闭第二、三层**，感知层（本地检测）始终独立运行。

### 第三方服务

- **LLM API（可选）**——仅在启用认知层/Agent 时使用。请查阅你选择的服务商的隐私政策。
- **摄像头厂商**——AIVX 通过 ONVIF/RTSP 与你的摄像头通信。请求直接从你的机器发往摄像头（局域网），AIVX 不中转。请查阅摄像头厂商的政策。

### 数据保留与删除

- 录像按**保留策略**自动清理（默认按天/按容量上限，可配置）。
- 你可以在 UI 中删除单条报警、单段录像、整个设备。
- 删除数据目录即清除全部数据。

### 儿童隐私

AIVX 不面向 13 岁以下儿童。

### 变更

本政策可能不时更新，更新内容将发布于此页面。

### 联系

如有问题，请在 [github.com/ojbkxc/AIVX](https://github.com/ojbkxc/AIVX) 提交 issue。

---

## English

**Last updated: September 19, 2026**

AIVX is a **self-hosted** network video recorder (NVR) that runs on hardware you own. Unlike commercial cloud cameras/cloud NVRs, **AIVX operates no backend servers and uploads none of your video to any cloud**. This policy explains what data AIVX handles, where it lives, what may leave your machine, and which third parties may see it.

### Data Collection

**AIVX does not collect, store, or transmit any personal data to its developer. We operate no cloud backend.**

- **Video streams** — camera footage is stored **only on your own disk** (recordings, alert snapshots). It is never sent to the AIVX developer or to any cloud. Video leaves your machine only when:
  - You actively watch live/playback through a browser (transmitted within your LAN or the access scope you configure);
  - You actively configured alert push (Webhook / email / Telegram etc.), in which case the alert snapshot/description is sent to the endpoint you configured.
- **Camera credentials** — RTSP URLs, ONVIF usernames/passwords, GB28181 credentials are stored **in your own database**, used only to connect to cameras you own. They are encrypted at rest (see *Data Protection*).
- **AI inference** — local small models (YOLO object detection) run **entirely on your own CPU/GPU**; frame data never leaves the machine. Optional LLM semantic analysis / false-positive review is enabled **only if you configure** an OpenAI-compatible API; only the **cropped key frame** is sent to the endpoint you configured.
- **No PII** — AIVX does not collect device identifiers, advertising IDs, phone numbers, email addresses, account names, or location. No analytics SDK, no remote crash reporting.

### Data Storage Location

All user data lives **on your own machine**:

| Data | Storage |
| --- | --- |
| Recordings, alert snapshots, thumbnails | Your configured recording directory (default `data/record`) |
| Device / algorithm / zone / alert index | Local SQLite (optional PostgreSQL) |
| Config, secrets, credentials | Local config files + encrypted storage |

Nothing is uploaded to an AIVX-operated server. Uninstalling / deleting the data directory removes all local data.

### When Data Leaves Your Machine

AIVX is **fully offline by default**. The following data flows occur **only when you deliberately configure them**, and only flow to endpoints you specify:

| Data flow | When it happens | Sent to |
| --- | --- | --- |
| Alert push | You configured Webhook / email / Telegram | Your configured receiver |
| LLM semantic analysis | You enabled the cognition layer and configured an API | Your configured OpenAI-compatible API |
| Agent ops conversation | You used the AI ops Agent | Your configured LLM channel |

**Key commitment**: with no active configuration, AIVX sends nothing to the internet. Video frames, camera credentials, and alert records never leave your machine in the default configuration.

### Data Protection

- **Encrypted camera credentials** — sensitive fields (RTSP passwords, ONVIF passwords) are stored encrypted (`enc:` prefix + key-derivation encryption, following AIGX practice), never in plaintext.
- **Admin auth** — the web UI requires login (RBAC: admin / user); all `/api/*` endpoints are authenticated.
- **Transport security (optional)** — HTTPS via reverse proxy + TLS for production deployments.
- **Least privilege** — AIVX requests only the network access needed to reach your cameras; no scanning, no uploads, no telemetry.

### AI and Privacy

AIVX's three-layer AI architecture is progressively private:

1. **Perception (local small models)** — YOLO detection runs **fully local**; frame data never leaves the machine. **This is the default**, with no external dependency.
2. **Cognition (optional LLM)** — enabled only when you configure an OpenAI-compatible API. It sends **cropped key frames** (not the full frame stream), rate-limited and throttled.
3. **Interaction (Agent)** — the AI ops Agent uses the LLM channel you configured. Conversation is sent to that channel; AIVX does not log the conversation content itself.

**You can turn off layers 2 and 3 at any time**; the perception layer (local detection) always runs independently.

### Third-Party Services

- **LLM API (optional)** — used only when cognition / Agent is enabled. Please review the privacy policy of the provider you choose.
- **Camera vendors** — AIVX talks to your cameras via ONVIF/RTSP. Requests go directly from your machine to the cameras (LAN); AIVX does not intermediate. Please review the camera vendor's policy.

### Data Retention & Deletion

- Recordings are auto-cleaned per retention policy (default by days / by size cap, configurable).
- You can delete individual alerts, recording segments, or entire devices in the UI.
- Deleting the data directory clears everything.

### Children's Privacy

AIVX is not directed to children under the age of 13.

### Changes

This policy may be updated from time to time; changes will be posted on this page.

### Contact

If you have questions, open an issue at [github.com/ojbkxc/AIVX](https://github.com/ojbkxc/AIVX).