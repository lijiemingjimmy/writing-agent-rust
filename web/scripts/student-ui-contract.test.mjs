import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const studentChatPath = new URL("../src/pages/StudentChat.tsx", import.meta.url);

test("keeps the student chat focused on history, conversation, and the composer", async () => {
  const source = await readFile(studentChatPath, "utf8");

  for (const requiredText of [
    "WAM · 写作主体性导师",
    "新建对话",
    "历史对话",
    "有什么可以帮忙的？",
    "切换学生",
    "联网",
    "我是写作与沟通智能体，可以帮助你选题、检索资料、课程答疑以及训练评价。"
  ]) {
    assert.match(source, new RegExp(requiredText));
  }

  assert.doesNotMatch(source, /<span>写沟<\/span>/);
  assert.doesNotMatch(source, /<span>写<\/span>/);

  for (const removedText of ["写作进度", "快捷提问", "能力参考", "记录提示", "联网搜索开启"]) {
    assert.doesNotMatch(source, new RegExp(removedText));
  }
});

test("keeps the student access card copy minimal", async () => {
  const source = await readFile(studentChatPath, "utf8");

  assert.match(source, />学生端访问</);
  assert.doesNotMatch(source, /写作与沟通过程教练<\/span>/);
  assert.doesNotMatch(source, /用于区分学习记录与恢复历史对话/);
});

test("opens imported trajectory Runs through a selectable persisted replay inspector", async () => {
  const source = await readFile(studentChatPath, "utf8");

  assert.match(source, /imported\.run_ids/);
  assert.match(source, /inspectImportedRun/);
  assert.match(source, /aria-label="导入运行轨迹"/);
  assert.match(source, /<select/);
  assert.match(source, /导入轨迹/);
});

test("offers a bounded text or Markdown upload for the active Rust-backed session", async () => {
  const source = await readFile(studentChatPath, "utf8");

  assert.match(source, /uploadSessionDocument/);
  assert.match(source, />上传资料</);
  assert.match(source, /accept="\.txt,\.md,text\/plain,text\/markdown"/);
  assert.match(source, /disabled=\{!sessionId \|\| busy\}/);
  assert.match(source, /请先开始一次对话/);
});

test("offers an explicit synthesis action instead of pretending the button is a chat message", async () => {
  const source = await readFile(studentChatPath, "utf8");

  assert.match(source, />形成思路</);
  assert.match(source, /action:\s*"synthesize"/);
  assert.match(source, /disabled=\{!sessionId \|\| !messages\.length \|\| busy\}/);
  assert.match(source, /if \(!options\.action\)/);
  assert.match(source, /metadata_json\.action === "synthesize"/);
});
