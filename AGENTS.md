# AGENTS.md — AIVX

> 给 AI 编码代理的仓库指南。规则分三级:**Never(禁止)/ Ask first(先问)/ 默认自主**。
> 每条硬规则都对应一个真实踩过的坑,不要试图绕过。

## 0. 项目速览(新会话必读)

**AIVX**(AI Video eXtended)是高性能自托管 AI 网络视频录像机(NVR):
Rust(axum 0.7 + Tokio)后端 + React 18 + TypeScript + Vite 前端,默认 SQLite 存储
(可切 PostgreSQL 经 SeaORM feature),单二进制交付(前端构建产物内嵌 `static/`)。

- **仓库**:https://github.com/ojbkxc/AIVX(main 分支,直接 push,不走 PR)
- **姊妹项目**:[AIGX](https://github.com/ojbkxc/AIGX)(AI 网关,提供 LLM 渠道与 Agent 框架范式)
- **工作区**:Cargo workspace = 主 crate `aivx` + 协议层 `aivx-net`(可扩展 `aivx-detect`/`aivx-rules`)
- **三层 AI 架构**:
  - `perception` 感知层 — YOLO 目标检测 + 跟踪 + 运动门控(本地小模型,`ort` 推理)
  - `cognition` 认知层 — LLM 误报复核 + 语义描述(可选,走 OpenAI 兼容 API)
  - `agent` 交互层 — AI 运维 Agent(抄 AIGX `src/agent`,换 NVR 工具注册表)
- **后端主要模块**(`src/`):`api`(管理面)、`config`、`db`(SeaORM entity/migration)、
  `perception`、`cognition`、`agent`、`notify`、`storage`、`auth`
- **前端**:`frontend/`(React+TS);构建产物落在 `static/` 并提交入库
- **协议层**:`aivx-net/`(ONVIF 发现 / RTSP 拉流 / GB28181 信令,独立 crate)

### 硬约束(违反会导致返工,全部踩过的坑)

1. **本地不编译 Rust**——本机无完整验证工具链,所有 Rust 编译验证走 GitHub CI(push 后查
   Actions API)。本地只做前端验证:`cd frontend && npm install && npm run build`。
2. **axum 0.7 路由参数是 `:id` 不是 `{id}`**——`{id}` 是 0.8 语法,在 0.7 下被当字面量,
   导致所有带参路由 405。
3. **`--locked` 编译**——改 `Cargo.toml` 依赖必须同步 `Cargo.lock`,否则 CI 全红。
4. **rustfmt/clippy 强制**——CI 跑 `cargo fmt --all -- --check` 和
   `cargo clippy --all-targets -- -D warnings`;保持改动小、风格贴近邻行代码。
5. **并行会话共存**——常有另一个 AI 会话同时编辑本仓库。开工前 `git status` 检查工作区;
   **只 add/commit 自己改的文件**(不 `git add -A`),发现他人未提交改动时避让。
6. **Rust 测试用独立临时目录**——`temp_dir + pid + AtomicU64 序号`;并行测试共用一个
   SQLite 文件会 `database is locked`。
7. **禁止用 PowerShell 原生命令写含非 ASCII 的文件**——编码页不匹配会导致中文乱码。
   写文件用编辑工具或 Python(显式 `encoding='utf-8'`)。
8. **所有源码保持 UTF-8(无 BOM)**;读含中文的文件必须显式指定编码。
9. **前端管理后台永远不调 `/v1/*`**——管理面一律走 `/api/*`(沿用 AIGX 分层)。
10. **部署新二进制后检查 config**——新二进制首启可能把 `config.toml` 重置回默认值。

## 1. Never(硬性禁止)

1. 禁止本地 `cargo build` / `cargo check` / `cargo test`(见硬约束 1)。
2. 禁止把摄像头凭据(RTSP 用户名/密码)、ONVIF 密码、LLM API key、JWT secret 写进任何
   入库文件;部署脚本凭据走环境变量。
3. 禁止为绕过 CI 而修改 workflow(`.github/workflows/`)——除非任务就是修 CI 且用户明确要求。
4. 禁止直接改 `static/`(前端构建产物,由 `frontend` build 生成)。
5. 禁止删除/绕过测试使 CI 变绿;不许 `.skip`/`.only`/删断言。
6. 禁止动 SeaORM migration 历史文件(`src/db/migration/` 已有迁移不可改,只能新增)。
7. 禁止 `git add -A` / `git add .`:只 add 自己本次改动的文件。
8. 禁止提交 secrets:签名密钥、数据库密码、LLM key 走 CI secrets,不入库。
9. **禁止降低用户监看的分辨率和帧率**——分析可降(子码流),但前端监看/录像画面不得降。
   这是 NVR 的第一准则(源自 ai-nvr 的教训)。
10. 禁止重新引入其他项目的私有标识:本仓库内部标识统一 `aivx*`。

## 2. 架构边界

- **三层 AI 单向依赖**:`perception` 不依赖 `cognition`/`agent`;`cognition` 不依赖 `agent`。
  检测层在无 LLM 配置时必须能独立运行。
- **分析用子码流,录像/监看用主码流**:检测输入缩到低分辨率(默认 640×640),录像用
  `-c copy` 不转码。两者不可混淆。
- **性能优先**:帧路径目标 0 拷贝(预分配帧池 + 借用);运动门控在检测前;报警写库走
  异步批量 channel,分析循环绝不直接碰 DB(见 rebucca 的 SQLite 写锁教训)。
- **运动门控**:无运动时跳过 YOLO;有区域/越线类布控时 `force_detect` 持续检测。
- **报警去重**:同一目标同一区域同一算法,仅在状态变化时写库/推送,不做每帧写。
- **RBAC**:角色 admin > user;管理面接口一律鉴权;普通用户可见自己资源。

## 3. 开发方法(Think Before Coding / 简化优先 / 手术式修改)

- **思考优先**:假设显式;多重解释时呈现并询问,不沉默选择;存在更简单方法必须说出来。
- **范围只从用户请求的原话出发**:编码前回答三件事——用户要求什么 / 我将构建什么 /
  我将放弃什么。方法削减必须明说放弃了什么,不许沉默削减。
- **简化优先**:最少的代码解决问题;没有超范围功能;单用途不抽象。
- **手术式修改**:只改必须改的;不"顺手"改进邻近代码;匹配现有风格。
- **bug fix 只修已复现的缺陷**:hunch(猜相邻代码也有问题)不可作为扩大范围的理由。
- **参考实现优先**:实现新功能前,对比本仓库 `C:\GitHub\rustsp` 下的参考项目
  (frigate / rebucca / ruoyi-qs-nvr / ai-nvr / open-nvr),引用其做法;偏离需命名并给理由。
- 注释只写"为什么",不叙述"做什么";中文注释与代码混排时保持 UTF-8。
- 提交信息:conventional 前缀 + 中文祈使句(`feat(perception): 增加越线检测`、
  `fix(api): ...`、`perf(motion): ...`)。

## 4. 验证纪律

- **完成态定义 = CI 绿。** push 不是终点;push 后盯 CI,红了修根因再推,不许留红 CI 结束回合。
- **CI 看板查询**(本机无 `gh`):
  ```
  curl -s "https://api.github.com/repos/ojbkxc/AIVX/actions/runs?head_sha=<SHA>&per_page=5"
  ```
- **CI 关卡**:lint(fmt+clippy)→ frontend(tsc+vite build)→ rust(feature 组合
  `cargo check --locked` + `cargo test --locked` + `cargo build --release --locked`)。
- **push 前自检**:`git status` 确认只包含自己的改动 → diff 对照第 1/2 节规则 → 提交信息
  符合第 3 节格式。
- **不许宣称被阻塞**:限制是"从真实失败挣来的结论",不是从配置/文档读来的字段。没实际
  尝试就说 "not attempted",不许说 "we can't";被挡下要引用真实报错。
- **结束卫生**:回合结束时不留未提交改动;最终答复带 CI 结果或相关 commit SHA。

## 5. Ask first(先问再做)

- 升级 axum / tokio / SeaORM 等核心依赖大版本(历史上有 API 破坏性变更)。
- 新增重依赖(如引入新的推理后端、视频库),或改 workspace 成员结构。
- 动 `src/db/migration/`(已有迁移不可改,只能新增)。
- 引入新的协议接入(GB28181 / 厂商 SDK)——先确认需求,再定 crate 边界。
- 重命名公共 API、改数据库 schema(`src/db/entity/` 需同步导出)。
- 任何删除超过 100 行的批量清理。

## 6. 参考文件

- 项目设计蓝图:`DESIGN.md`(数据模型 / trait 签名 / 三层 AI / 性能预算)
- AIGX 开发规范(姊妹项目):`../AIGX/AGENTS.md`
- 参考监控项目:`C:\GitHub\rustsp\{frigate-dev,rebucca-main,ruoyi-qs-nvr-master,ai-nvr-main,open-nvr-main,frigate-event-handler-master,ai-adapter-main}`