# Python 最新运行时与双前端完整等价设计

## 目标

使用 Rust 实现学生会话运行时，以及学生端和教师端实际调用的 HTTP Router。验证覆盖领域边界、聊天服务和相关回归场景。

这里的“完整”指学生在一次或多轮对话中可观察到的 Agent 行为完整。Agent 主控、状态管理和服务接口由 Rust 实现。

## 保持不变的隔离边界

- Rust 后端、SQLite、前端构建目录和 Git 仓库仍然完全独立。
- 教师端兼容 API 只能分析 Rust 自己的新数据库；不复制旧 Git、GitHub Pages、真实域名、绝对路径或原数据库。
- 可以提供通用的 macOS `launchd` 常驻模板和安装检查，但所有路径、域名、Token 与数据库位置必须由 Rust 项目自己的配置或环境变量注入。
- 不复制原学生案例和真实会话；课程原始语料仅保存在本地，因授权限制暂未开源。
- React 学生端协议保持兼容，本次不改视觉层。

## 行为基准

一次普通会话严格采用 Python 最新顺序：

1. 读取并规范化会话状态与记忆。
2. 安全分类；危险输入在业务路由前被截断。
3. 执行课程领域边界；问候、明确偏题和真实亲属冒充请求走确定性承接，不调用模型。
4. 由 BranchController 解析分支：未锁定时路由；锁定后普通消息不换 Skill；只有显式“切换分支/切换到”才解除锁定；裸切换进入 `awaiting_switch`；可以显式锁定普通聊天。
5. 根据被选 skill 进行专属槽位提取。
6. 将用户消息、槽位和路由结果写入 WritingContext。
7. 缺槽位时只询问第一个缺口并结束本轮。
8. Socratic 路径先由确定性 ThinkingFlow 决策，再把状态变化写回上下文。
9. 非材料检索业务由模型判断是否使用课程语料或外部检索；解析或供应商失败时退回确定性策略。
10. MaterialSearch 使用确定性、分区且可溯源的回复，不让模型虚构检索结果。
11. 其他路径按结构化 Prompt 调用模型，随后执行边界与证据守卫。
12. 用最终回复反向更新候选方向、追问、待回答问题和面批摘要。
13. 保存完整 student_progress、knowledge_use、检索状态、route_decision、branch 与事件。

## 等价矩阵

| 子系统 | Python 合同 | Rust 实现策略 |
|---|---|---|
| Router | 命令、显式切换、粘性分支、资料拒绝、灵感、选题、P1 高频技能、别名与关键词 | 逐项移植谓词与优先级；RouteDecision 与 Python 阶段/意图/风险规则一致 |
| BranchController | `unresolved`、`locked`、`awaiting_switch`、显式普通聊天和最近 20 次切换记录 | 独立 Rust 模块；Router 只负责未锁定目标判断，分支状态写入 session state |
| DomainBoundary | 问候、偏题回引、禁止真实亲属冒充、停止角色扮演 | 安全分类之后、业务路由之前执行；确定性回答，模型调用数为零 |
| SlotFiller | 六类专属填槽与命令/awaiting 特例 | 独立 `slot_filler` 模块，不再用通用字符串长度替代业务规则 |
| WritingContext | 新题重置、主题稳定、场景/困惑/机制、路径选择、证据/反例、搜索词与摘要 | 保留强类型核心字段，兼容字段继续放入 flatten map；增加 before/after-reply 两阶段更新 |
| ThinkingFlow | motivation → candidates → choice reason → evidence → refined/summary | 使用 Python 的 `required_action`、`missing_slot`、`allowed_response_kind` 合同和四类候选路径 |
| Knowledge decision | 模型 JSON 决策 + 失败回退 + 用户拒绝优先 | 新增决策提示、严格解析与回退；模型失败不终止整轮 |
| MaterialSearch | 上下文增强查询、年份、课程/学术/网页分区、失败说明 | 独立确定性计划与回复构建器，输出只来自 SearchHit |
| Prompt | policy + skill + state + memory + evidence + latest turn | 保留 Rust 的不可信数据隔离，同时补齐 Python 的语义字段和流程合同 |
| Metadata | student_progress、knowledge_use、两个搜索通道、路线与守卫 | 从最终状态集中构造，不再输出简化占位对象 |
| HTTP Router | student access、sessions、messages、documents、reports、skills、teacher dashboard | 提供 Rust 原生兼容路由；兼容 `response_mode`；所有学生数据端点强制 Bearer 并校验归属 |
| 浏览器跨域 | 两个 GitHub Pages 前端携带 `Authorization`、`x-teacher-token`、multipart 和 DELETE | CORS 精确允许配置来源、所需请求头与 GET/POST/PUT/DELETE；不允许任意来源 |
| macOS 常驻 | 单一后端支持学生端和教师端，崩溃拉起、开机运行、健康检查 | 提供不含真实域名和绝对路径的 launchd 模板、安装/校验脚本和 dry-run 测试 |

## 验收标准

- Python 中学生端 Router、SlotFiller、Socratic、材料检索与高频 Skill 合同均有对应 Rust 测试。
- 多轮对话能保持主题，识别纠正与换题，生成候选路径，解析选择，追问理由，再检查证据。
- 普通闲聊不污染 `socratic_rounds`；材料检索不经过生成式回答。
- 模型知识决策可压制关键词触发；决策失败时仍能用回退策略完成回复。
- Rust 全量测试、Clippy、前端测试、生产构建和端到端演示通过。
- 隔离脚本通过，最终只推送课程 GitLab remote。
- 当前 Python 前端实际调用的 Router 在 Rust 中均有兼容端点和合同测试。
- Python 前端的 `response_mode: synthesize` 必须立即形成思路且不继续追问。
- 无 Bearer 的学生请求返回 401；跨学生访问返回 403；身份字段只取 Token principal。
- 公开 Skill 列表不泄露路径或内部策略；详情和 reload 受教师 Token 保护。
- 浏览器预检允许两个前端真实使用的请求头与方法。
- `重置`、`清空`、`重新开始` 等普通文字不清空同一 session；只有前端新建会话产生新上下文边界。
