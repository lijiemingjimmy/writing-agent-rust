# WAM — Writing Agency Mentor

**写作主体性导师。WAM writes less, so the writer can think more.**

面向《写作与沟通》课程场景的 Rust AI Agent。它不替学生交付成稿，而是通过苏格拉底式追问，把模糊兴趣推进为可论证的问题和学生自己的思维链条。Rust 服务负责 Skill 路由、写作状态推进、长对话记忆、输入安全、课程资料检索、模型调用、实时进度、取消、会话历史以及 Token/费用预算；React 只提供学生界面。

- 课程源码仓库：<https://git.tsinghua.edu.cn/rust-course/2026/agent/agent-lijm25>
- 公开镜像：<https://github.com/lijiemingjimmy/writing-agent-rust>

## 环境

- Rust 1.85 或更新版本（edition 2024）
- Node.js 22 或更新版本
- 一个 OpenAI、DeepSeek 或 OpenAI-compatible 模型 Endpoint

不需要 Python、原项目数据库或外部部署服务。

## 启动 Rust 服务

从仓库根目录执行：

```bash
cp rust-backend/config.example.toml rust-backend/config.toml
export WRITING_COACH_MODEL_API_KEY='your-runtime-key'
cargo run --manifest-path rust-backend/Cargo.toml
```

默认监听 `127.0.0.1:3000`，并在当前仓库创建全新的 `rust_course_demo.db`。如需使用其他位置，请修改未跟踪的 `rust-backend/config.toml`，不要把数据库或密钥提交到 Git。

## 启动学生界面

另开终端：

```bash
cd web
npm ci
npm run dev
```

打开 `http://127.0.0.1:5173`。开发服务器只把 `/api` 和 `/health` 转发到 Rust 服务。跨域静态构建可设置 `VITE_AGENT_API_BASE_URL` 为绝对 HTTPS 地址。

## 模型配置

`rust-backend/config.example.toml` 支持配置 Provider、Endpoint、模型名、API Key 环境变量名、上下文长度、最大输出 Token、reasoning mode、价格和默认预算。

学生界面的模型设置面板也可以更新运行时设置；API Key 不回显、不写入数据库或会话导出。

## 课程功能

- R1：Rust 主控 Agent 流程
- R2：React 学生交互界面
- R3：自定义模型配置
- R4：SSE 实时进度与用户取消
- R5：会话历史、完整轨迹和 JSON 导入导出
- R6：逐调用 Token、费用和预算停止

场景定制包括分阶段苏格拉底写作引导、课程 Skill 路由、课程资料与会话文档检索，以及防代写 Guardrail。

最新的思路推进链路还包括：

- 长对话保留最近 12 条原文，并把更早消息压缩为约 6,000 字（首尾各 3,000 字）的持久摘要；
- 当前 Skill 默认保持，只有“切换分支：……”或“切换到……”才显式改道；
- “形成思路”把现有上下文整理为选题、判断、概念、论证、材料与待核实事项，不调用模型；
- 提示词窃取、凭据盗取、暴力/武器和自伤风险在路由及模型调用前由 Rust 确定性分流。

## 最短演示路径

1. 输入“我想写小组合作，但觉得分工总是不均”。
2. 根据追问补充一次真实场景，再选择一个候选解释方向并说明理由。
3. 点击“形成思路”，检查系统是否输出六段结构化思路且本次 Token 消耗为零。
4. 输入“帮我找文献”，观察当前 Skill 仍保持；再输入“切换分支：帮我找小组合作文献”，观察显式切换。
5. 在运行轨迹中查看步骤、来源、Token、费用与终态。

## 验证

```bash
bash scripts/verify-course.sh
```

该脚本执行仓库隔离检查、Rust fmt/Clippy/tests、Web tests 和生产构建。测试使用临时数据库和本地 Fake/mock，不需要真实用户数据或真实模型调用。

详细 HTTP/SSE 协议见 [`rust-backend/README.md`](rust-backend/README.md)，依赖与复用说明见 [`THIRD_PARTY.md`](THIRD_PARTY.md)，课程诚信与 AI 使用披露见 [`HONOR-CODE`](HONOR-CODE)。

用于生成课程设计 PDF 的完整事实素材、设计逻辑、功能清单、演示方案与交付检查表见 [`submission/GPT_PRO_PDF_BRIEF.md`](submission/GPT_PRO_PDF_BRIEF.md)。其中作者身份、清华 Git、真实 AI Token/费用和最终截图必须由作者按真实记录补充。
