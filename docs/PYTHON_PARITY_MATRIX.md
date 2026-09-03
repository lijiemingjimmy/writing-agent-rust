# 当前 Python → Rust 等价矩阵

基准：`writing-coach-agent` 当前 `71f51dd` 工作树。Rust 实现不导入 Python 模块，不读取原数据库。

## 会话运行时

| Python 模块 | Rust 模块 | 状态 |
| --- | --- | --- |
| `skills/router.py` + `route_decision.py` | `skills/router.rs` + `domain/route.rs` | 命令、粘性分支、显式切换、资料拒绝/检索、灵感、选题与高频技能优先级已对齐 |
| `skills/slot_filler.py` | `skills/slot_filler.rs` | 专属填槽、awaiting 消费、选项识别已对齐 |
| `agent/writing_context.py` | `domain/state.rs` | 用户消息前后两阶段更新、换题、选择、证据与摘要已对齐 |
| `agent/thinking_flow.py` | `skills/thinking_flow.rs` | motivation/candidates/choice/evidence/refined/summary 合同已对齐 |
| `services/material_search.py` | `skills/material_search.rs` | 查询增强、年份、课程/学术/网页分区和确定性回复已对齐 |
| `ChatService._decide_knowledge_use` | `skills/knowledge_decision.rs` | 模型 JSON 决策、拒绝优先和失败回退已对齐 |
| `services/chat_service.py` | `agent/writing_coach.rs` | 调用顺序、短路、守卫、reply-after 更新和元数据已对齐 |

## HTTP Router

| Python Router | Rust Router | 状态 |
| --- | --- | --- |
| `/health` | `/health` | 已实现 |
| `/api/student/access/bootstrap` | 同路径 | 已实现；随机 token，仅保存摘要 |
| `/api/chat` | 同路径 | 已实现；支持凭证身份与会话绑定 |
| `/api/sessions`、`/{id}` | 同路径 | 已实现 |
| `/api/sessions/{id}/messages` GET/POST | 同路径 | 已实现 |
| `/api/sessions/{id}/documents` | 同路径 | 已实现；同时接受 multipart 与 Rust 前端原始文本协议 |
| `/api/sessions/{id}/report` | 同路径 | 已实现 |
| `/api/skills`、`/{id}`、`/reload` | 同路径 | 已实现 |
| `/api/teacher/stats`、`students`、详情与删除 | 同路径 | 已实现 |
| 教师 `export`、`summarize`、`ask`、`class-*` | 同路径 | 已实现 |
| 教师 `analyze-upload`、`pre-conference*` | 同路径 | 已实现 |

Rust 额外保留 `/api/runs`、SSE、取消、预算、模型设置和会话轨迹导入导出，这是课程作业的 Rust Agent 增强功能。
