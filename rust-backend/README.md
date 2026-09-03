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

学生凭证使用随机 256-bit token；SQLite 只保存 SHA-256 摘要。设置环境变量
`WRITING_COACH_TEACHER_TOKEN` 后，教师端请求必须携带同值的 `x-teacher-token`。

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
