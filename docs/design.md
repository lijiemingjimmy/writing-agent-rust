# Standalone Rust Course Agent Design

## Goal

Deliver a self-contained Rust-controlled writing coach Agent with its own Git metadata, runtime services, database, deployment workflow, and frontend.

## Repository boundary

- The repository root is `writing-coach-rust-agent` with its own `.git` directory.
- The delivery remote is `git@git.tsinghua.edu.cn:rust-course/2026/agent/agent-lijm25.git`; the existing GitHub repository remains an optional public mirror.
- No database, SQLite sidecar, API key, absolute local path, Python runtime, GitHub Pages workflow, Mac mini configuration, or unrelated Git history is included.

## Runtime boundary

- Rust owns routing, writing state, retrieval, model orchestration, run progress, cancellation, persistence, token accounting, and budget enforcement.
- React provides only the student interface and communicates with one Rust API base.
- A fresh SQLite database is created for the course project. Unrelated databases are never opened.
- The project builds, tests, and runs with Rust and Node.js after the Python service is stopped.

## Frontend boundary

- `StudentChat`, run progress, model settings, usage, document upload, and session import/export remain.
- Teacher pages, teacher API functions, teacher authentication, Python port 8000, and dual-backend routing are removed.
- Development proxy routes `/api` and `/health` only to Rust on `127.0.0.1:3000`.
- Production uses a single optional `VITE_AGENT_API_BASE_URL`.

## Data boundary

- Public/course-owned teaching material may be included.
- Original student examples are excluded.
- Skill references to excluded student examples are replaced with synthetic examples under `corpus/examples`.

## Acceptance criteria

1. Git common directory belongs to the standalone Rust repository; development worktrees may exist, but none may point to another repository.
2. No tracked file contains an old remote, old deployment target, local absolute path, Python runtime dependency, teacher API route, or original database name.
3. Rust formatting, Clippy, all Rust tests, all Web tests, and the Web production build pass.
4. The Rust server starts against a temporary fresh database and the student page renders without a Python process.
5. Other repositories and databases remain unchanged.
