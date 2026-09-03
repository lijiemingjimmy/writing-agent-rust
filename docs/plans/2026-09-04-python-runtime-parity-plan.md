# Python 会话运行时完整等价实施计划

1. 建立行为测试合同
   - 扩充 Router 测试：灵感、高频技能、显式切换、资料来源回答、资料拒绝、阶段判断。
   - 新增 SlotFiller 测试：novelty、literature、socratic 选择与 follow-up。
   - 调整 ThinkingFlow 测试为 Python 当前决策合同，覆盖综述/修改/关系/小组四类候选路径。

2. 迁移确定性领域逻辑
   - 新建 `skills/slot_filler.rs`。
   - 完整同步 `skills/router.rs` 和 RouteDecision 规则。
   - 将 WritingContext 拆成 route 前、skill 后、reply 后三个明确的更新点。
   - 完整同步 ThinkingFlow 的决策、状态应用和候选路径。

3. 迁移知识与材料流程
   - 新建知识使用决策结构与 JSON 解析器。
   - 实现模型决策失败后的确定性回退。
   - 新建材料检索计划/回复构建器，按课程材料、在线文献/网页、使用建议、下一步输出。

4. 重写会话编排
   - Router 必须读取本轮写入前状态。
   - 依次执行填槽、上下文更新、缺槽位短路、思路状态机、知识决策、检索、生成/确定性回复、守卫、回复后更新与持久化。
   - 补齐 `student_progress`、`knowledge_use`、检索状态和事件元数据。

5. 补齐当前前端 Router 合同
   - 实现 student access、sessions、messages、documents、reports 与 skills 兼容端点。
   - 实现教师端 stats、students、detail、summary、class insights、archive ask 与导出端点。
   - 使用 Rust 新数据库生成统计和摘要，不读取或导入原 Python 数据库。

6. 验证与交付
   - 运行定向测试后运行 `cargo test --all-targets`、`cargo clippy --all-targets -- -D warnings`。
   - 运行前端测试与生产构建。
   - 以本地 API 做真实多轮对话验证。
   - 运行隔离脚本，确认原 Python 数据库哈希未变。
   - 提交 Rust 仓库并只推送 `course/main`。
