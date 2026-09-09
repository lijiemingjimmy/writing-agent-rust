# Git and Frontend Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create an independent course repository containing a Rust Agent and student-only React frontend without touching the original repository.

**Architecture:** Export only approved files into a fresh repository, enforce the boundary with executable audits, reduce the Web client to one Rust API, and validate against a fresh temporary SQLite database. The original repository remains read-only throughout.

**Tech Stack:** Rust 2024, Axum, Tokio, SQLx/SQLite, React 19, TypeScript, Vite 6, Node test runner, shell isolation checks.

**Spec:** `docs/design.md`

## Global Constraints

- Limit changes to the current repository.
- Never open the original `writing_coach.db` from the new Rust process.
- Never copy `.git`, SQLite databases or sidecars, secrets, Python environments, build outputs, student records, or old deployment workflows.
- Do not push the new repository until all isolation, Rust, Web, and browser checks pass.

---

### Task 1: Establish executable isolation boundaries

**Files:**
- Create: `.gitignore`
- Create: `scripts/check-isolation.sh`
- Create: `web/scripts/student-only-boundary.test.mjs`

- [ ] Add behavior tests that fail while the copied Web tree still contains teacher/Python routing.
- [ ] Run the focused Web test and confirm the expected failure.
- [ ] Add the repository isolation script and run it against controlled failing fixtures.
- [ ] Add ignore rules for secrets, databases, sidecars, Rust/Node build outputs, and session exports.

### Task 2: Reduce the frontend to the student Rust application

**Files:**
- Modify: `web/src/main.tsx`
- Create: `web/src/api-base.mjs`
- Modify: `web/src/api.ts`
- Modify: `web/vite.config.ts`
- Modify: `web/package.json`
- Modify: `web/src/styles.css`
- Delete in new repository only: `web/src/teacher/`, `web/src/pages/TeacherDashboard.tsx`, teacher/Pages scripts and tests

- [ ] Render only `StudentChat`.
- [ ] Replace the audience-based API topology with one optional `VITE_AGENT_API_BASE_URL`.
- [ ] Remove teacher API types, calls, tokens, routes, build flags, and Python proxy.
- [ ] Remove teacher-only CSS and route helpers.
- [ ] Run focused boundary tests, all Web tests, and the production build.

### Task 3: Remove backend and data migration coupling

**Files:**
- Modify: `rust-backend/config.example.toml`
- Modify: `rust-backend/migrations/`
- Modify: `rust-backend/tests/`
- Modify: `skills/**/*.yaml`
- Modify: `rust-backend/README.md`

- [ ] Change the example database to `rust_course_demo.db`.
- [ ] Replace copied-user-database and Python compatibility tests with fresh-database Rust tests.
- [ ] Remove Skill references to excluded original student examples.
- [ ] Remove Python, Pages, old database, and migration-period instructions from documentation.
- [ ] Run Rust formatting, Clippy, and all-target tests.

### Task 4: Initialize and verify independent Git state

**Files:**
- Create: `.git/` using `git init --initial-branch=main`
- Create: `README.md`
- Create: `THIRD_PARTY.md`
- Create: `scripts/verify-course.sh`

- [ ] Initialize a fresh Git repository only in the new directory.
- [ ] Set `origin` to `https://github.com/lijiemingjimmy/writing-agent-rust.git`.
- [ ] Verify one worktree, a local common directory, no old objects/remotes, and no forbidden tracked files.
- [ ] Run the complete Rust/Web/isolation verification script.
- [ ] Start Rust against a temporary database, start Vite, and perform browser verification.
- [ ] Recheck the original HEAD, status, database hash, and sidecars.
- [ ] Commit and push only the new repository after every gate succeeds.
