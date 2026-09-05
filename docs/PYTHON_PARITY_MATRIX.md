# 当前 Python → Rust 等价矩阵

基准：`writing-coach-agent/.worktrees/context-security-integration` 当前实际工作树（`31f2d5d` 加领域边界未提交改动）。Rust 版本以这一产品逻辑和可观察行为作为迁移基准。

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
| `agent/domain_boundary.py` | `agent/domain_boundary.rs` | 已对齐：问候、偏题、亲属冒充与停止扮演均在模型前确定性处理 |
| `skills/branch_controller.py` | `skills/branch_controller.rs` | 已对齐：locked/unresolved/awaiting_switch、普通聊天锁定与切换历史 |
| `services/conversation_context.py` | `agent/conversation_memory.rs` | 已对齐：24k 总字符阈值、12k 最近窗口、首条消息、持久摘要与模型失败降级 |
| `ChatService._answer_synthesis` | `agent/writing_coach.rs::answer_synthesis` | 已对齐：完整会话 + 写作状态的 JSON 模型收束、固定渲染与确定性降级 |

## HTTP Router

| Python Router | Rust Router | 状态 |
| --- | --- | --- |
| `/health` | `/health` | 已实现 |
| `/api/student/access/bootstrap` | 同路径 | 已实现；随机 token，仅保存摘要 |
| `/api/chat` | 同路径 | 已实现严格 Bearer、归属校验以及 `response_mode`/`action` 双协议 |
| `/api/sessions`、`/{id}` | 同路径 | 已实现 |
| `/api/sessions/{id}/messages` GET/POST | 同路径 | 已实现 |
| `/api/sessions/{id}/documents` | 同路径 | 已实现；同时接受 multipart 与 Rust 前端原始文本协议 |
| `/api/sessions/{id}/report` | 同路径 | 已实现 |
| `/api/skills`、`/{id}`、`/reload` | 同路径 | 列表只公开安全字段；详情/reload 受教师鉴权 |
| `/api/teacher/stats`、`students`、详情与删除 | 同路径 | 已实现 |
| 教师 `export`、`summarize`、`ask`、`class-*` | 同路径 | 已实现 |
| 教师 `analyze-upload`、`pre-conference*` | 同路径 | 已实现 |

Rust 额外保留 `/api/runs`、SSE、取消、预算、模型设置和会话轨迹导入导出，这是课程作业的 Rust Agent 增强功能。

## 本轮确认的旧矩阵遗漏

- Python 最新学生数据接口全部强制 Bearer，Rust 旧实现仍允许匿名。
- Python 前端固定发送 `response_mode`，Rust 旧 DTO 只读取 `action`。
- 公网双前端需要 Authorization、X-Teacher-Token、multipart 与 DELETE 的 CORS 预检支持。
- 文本不能重置同一 session；裸分支切换必须等待目标；普通聊天可以被显式锁定。
- 问候、偏题与真实亲属角色冒充必须在模型之前确定性处理。
