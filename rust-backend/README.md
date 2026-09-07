# Rust Agent Server

Axum 服务通过统一 `RunEngine` 执行 Skill 路由、写作状态推进、资料检索、模型调用、Guardrail、持久化和停止条件。

## 配置

从仓库根目录复制示例配置：

```bash
cp rust-backend/config.example.toml rust-backend/config.toml
export WRITING_COACH_MODEL_API_KEY='your-runtime-key'
cargo run --manifest-path rust-backend/Cargo.toml
```

未设置 `WRITING_COACH_CONFIG` 时读取 `rust-backend/config.toml`。`database_url` 默认指向课程项目自己的新数据库；`skill_root` 和 `corpus_root` 相对进程当前目录解析。

课程原始语料暂未开源，只保存在作者本地。需要本地演示时，在未提交的运行配置中将
`local_corpus_root` 设置为绝对目录；服务会只读、递归检索其中的 Markdown，并将来源
规范化为相对路径。
`corpus/` 仅包含可公开的合成测试样例。

真实 Key 只从 `model.api_key_env` 指定的环境变量或设置 API 的进程内临时值读取，不写入配置、SQLite、日志、SSE 或导出文件。

## 主要路由

| 方法与路由 | 用途 |
| --- | --- |
| `GET /health` | 服务身份和健康检查 |
| `POST /api/runs` | 创建异步 Agent Run |
| `GET /api/runs/:id` | 读取状态、预算与用量 |
| `GET /api/runs/:id/events` | SSE 历史回放与实时进度 |
| `POST /api/runs/:id/cancel` | 幂等取消 |
| `POST /api/student/access/bootstrap` | 创建学生访问凭证 |
| `GET /api/sessions` | 会话列表 |
| `POST /api/sessions` | 创建空白会话 |
| `GET /api/sessions/:id` | 读取会话状态 |
| `GET /api/sessions/:id/messages` | 会话消息 |
| `POST /api/sessions/:id/messages` | 兼容旧前端的同步消息入口 |
| `POST /api/sessions/:id/documents` | 上传受限 TXT/Markdown 资料 |
| `GET /api/sessions/:id/documents` | 列出文件、索引状态与片段数 |
| `DELETE /api/sessions/:id/documents/:document_id` | 删除文件及其索引片段 |
| `GET /api/sessions/:id/report` | 生成写作过程报告 |
| `GET /api/sessions/:id/export` | 导出完整会话轨迹 |
| `POST /api/sessions/import` | 用新 ID 导入轨迹 |
| `GET/PUT /api/settings/model` | 查询或更新模型与预算配置 |
| `GET /api/skills` | 列出课程 Skills |
| `/api/teacher/*` | 教师统计、学生过程、洞察、摘要、导出和面批资料 |

Run 终态为 `completed`、`cancelled`、`budget_exceeded` 或 `failed`。每个模型响应的输入 Token、输出 Token、价格快照和整数微美元费用先持久化，再检查预算。

## 数据库

启动时在配置指定的新 SQLite 文件上运行 `migrations/`。连接启用 foreign keys、WAL 和 busy timeout。数据库及其 sidecar 全部被 `.gitignore` 排除。

不要把 `database_url` 指向任何其他项目或真实用户数据库。课程演示使用新数据库或测试创建的临时数据库。

学生凭证使用随机 256-bit token；SQLite 只保存使用 `security.student_token_pepper`
计算的 HMAC-SHA256 摘要。学生数据接口要求 `Authorization: Bearer <token>`，并按
principal 校验会话归属。教师端在私有运行配置中设置 `security.teacher_access_token`
后，请求必须通过 `x-teacher-token` 请求头或 `teacher_token` 查询参数携带同值。

`conversation_context_max_chars` 默认为 24,000，未超过时保留同一会话全部消息；
超过后保留首条用户消息、`conversation_context_recent_chars` 指定的最近窗口、已确认事实
和持久摘要。“形成思路”使用同一上下文调用模型收束；模型供应商错误时才使用确定性降级。

会话资料上传后按标题和长度确定性分块，与文档记录事务化写入。检索查询会组合本轮消息、
写作上下文和最近用户消息，命中片段以 untrusted evidence 进入普通与 Socratic Prompt；响应
元数据包含文件、标题和 chunk 标识，便于前端解释“本轮具体看到了什么”。历史导入会从
正文重建索引。上下文预检失败时，终态错误会给出预计 token、配置上限和缩短输入/删除资料
的恢复提示。

## macOS 常驻运行

先编译 release 二进制并准备仅属于本 Rust 项目的配置与数据库目录，然后执行 dry-run：

```bash
DRY_RUN=1 scripts/install-launchd.sh \
  --label edu.example.wam \
  --binary /absolute/path/writing-coach-server \
  --config /absolute/path/config.toml \
  --workdir /absolute/path/project \
  --log-dir /absolute/path/logs
```

确认输出后去掉 `DRY_RUN=1` 才会写入用户 LaunchAgents 并启动服务。健康检查脚本只接受
回环地址；卸载脚本会停止服务并把 plist 移到废纸篓，不直接删除。

## 测试

```bash
cd rust-backend
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
NO_PROXY=127.0.0.1,localhost,::1 \
  no_proxy=127.0.0.1,localhost,::1 \
  cargo test --all-targets
```

网络边界在测试中使用进程内 Fake 或回环 mock，不读取真实凭据。
