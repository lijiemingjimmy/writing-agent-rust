# Python 最新运行时与双前端完整等价实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 使用 Rust 完整承接现有写作智能体的最新学生端、教师端与 Agent 行为，并支持 macOS 单后端常驻。

**Architecture:** 以 Python `context-security-integration` 实际工作树为可观察行为基线，在 Rust 中分别实现领域边界、分支状态机、统一会话记忆和严格 API 鉴权。两个既有前端只依赖同一个 Rust HTTP 服务；部署资产使用参数化 launchd 模板，不包含旧项目路径、域名或数据库。

**Tech Stack:** Rust 2024、Axum、Tokio、SQLx/SQLite、Serde、Tower HTTP CORS、React/Vite、macOS launchd。

**Spec:** `docs/plans/2026-09-04-python-runtime-parity-design.md`

## Global Constraints

- 只修改 `/Users/lijieming/Downloads/写作与沟通/writing-coach-rust-agent`。
- 不读取、复制或写入 Python 的 SQLite 数据；测试只使用临时 Rust 数据库。
- 不加入旧 GitHub remote、Pages workflow、真实域名、Mac mini 绝对路径或用户数据。
- 每个行为先写失败测试并确认失败原因，再实现最小修复。
- 最终只推送清华课程仓库 `course/main`。

---

### Task 1: 最新基线与失败合同

- [x] 在 `rust-backend/tests/run_api.rs` 增加匿名 401、跨学生 403、身份防伪、`response_mode`、Skill 教师鉴权和 CORS 预检测试。
- [x] 在 `rust-backend/tests/chat_contract.rs` 增加问候、偏题、亲属冒充、停止扮演和文字不重置测试。
- [x] 在 `rust-backend/tests/skill_router.rs` 增加裸切换等待、等待期间不误路由、目标确认和普通聊天锁定测试。
- [x] 分别运行定向测试并确认它们因当前缺失行为失败，而不是测试装配错误。

### Task 2: 领域边界与分支状态机

- [x] 新建 `rust-backend/src/agent/domain_boundary.rs`，移植 Python 确定性领域判定和原因码。
- [x] 新建 `rust-backend/src/skills/branch_controller.rs`，实现 `unresolved`、`locked`、`awaiting_switch`、普通聊天锁定与 20 条切换历史。
- [x] 修改 `rust-backend/src/agent/writing_coach.rs`：安全之后先执行领域边界，再执行 BranchController；删除文字重置分支；所有响应写出 `branch` 元数据。
- [x] 运行领域边界、分支和完整 chat contract 测试直至通过。

### Task 3: Python 前端请求与学生权限

- [x] 修改 `rust-backend/src/api/dto.rs`，同时解析 Rust `action` 与 Python `response_mode`，并拒绝未知模式。
- [x] 将 `/api/chat`、sessions、messages、documents、reports、导入导出改为强制学生 Bearer；新会话绑定 principal，身份字段只取 Token。
- [x] 修改 Rust 学生前端 API 层和身份入口，bootstrap 后保存 Token、所有受保护请求携带 Authorization、401 时清理凭证。
- [x] 保留 Rust Run/SSE 扩展协议可用，并确保其创建的会话也绑定当前 principal。
- [x] 运行 API 与前端定向测试直至通过。

### Task 4: 教师接口与浏览器跨域

- [x] Skill 列表继续只返回公开字段；详情和 reload 复用教师鉴权。
- [x] 教师鉴权同时兼容 `x-teacher-token` 和 Python 的 `teacher_token` 查询参数；示例与部署文档要求公网运行时显式配置教师 Token，同时保留 Python 本地开发时未配置即开放的兼容语义。
- [x] CORS 允许配置来源上的 GET/POST/PUT/DELETE、Content-Type、Authorization、X-Teacher-Token 和 Last-Event-ID。
- [x] 对照教师前端 TypeScript 类型核验每个 teacher endpoint 的字段、状态码、上传限制和下载头。
- [x] 运行 API、CORS、教师导入和公开面测试直至通过。

### Task 5: 会话上下文与形成思路

- [x] 将 ConversationMemory 的压缩阈值改为 Python 的总字符阈值与近期字符预算，始终保留首条用户消息、持久摘要、已确认事实和最新轮次。
- [x] `response_mode=synthesize` 与 `action=synthesize` 共用同一条停止追问的综合路径。
- [x] 验证 general、Socratic、资料检索、课程问答和切换后分支都读取同一份会话上下文。
- [x] 运行长对话、代词指向、改题、分支切换和形成思路回归测试。

### Task 6: 通用 Mac mini 常驻资产

- [x] 新建参数化 launchd plist 模板与安装/卸载前检查脚本；二进制、配置、日志、工作目录和健康地址全部由参数提供。
- [x] 安装脚本提供 `DRY_RUN=1`，不会触碰系统服务；验证脚本只检查配置和本地 `/health`。
- [x] 增加 shell 语法、dry-run 和禁止旧域名/绝对路径的合同测试。

### Task 7: 全量验证与课程仓库交付

- [x] 运行 `cargo fmt --check`、`cargo clippy --all-targets --all-features -- -D warnings`、`cargo test --all-targets`。
- [x] 运行前端全部测试与生产构建。
- [x] 启动临时 Rust 服务，以两个不同 principal 验证学生隔离、Python `response_mode`、教师 Token、multipart 和 CORS。
- [x] 运行隔离脚本并核对课程演示数据边界。
- [x] 审查 diff，提交后只推送 `course/main`，用远端只读查询核对提交哈希。
