# Python 行为对齐与会话资料闭环设计

## 目标

以当前 `writing-coach-agent` Python 工作树的可观察产品行为为基准，对齐 Rust 版的 Skill、Router、写作上下文、形成思路和回答边界；同时修复“资料上传成功但模型不一定看到”的缺陷。最终用户可以上传 TXT/Markdown，在普通写作讨论和苏格拉底追问中检索到相关片段，并在界面上确认本轮实际使用的来源。

## 边界

- Python 仓库只读，Rust 仓库是唯一修改目标。
- 不复制或提交 Python 的 `writing_coach.db`、WAL/SHM、真实会话、真实用户资料和私有课程语料。
- 可复现数据通过 SQLite migration、公开示例 Markdown 和 seed/import 命令生成；运行时数据库继续被 Git 忽略。
- 延续现有学生端与教师端产品，不重做 UI；只增加资料可见性、错误信息和必要的宽屏适配。
- 路由、状态和回答策略要求等价；生成式回答不做逐字比较。

## 已确认缺陷

1. `knowledge_plan` 仅为 `draft_diagnosis`、`writing_feedback` 启用 `session_documents`，其他 Skill 无法使用上传资料。
2. `build_socratic_humanizer` 没有接收 `KnowledgeBundle`，因此苏格拉底路径无法把检索命中注入模型。
3. `search_session_documents` 把整份文档作为单个命中，长文档相关性和上下文成本不可控。
4. 前端上传后只显示“已加入”，没有资料列表、索引状态或回答来源。
5. Rust 与当前 Python 的多份 Skill YAML 已产生差异，缺少自动对齐检查。

## 架构

### 1. 行为基准层

新增机器可读的 Python→Rust parity cases。每个 case 固定用户消息、历史状态和预期的 Skill、route intent、stage、risk、branch mode、required action。Rust 测试直接读取这些 case；另提供只读 Python 导出脚本生成基准结果，避免手工维护两套含义不同的断言。

Skill YAML 以当前 Python 版本为基线同步。测试比较影响运行时的字段：id、触发词、所需槽位、answer policy、output template、corpus paths；允许 Rust 对不可公开路径做显式过滤，但不允许静默改变教学逻辑。

### 2. 会话资料存储层

保留 `documents` 原始文本记录，新增 `document_chunks`：

- `id`、`document_id`、`session_id`
- `chunk_index`、`heading`
- `start_char`、`end_char`
- `text`、`search_text`
- `created_at`

Markdown 先按标题切分，超长部分按段落切分；目标片段 800–1200 字符，最大 1600 字符，相邻片段保留最多 120 字符。TXT 按段落使用同一规则。上传事务同时写入 document 与 chunks，任何一步失败则不留下半成品。

新增 `retrieval_events`，记录 session、message/run、query、命中 chunk id、score 和是否注入 Prompt。它只保存可追溯元数据，不保存模型密钥。

### 3. 检索与 Prompt 层

会话存在文档时，`session_documents` 对全部写作 Skill 可用。检索查询由当前消息、当前选题、研究问题和最近用户表达组合；用户提到“上传的资料”“这篇文章”或文件名时强制检索。

检索以 Unicode/CJK n-gram 与标题加权为基础，不引入外部向量服务。返回 top-k 独立片段，并执行总字符预算限制。

普通 Prompt 和 Socratic Prompt 统一接收 `KnowledgeBundle`。资料作为不可信证据块注入，明确标注文件名、标题与片段 id；Skill 指令不能被上传文档覆盖。

### 4. API 与 UI 层

在现有上传接口之外增加：

- `GET /api/sessions/{id}/documents`：列出当前会话资料与索引状态。
- `DELETE /api/sessions/{id}/documents/{document_id}`：删除当前会话资料及片段。
- 回答 metadata 的 `session_document_sources`：本轮实际注入的来源。

学生端显示资料文件名、格式、大小、片段数和状态。回答下方显示“本轮参考了 N 个会话资料片段”，可展开查看文件和标题。未使用时不制造引用。

### 5. 对话策略修正

- 选择题只用于需要阶段性收束且选项近似互斥的场景；始终允许组合或开放回答。
- 一轮只保留一个主问题；连续深入 2–3 轮后提供“小结/继续/形成思路”的出口。
- 将边界结果分为允许、软引导和硬拒绝。正常讨论、局部示范、观点比较不得被硬拒绝；只有明确要求交付可直接提交的完整作业才进入硬边界。
- 姓名、学号仅为展示字段；会话归属只依据随机 Bearer principal。

## 错误处理

- 上传错误返回稳定 error code：`unsupported_document_type`、`document_too_large`、`invalid_document_utf8`、`document_index_failed`。
- 上下文超限在调用 Provider 前返回估算 Token、配置上限和处理建议。
- 检索失败不伪装为“未命中”；metadata 和 UI 区分 `not_used`、`no_hits`、`failed`。
- 用户文档内容永远不能改变系统 Prompt、Skill 或权限。

## 验收

核心端到端用例上传含唯一哨兵句的 Markdown，再在普通和 Socratic 两条路径提问。测试必须截获模型请求并证明哨兵句存在于 Prompt，同时 metadata 与 UI 显示正确来源。跨学生、跨会话和删除后均不得检索到该片段。

全量要求：Rust fmt、clippy、tests，Web tests/build，以及无需 Python 服务的本地启动与 API 流程全部通过。
