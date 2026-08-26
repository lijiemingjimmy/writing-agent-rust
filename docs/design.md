# Standalone Rust Course Agent Design

## Goal

Deliver a self-contained Rust-controlled writing coach Agent without sharing Git metadata, runtime services, databases, deployment workflows, or frontend code with the original Python project.

## Repository boundary

- The repository root is `writing-coach-rust-agent` with its own `.git` directory.
- The only permitted remote is `https://github.com/lijiemingjimmy/writing-agent-rust.git` until a Tsinghua Git remote is explicitly added.
- The original `writing-coach-agent` repository is a read-only source of previously authored assets.
- No database, SQLite sidecar, API key, absolute local path, Python runtime, GitHub Pages workflow, Mac mini configuration, or original Git history is copied.

## Runtime boundary

- Rust owns routing, writing state, retrieval, model orchestration, run progress, cancellation, persistence, token accounting, and budget enforcement.
- React provides only the student interface and communicates with one Rust API base.
- A fresh SQLite database is created for the course project. The original `writing_coach.db` is never opened.
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

1. Git common directory is the new repository's own `.git` directory and exactly one worktree exists.
2. No tracked file contains an old remote, old deployment target, local absolute path, Python runtime dependency, teacher API route, or original database name.
3. Rust formatting, Clippy, all Rust tests, all Web tests, and the Web production build pass.
4. The Rust server starts against a temporary fresh database and the student page renders without a Python process.
5. The original repository status, HEAD, and database SHA-256 remain unchanged.
