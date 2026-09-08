import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

async function collect(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const chunks = [];
  for (const entry of entries) {
    const target = path.join(directory, entry.name);
    if (entry.isDirectory()) chunks.push(await collect(target));
    else if (/\.(tsx|ts|mjs)$/.test(entry.name)) chunks.push(await readFile(target, "utf8"));
  }
  return chunks.join("\n");
}

test("migrates every teacher API action and critical interaction", async () => {
  const source = await collect(fileURLToPath(new URL("../src/teacher", import.meta.url)));
  for (const token of [
    "fetchTeacherStats", "fetchTeacherStudents", "fetchTeacherStudentDetail",
    "fetchTeacherStudentSummary", "deleteTeacherStudentSession", "askTeacherArchive",
    "summarizeTeacher", "fetchTeacherClassSummary", "fetchTeacherClassInsights",
    "analyzeTeacherJson", "downloadTeacherExport",
    "window.confirm", "writing_context", "route_history", "skill_events",
    "guardrail_triggered", "skipped_records", "examples_by_skill"
  ]) assert.match(source, new RegExp(token));
});

test("downloads teacher exports with an auth header instead of a URL token", async () => {
  const api = await readFile(new URL("../src/api.ts", import.meta.url), "utf8");
  assert.match(api, /downloadTeacherExport/);
  assert.match(api, /teacherAuthHeaders/);
  assert.doesNotMatch(api, /teacherTokenQuery/);
  assert.doesNotMatch(api, /\?teacher_token=/);
});

test("includes every teacher workspace page", async () => {
  const source = await collect(fileURLToPath(new URL("../src/teacher", import.meta.url)));
  for (const label of ["总览", "学生", "对话记录", "班级洞察", "数据导入", "设置"])
    assert.match(source, new RegExp(label));
});
