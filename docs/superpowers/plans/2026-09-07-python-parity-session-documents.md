# Python Parity and Session Documents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 对齐当前 Python 写作智能体的运行行为，并让上传到会话的 TXT/Markdown 在普通回答和苏格拉底追问中真正可检索、可注入、可追溯。

**Architecture:** Python 仓库作为只读行为基准；Rust 通过 parity fixtures 固化 Router/Skill 合同。SQLite 保存文档及分块，统一 KnowledgeBundle 进入全部 Prompt 路径，前端展示索引状态与本轮来源。

**Tech Stack:** Rust 2024、Axum、Tokio、SQLx/SQLite、Serde、React 19、TypeScript、Vite。

**Spec:** `docs/superpowers/specs/2026-09-07-python-parity-session-documents-design.md`

## 执行结果（2026-09-07）

本计划已实现并通过完整验证。实现时有三处安全化调整：

- 使用只读 YAML 比较器和 Rust JSON fixtures 固化基准，没有导入或执行 Python 运行时，也没有接触 Python 数据库；
- 当前仓库仅有 3 个既有 migration，因此新增迁移采用 `0004_session_document_chunks.sql`，未人为跳到 `0009`；
- 不提交预生成 SQLite seed。公开演示改为可上传的合成 Markdown，数据库由 migration 和 API 在指定的新文件上生成，避免把运行时数据库、凭据或 principal 固化进 Git。

验收结果：Rust all-target tests、Clippy `-D warnings`、fmt、Web 40 项测试、Vite production build、17 个 Skill YAML 只读对齐检查、仓库隔离检查均通过；本地页面在 1280×720 与 1920×1080 下无横向溢出或错误 overlay。

## Global Constraints

- Python 仓库只读；不写入或复制它的数据库、用户记录和私有语料。
- 不提交运行时 `.db`、`.sqlite`、WAL/SHM；公开数据只通过 migration 与 seed 输入产生。
- 所有生产行为先写失败测试并确认失败原因，再做最小实现。
- 姓名和学号不能作为授权依据，会话资料严格按 Bearer principal 与 session 隔离。
- 上传资料是 untrusted evidence，不能覆盖系统、Skill 或权限指令。

---

### Task 1: 固化最新 Python 行为基准并同步 Skill

**Files:**
- Create: `rust-backend/tests/fixtures/python_parity_cases.json`
- Create: `scripts/export_python_parity.py`
- Create: `scripts/check_skill_parity.py`
- Modify: `rust-backend/tests/skill_router.rs`
- Modify: `skills/**/*.yaml`
- Modify: `docs/PYTHON_PARITY_MATRIX.md`

**Interfaces:**
- Produces fixture fields: `name`, `message`, `state`, `expected_skill`, `expected_intent`, `expected_stage`, `expected_risk`, `expected_branch_mode`.
- Produces `check_skill_parity.py --python-root PATH`, exit 0 only when public runtime fields match.

- [ ] Write fixture-driven Rust tests for greetings, open topic exploration, numbered choices, explicit branch changes, material refusal/search, draft feedback and synthesis.
- [ ] Run `cargo test --manifest-path rust-backend/Cargo.toml --test skill_router python_parity -- --nocapture`; verify RED on current divergences.
- [ ] Add the read-only Python exporter and Skill comparator; neither script imports or opens a database.
- [ ] Synchronize current public Skill YAML from Python and document every intentional corpus-path filter.
- [ ] Re-run the focused tests and `python3 scripts/check_skill_parity.py --python-root ../writing-coach-agent`; verify GREEN.
- [ ] Commit: `test: lock current python routing parity`.

### Task 2: 文档分块与事务化数据库存储

**Files:**
- Create: `rust-backend/migrations/0009_session_document_chunks.sql`
- Create: `rust-backend/src/corpus/chunking.rs`
- Modify: `rust-backend/src/corpus/mod.rs`
- Modify: `rust-backend/src/domain/session.rs`
- Modify: `rust-backend/src/store/sessions.rs`
- Modify: `rust-backend/src/api/sessions.rs`
- Modify: `rust-backend/tests/session_api.rs`
- Modify: `rust-backend/tests/knowledge_tools.rs`

**Interfaces:**
- Produces `DocumentChunk { id, document_id, session_id, chunk_index, heading, start_char, end_char, text, search_text }`.
- Produces `chunk_document(filename: &str, text: &str) -> Vec<NewDocumentChunk>`.
- Produces `DocumentRepository::create_with_chunks(...)` and `list_chunks_by_session(...)`.

- [ ] Write failing unit tests for Markdown headings, TXT paragraphs, long-section splitting, overlap bounds, stable ordering and empty text.
- [ ] Run focused chunking tests and verify RED because the module/API is absent.
- [ ] Implement deterministic chunking with target 800–1200, hard max 1600 and overlap at most 120 characters.
- [ ] Write failing repository/API tests proving upload atomically creates chunks and rejects partial writes.
- [ ] Add migration, domain structs and transactional repository methods; update upload response with `chunk_count` and `index_status`.
- [ ] Run session API and knowledge tool tests until GREEN.
- [ ] Commit: `feat: index uploaded session documents`.

### Task 3: 全 Skill 会话资料检索与上下文查询

**Files:**
- Modify: `rust-backend/src/corpus/session_documents.rs`
- Modify: `rust-backend/src/agent/writing_coach.rs`
- Modify: `rust-backend/src/tools/knowledge.rs`
- Modify: `rust-backend/tests/knowledge_tools.rs`
- Modify: `rust-backend/tests/chat_contract.rs`

**Interfaces:**
- Produces `build_session_document_query(message, writing_context, recent_messages) -> String`.
- `session_documents` SearchHit metadata includes `document_id`, `chunk_id`, `chunk_index`, `heading`.
- `knowledge_plan` schedules session documents whenever the current session has indexed documents; explicit document references force a search.

- [ ] Write failing search tests where the final message is ambiguous but topic/research-question context locates the correct chunk.
- [ ] Write failing isolation tests for another student/session and deletion.
- [ ] Replace whole-document scoring with chunk scoring and total evidence character budget.
- [ ] Extend the knowledge plan with contextual query terms and session-document availability.
- [ ] Run focused knowledge and chat tests until GREEN.
- [ ] Commit: `feat: retrieve session evidence across writing skills`.

### Task 4: 将证据注入普通与 Socratic Prompt

**Files:**
- Modify: `rust-backend/src/skills/prompt.rs`
- Modify: `rust-backend/src/agent/writing_coach.rs`
- Modify: `rust-backend/tests/chat_contract.rs`
- Modify: `rust-backend/tests/prompt_contract.rs`

**Interfaces:**
- Extend `SocraticPromptContext` with `knowledge: &KnowledgeBundle`.
- Both prompt builders emit the same `[Course and Session Evidence]` untrusted-data block.
- Answer metadata includes `session_document_sources` and `session_document_status`.

- [ ] Add the sentinel regression test: upload `访谈记录.md` containing `青铜雨伞假说`, ask about it, and inspect the captured model request.
- [ ] Run the test and verify RED on the Socratic path because the evidence block is absent.
- [ ] Add KnowledgeBundle to SocraticPromptContext and reuse one evidence formatter for both paths.
- [ ] Add metadata tests for `used`, `no_hits`, `not_used` and `failed` without fabricating sources.
- [ ] Run prompt and chat contract tests until GREEN.
- [ ] Commit: `fix: inject session evidence into socratic prompts`.

### Task 5: 资料列表、删除和前端来源可见性

**Files:**
- Modify: `rust-backend/src/api/sessions.rs`
- Modify: `rust-backend/src/api/dto.rs`
- Modify: `rust-backend/src/store/sessions.rs`
- Modify: `rust-backend/tests/session_api.rs`
- Modify: `web/src/api.ts`
- Modify: `web/src/pages/StudentChat.tsx`
- Modify: `web/src/styles.css`
- Modify: `web/scripts/run-client.test.mjs`
- Modify: `web/scripts/student-ui-contract.test.mjs`

**Interfaces:**
- `GET /api/sessions/{id}/documents -> { documents: DocumentSummary[] }`.
- `DELETE /api/sessions/{id}/documents/{document_id} -> 204`.
- `DocumentSummary` exposes id, filename, content_type, size_bytes, chunk_count, index_status, created_at; never exposes server paths.

- [ ] Write failing API authorization/list/delete tests and front-end client contract tests.
- [ ] Implement list/delete endpoints with owner checks and cascading chunk deletion.
- [ ] Write failing UI tests for supported-format text, indexed status, source badges and delete action.
- [ ] Implement a compact session-materials panel and expandable per-answer sources.
- [ ] Add responsive rules that eliminate horizontal page overflow at 1280px and 1920px.
- [ ] Run Web tests and API tests until GREEN.
- [ ] Commit: `feat: show indexed documents and answer sources`.

### Task 6: 追问、拒答和身份反馈回归

**Files:**
- Modify: `rust-backend/src/skills/thinking_flow.rs`
- Modify: `rust-backend/src/agent/domain_boundary.rs`
- Modify: `rust-backend/src/skills/prompt.rs`
- Modify: `rust-backend/tests/chat_contract.rs`
- Modify: `rust-backend/tests/run_api.rs`

**Interfaces:**
- Choice prompts always allow combination/open response unless an explicit exclusive decision is required.
- At most one primary question per normal turn.
- Boundary result distinguishes `allow`, `soft_redirect`, `hard_refuse`.

- [ ] Add failing cases for non-exclusive choices, open replies, depth break after 2–3 questions, local examples, argument comparison and true full-assignment requests.
- [ ] Add/retain failing authorization test proving identical name/student number cannot inherit another principal's history.
- [ ] Implement minimal flow/prompt/boundary changes and keep hard refusal limited to direct complete deliverables.
- [ ] Run focused chat and run API tests until GREEN.
- [ ] Commit: `fix: make socratic guidance adaptive`.

### Task 7: Token 错误、公开 seed 与完整交付验证

**Files:**
- Create: `corpus/examples/session-document-demo.md`
- Create: `scripts/seed_demo_data.sh`
- Modify: `rust-backend/src/error.rs`
- Modify: `rust-backend/src/api/error.rs`
- Modify: `rust-backend/src/agent/run_context.rs`
- Modify: `web/src/api.ts`
- Modify: `README.md`
- Modify: `.gitignore`

**Interfaces:**
- Context overflow error returns code, estimated tokens, configured limit and a Chinese recovery hint.
- Seed script accepts an explicit target database path, refuses existing targets unless `--force`, and never reads Python data.

- [ ] Write failing API/client tests for actionable context-limit and upload error codes.
- [ ] Implement structured errors and Chinese UI messages.
- [ ] Add a synthetic Markdown example and safe seed script; test it only against a temporary path.
- [ ] Verify Git ignores runtime DB/WAL/SHM and tracked files contain no real users, private corpus, secrets or absolute machine paths.
- [ ] Run `cargo fmt --manifest-path rust-backend/Cargo.toml --check`.
- [ ] Run `cargo clippy --manifest-path rust-backend/Cargo.toml --all-targets --all-features -- -D warnings`.
- [ ] Run `cargo test --manifest-path rust-backend/Cargo.toml --all-targets`.
- [ ] Run `npm test` and `npm run build` in `web/`.
- [ ] Start only the Rust service with a temporary database and run the sentinel upload→chat→source-display E2E flow.
- [ ] Review diff and commits; do not push until the implementation and verification results are reported.
- [ ] Commit: `docs: document local evidence workflow`.
