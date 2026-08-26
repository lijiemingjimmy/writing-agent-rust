# Dependencies and Provenance

## Rust crates

主要开源依赖包括 Axum、Tokio、SQLx、Serde、Reqwest、genai、tower-http、Tracing、Chrono、UUID、Regex 和 Glob。精确版本及传递依赖以 `rust-backend/Cargo.lock` 为准，各项目许可证以对应 crate 发布信息为准。

## Web packages

学生界面使用 React、React DOM、React Markdown、remark-gfm、TypeScript、Vite 和 `@vitejs/plugin-react`。精确版本及传递依赖以 `web/package-lock.json` 为准。

## Reused project assets

本项目复用了作者此前写作教练项目中的 React 学生界面、课程 Skill 配置、课程语料组织方式和业务测试案例。Agent 主控、运行状态、取消、Token/费用、会话轨迹和 Rust API 由本课程项目中的 Rust 实现承担。原项目的 Git 历史、Python 服务、教师端、数据库、部署配置和真实用户数据均未包含。

开发过程中使用了 AI 编程助手；完整原始对话和开发开销按课程要求另行提交。
