import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const studentChatPath = new URL("../src/pages/StudentChat.tsx", import.meta.url);
const studentStylesPath = new URL("../src/styles.css", import.meta.url);

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
  assert.match(source, /当前支持 TXT、Markdown/);
  assert.match(source, /会话资料/);
  assert.match(source, /已索引/);
  assert.match(source, /本轮参考了/);
});

test("offers an explicit synthesis action instead of pretending the button is a chat message", async () => {
  const source = await readFile(studentChatPath, "utf8");
  const composer = source.slice(
    source.indexOf('<form className="chat-composer"'),
    source.indexOf("</form>", source.indexOf('<form className="chat-composer"'))
  );

  assert.match(composer, /className="synthesize-button"/);
  assert.match(composer, />\s*形成完整思路\s*</);
  assert.match(source, /action:\s*"synthesize"/);
  assert.match(source, /disabled=\{!sessionId \|\| !messages\.length \|\| busy\}/);
  assert.match(source, /if \(!options\.action\)/);
  assert.match(source, /metadata_json\.action === "synthesize"/);
});

test("keeps run details collapsed across message submission and aligned below session documents", async () => {
  const source = await readFile(studentChatPath, "utf8");
  const styles = await readFile(studentStylesPath, "utf8");

  assert.match(source, /<details className="run-details"/);
  assert.match(source, /<summary>\s*<span>运行详情<\/span>/);
  assert.match(source, /onToggle=.*setRunDetailsOpen/);
  assert.match(source, /useState\(false\)/);
  assert.doesNotMatch(source, /setRunDetailsOpen\(true\)/);
  assert.ok(
    source.indexOf('<details className="run-details"') < source.indexOf('aria-label="导入运行轨迹"')
  );
  assert.match(source, /<AgentProgress state=\{runState\}/);
  assert.match(source, /<UsageSummary state=\{runState\}/);
  assert.match(styles, /\.session-documents,\s*\.run-details\s*\{[^}]*width:\s*min\(1080px, calc\(100% - 64px\)\)/s);
  assert.match(styles, /\.session-documents \+ \.run-details\s*\{\s*margin-top:\s*10px/s);
  assert.match(styles, /\.run-details > summary\s*\{[^}]*padding:\s*10px 14px[^}]*font-weight:\s*700/s);
});

test("does not submit while a Chinese input method is confirming composition", async () => {
  const source = await readFile(studentChatPath, "utf8");
  const handler = source.slice(
    source.indexOf("function handleComposerKey"),
    source.indexOf("function resetRunUi")
  );

  assert.match(handler, /nativeEvent\.isComposing/);
  assert.match(handler, /keyCode === 229/);
  assert.ok(handler.indexOf("nativeEvent.isComposing") < handler.indexOf('event.key === "Enter"'));
  assert.match(handler, /event\.key === "Enter" && !event\.shiftKey/);
});

test("bootstraps a student bearer credential and clears it on invalidation or switching", async () => {
  const source = await readFile(studentChatPath, "utf8");
  const api = await readFile(new URL("../src/api.ts", import.meta.url), "utf8");

  assert.match(source, /await bootstrapStudentAccess\(profile\.name, profile\.studentId\)/);
  assert.match(source, /clearStudentAccess\(\)/);
  assert.match(api, /\/api\/student\/access\/bootstrap/);
  assert.match(api, /Authorization.*Bearer/);
  assert.match(api, /response\.status === 401/);
  assert.match(api, /writing-coach:student-auth-invalid/);
});
