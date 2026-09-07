# WAM — Writing Agency Mentor

**写作主体性导师。WAM writes less, so the writer can think more.**

面向《写作与沟通》课程场景的 Rust AI Agent。它不替学生交付成稿，而是通过苏格拉底式追问，把模糊兴趣推进为可论证的问题和学生自己的思维链条。Rust 服务负责 Skill 路由、写作状态推进、长对话记忆、输入安全、课程资料检索、模型调用、实时进度、取消、会话历史以及 Token/费用预算；同时兼容原学生端和教师端实际使用的 API Router。

- 课程源码仓库：<https://git.tsinghua.edu.cn/rust-course/2026/agent/agent-lijm25>
- 公开镜像：<https://github.com/lijiemingjimmy/writing-agent-rust>

> **语料说明：**课程原始语料涉及授权限制，暂未开源，仅保存在作者本地。仓库保留
> Skill、检索实现和可公开的合成样例；本地演示时可在未提交的运行配置中设置
> `local_corpus_root`（绝对目录），只读检索该目录下的 Markdown，界面只显示相对来源路径。
> 完整课程资料检索。该目录已被 Git 忽略，不影响其余功能的编译、测试和运行。

## 环境

- Rust 1.85 或更新版本（edition 2024）
- Node.js 22 或更新版本
- 一个 OpenAI、DeepSeek 或 OpenAI-compatible 模型 Endpoint

项目在原有写作智能体的产品逻辑和既有资产基础上，以 Rust 重构核心 Agent
运行时，并提供本课程项目的配置、演示数据与启动方式。

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

- 24,000 字符以内保留同一会话全部原文；超过阈值后保留首条用户消息、最近 12,000 字符、已确认事实和至多 6,000 字符的持久摘要；
- 当前 Skill 默认保持；“切换分支”先进入等待目标状态，“切换分支：……”或“切换到……”则显式改道，普通聊天也可被锁定为分支；
- “形成思路”把完整可用会话与结构化写作状态交给模型收束为选题定位、核心问题、核心判断、概念路径、文章结构、材料与下一步；模型故障时使用可解释的确定性降级；
- 问候、明确偏题、真实亲属冒充和停止扮演在模型前走确定性领域边界；
- 提示词窃取、凭据盗取、暴力/武器和自伤风险在路由及模型调用前由 Rust 确定性分流。
- Python 当前的 student access、sessions、messages、documents、reports、skills 与 teacher dashboard Router 已由 Rust 原生实现；统计只读取 Rust 新数据库。

### 会话资料如何参与回答

学生端支持上传 UTF-8 编码的 `.txt` 和 `.md` 文件，单个文件上限 256 KiB。Rust 后端会按 Markdown 标题和文本长度切成有序片段，并在同一次数据库事务中保存文档与索引；任何写作 Skill 都可以结合当前问题、已形成的写作上下文和最近对话检索这些片段。命中的内容会作为“不可信证据”加入模型提示词，不能改变系统规则；回答下方会显示本轮实际使用的文件名和标题，资料面板也会显示索引片段数。删除文件会同时删除其索引片段。

可直接上传公开合成样例 [`corpus/examples/session-document-demo.md`](corpus/examples/session-document-demo.md)，随后提问“根据我上传的资料，分工不均可能是什么原因？”，验证回答来源中是否出现该文件。会话导出后再次导入时，Rust 会从导出的文档正文重新建立片段索引，不需要携带本机路径或数据库文件。

学生端首次确认姓名与学号时会向 Rust 后端换取随机 Bearer Token。受保护的会话、消息、资料、报告和导入导出接口都按 Token principal 校验归属，不能靠请求体伪造学号访问他人数据。Token 原文只保存在浏览器；数据库保存的是带独立 pepper 的 HMAC-SHA256 摘要。

## 最短演示路径

1. 输入“我想写小组合作，但觉得分工总是不均”。
2. 根据追问补充一次真实场景，再选择一个候选解释方向并说明理由。
3. 点击“形成思路”，检查系统是否停止追问、输出七段结构化思路，并在轨迹中记录这次模型调用的 Token 与费用。
4. 输入“帮我找文献”，观察当前 Skill 仍保持；再输入“切换分支：帮我找小组合作文献”，观察显式切换。
5. 在运行轨迹中查看步骤、来源、Token、费用与终态。
6. 上传 `corpus/examples/session-document-demo.md`，围绕“责任边界”继续追问，检查资料面板的“已索引”、回答来源与删除操作。

## 验证

```bash
bash scripts/verify-course.sh
```

该脚本执行仓库隔离检查、Rust fmt/Clippy/tests、Web tests 和生产构建。测试使用临时数据库和本地 Fake/mock，不需要真实用户数据或真实模型调用。

详细 HTTP/SSE 协议见 [`rust-backend/README.md`](rust-backend/README.md)，依赖与复用说明见 [`THIRD_PARTY.md`](THIRD_PARTY.md)，课程诚信与 AI 使用披露见 [`HONOR-CODE`](HONOR-CODE)。

如需在一台 macOS 主机上常驻运行，可先用 `DRY_RUN=1 scripts/install-launchd.sh ...` 核对所有参数，再显式安装；模板不包含个人机器路径、域名或旧项目配置。`scripts/verify-local-service.sh` 只访问传入的回环 `/health` 地址。

用于生成课程设计 PDF 的完整事实素材、设计逻辑、功能清单、演示方案与交付检查表见 [`submission/GPT_PRO_PDF_BRIEF.md`](submission/GPT_PRO_PDF_BRIEF.md)。其中作者身份、清华 Git、真实 AI Token/费用和最终截图必须由作者按真实记录补充。
