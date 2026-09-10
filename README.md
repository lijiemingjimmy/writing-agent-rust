# WAM — Writing Agency Mentor

**写作主体性导师。WAM writes less, so the writer can think more.**

面向《写作与沟通》课程场景的 Rust AI Agent。它不替学生交付成稿，而是通过苏格拉底式追问，把模糊兴趣推进为可论证的问题和学生自己的思维链条。Rust 服务负责 Skill 路由、写作状态推进、长对话记忆、输入安全、课程资料检索、模型调用、实时进度、取消、会话历史以及 Token/费用预算；同时提供学生端和教师端使用的 API Router。

- 课程源码仓库：<https://git.tsinghua.edu.cn/rust-course/2026/agent/agent-lijm25>
- 公开镜像：<https://github.com/lijiemingjimmy/writing-agent-rust>
- [设计报告（PDF）](docs/report.pdf) · [LaTeX 源码（TeXPage ZIP）](docs/report-source.zip) · [演示视频（MP4）](docs/media/demo.mp4)

> **语料说明：**课程原始语料涉及授权限制，暂未开源，仅保存在作者本地。仓库保留
> Skill、检索实现和可公开的合成样例；本地演示时可在未提交的运行配置中设置
> `local_corpus_root`（相对或绝对目录），只读检索该目录下的 Markdown，界面只显示相对来源路径。
> 默认本地语料目录为 `corpus/local/`，详见下面的“部署自己的语料”。私有文件已被 Git 忽略。

## 环境

- Rust 1.88 或更新版本（edition 2024）
- Node.js 22 或更新版本
- 一个 OpenAI、DeepSeek 或 OpenAI-compatible 模型 Endpoint

项目使用 Rust 实现核心 Agent 运行时，并提供配置、演示数据与启动方式。

## 启动 Rust 服务

从仓库根目录执行：

```bash
# 首次部署时复制；已有 config.toml 时不要覆盖。
cp rust-backend/config.example.toml rust-backend/config.toml
# 可选：也可以启动后在界面“模型设置”中填入临时 API Key。
export WRITING_COACH_MODEL_API_KEY='your-deepseek-api-key'
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

## 启动教师界面

教师端与学生端共用同一个 React 工程和 Rust API。确认私有运行配置已经设置
`security.teacher_access_token` 后，可以直接打开
`http://127.0.0.1:5173/#/teacher`，输入教师访问码。也可以另开终端执行：

```bash
cd web
npm run dev:teacher
```

随后打开 `http://127.0.0.1:5174/#/teacher`。教师端包含总览、学生、对话记录、
班级洞察、数据导入和设置；独立构建使用 `npm run build:teacher`，产物目录
`web/dist-teacher/` 已被 Git 忽略。

## 模型配置

`rust-backend/config.example.toml` 支持配置 Provider、Endpoint、模型名、API Key 环境变量名、上下文长度、最大输出 Token、reasoning mode、价格和默认预算。

默认 Provider 为 `DeepSeek`，Endpoint 为 `https://api.deepseek.com`，Model 为 `deepseek-v4-flash`。学生界面“模型设置”保留这三个值后填入自己的临时 API Key 即可；第三方兼容服务请改成服务商提供的地址和模型名称。“高级设置”可以调整上下文、输出长度、思考强度和预算，价格字段是手动估算值，并非实时官方报价。

界面保存的设置与临时 Key 仅在当前 Rust 进程生效，重启后需重新填写；常驻部署请通过自己的环境变量提供 Key，并把非敏感模型参数写入本地 `config.toml`。API Key 不回显、不写入数据库或会话导出。

教师端“对话记录”中的快捷提问会直接使用这份共用配置调用模型，无需在教师端再次填写 Key。教师端显示配置状态，返回窗口时自动刷新。教学建议基于最多 20 条近期学生提问抽样，每个会话最多 2 条，并显示证据；综合备课问题不要求与学生提问逐字匹配。

## 部署自己的语料

公开仓库包含 Agent 源码、写作 Skills 和少量 `corpus/examples/` 合成样例，**不包含完整课程原始语料**。Skill 描述如何引导写作；语料提供可检索的课程内容，两者可以独立添加。

最方便的方式是把 UTF-8 编码的 `.md` 文件复制到 **`corpus/local/`**。服务启动会自动创建这个目录，允许按课程或章节建立子目录：

```text
corpus/
  examples/                 # 仓库自带的公开合成示例
  local/                    # 你自己的部署语料，内容默认不进 Git
    课程讲义/
      选题与问题意识.md
      证据与论证.md
    作业要求.md
```

也可以先复制一个示例体验：

```bash
mkdir -p corpus/local
cp corpus/examples/session-document-demo.md corpus/local/
```

随后在学生端提问与资料相关的课程问题，例如“课程资料中对小组分工的责任边界有什么说明？”。当本轮课程检索命中时，回答下方的“本轮参考了…个资料片段”会显示文件名和标题；无需为每份资料修改 Skill。**新增、修改、删除文件会在下一次检索时生效，无需重启，也不需要向量数据库。**检索是否触发取决于当前问题和写作阶段，不是每一轮都强行引用语料。

已有语料目录不需要复制。修改私有 `rust-backend/config.toml`：

```toml
local_corpus_root = "/absolute/path/to/your/course-materials"
# 也支持相对服务启动目录的路径，例如 "my-course/notes"。
```

省略该项时默认使用 `<corpus_root>/local`。请从仓库根目录启动服务。只有 `.md` 文件参与这套部署语料检索，建议用清晰的 Markdown 标题划分章节；其它格式需先转成 UTF-8 Markdown。目录下的语料对这台服务的所有学生会话可用。界面的“上传资料”则是**当前会话专用**的 TXT/Markdown 附件，不会变成全班共享语料。

默认目录下的私有内容已被 `.gitignore` 排除，公开示例不会被自动复制成你的课程内容。部署到其他机器时请单独复制或挂载自己的语料目录。

## 聊天交互

- 发送消息、打开历史会话时自动定位到最新内容；收到回答时，在底部继续跟随。
- 向上阅读历史时保留阅读位置，点击“回到最新”可继续跟随。
- 最后一条提问下方可“修改并重新发送”，支持取消。重新发送会建立一个保留前文与当前附件的新版本，原问题和回答留在历史对话中。
- 新对话会保存每轮提问前的写作状态，修改时恢复该状态，避免旧回答或摘要污染新版本；升级前的旧会话没有状态快照时，保留前文并重新整理写作状态。
- 运行中不能修改提问；请先停止或等待完成。旧版本的运行轨迹和费用留在原会话，不重复计入新版本。

## 课程功能

- R1：Rust 主控 Agent 流程
- R2：React 学生端与教师教学工作台
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
- student access、sessions、messages、documents、reports、skills 与 teacher dashboard Router 由 Rust 原生实现；统计读取本服务数据库。

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

详细 HTTP/SSE 协议见 [`rust-backend/README.md`](rust-backend/README.md)，依赖说明见 [`THIRD_PARTY.md`](THIRD_PARTY.md)，课程诚信与 AI 使用披露见 [`HONOR-CODE`](HONOR-CODE)。

如需在一台 macOS 主机上常驻运行，可先用 `DRY_RUN=1 scripts/install-launchd.sh ...` 核对所有参数，再显式安装；模板不包含个人机器路径、域名或旧项目配置。`scripts/verify-local-service.sh` 只访问传入的回环 `/health` 地址。


### 共享模型配置权限

通过 UI 修改模型设置需要教师访问码。请先在私有配置的 `[security]` 中设置随机的
`teacher_access_token`，再在模型设置面板输入该访问码。未设置时，UI 不允许修改共享配置，
仍可通过私有 TOML 和模型 Key 环境变量启动。更换 Endpoint 或 Provider 时必须重新提供 Key，
防止已有凭据被发送到另一个服务。学生运行、取消与 SSE 都要求 Bearer 身份凭据并校验会话归属。
