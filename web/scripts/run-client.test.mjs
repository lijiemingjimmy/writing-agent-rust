import assert from "node:assert/strict";
import test from "node:test";

import * as runState from "../src/run-state.ts";

let apiPromise;
function loadApi() {
  apiPromise ??= import("../src/api.ts");
  return apiPromise;
}

function required(name) {
  assert.equal(typeof runState[name], "function", `${name} must be implemented`);
  return runState[name];
}

function initial() {
  assert.ok(runState.initialRunState, "initialRunState must be implemented");
  return runState.initialRunState;
}

test("terminal completion stops reconnect and exposes the assistant answer", () => {
  const reduceRunEvent = required("reduceRunEvent");
  const state = reduceRunEvent(initial(), {
    event: "run.completed",
    data: {
      run_id: "0123456789abcdef0123456789abcdef",
      seq: 7,
      kind: "run.completed",
      payload: { answer: "已完成", metadata: { current_skill: "topic" } },
      created_at: "2026-08-26T10:00:00Z"
    }
  });

  assert.equal(state.status, "completed");
  assert.equal(state.shouldReconnect, false);
  assert.equal(state.answer, "已完成");
  assert.equal(state.lastSeq, 7);
});

test("cancelled, budget-exceeded, and failed events are distinct terminal states", () => {
  const reduceRunEvent = required("reduceRunEvent");
  for (const fixture of [
    ["run.cancelled", "cancelled", { reason: "student_stop" }, "student_stop"],
    ["run.budget_exceeded", "budget_exceeded", { reason: "cost budget exhausted" }, "cost budget exhausted"],
    ["run.failed", "failed", { message: "agent execution failed" }, "agent execution failed"]
  ]) {
    const [event, expectedStatus, payload, expectedReason] = fixture;
    const state = reduceRunEvent(initial(), {
      event,
      data: { seq: 3, kind: event, payload }
    });
    assert.equal(state.status, expectedStatus);
    assert.equal(state.shouldReconnect, false);
    assert.equal(state.terminalMessage, expectedReason);
  }
});

test("event sequence is monotonic and duplicate or out-of-order SSE is ignored", () => {
  const reduceRunEvent = required("reduceRunEvent");
  const first = reduceRunEvent(initial(), {
    event: "step.started",
    data: { seq: 2, kind: "step.started", payload: { step: "route_skill" } }
  });
  const duplicate = reduceRunEvent(first, {
    event: "step.started",
    data: { seq: 2, kind: "step.started", payload: { step: "wrong_duplicate" } }
  });
  const outOfOrder = reduceRunEvent(duplicate, {
    event: "step.completed",
    data: { seq: 1, kind: "step.completed", payload: { step: "wrong_old" } }
  });

  assert.strictEqual(duplicate, first);
  assert.strictEqual(outOfOrder, first);
  assert.equal(first.lastSeq, 2);
  assert.equal(first.currentStep, "route_skill");
  assert.equal(first.events.length, 1);
});

test("model usage keeps per-call input/output separate and uses cumulative totals", () => {
  const reduceRunEvent = required("reduceRunEvent");
  const state = reduceRunEvent(initial(), {
    event: "model.usage",
    data: {
      seq: 4,
      kind: "model.usage",
      payload: {
        purpose: "coach",
        model: "test-model",
        input_tokens: 1200,
        output_tokens: 300,
        cost_microusd: 2400,
        cumulative_input_tokens: 2200,
        cumulative_output_tokens: 450,
        cumulative_cost_microusd: 4100
      }
    }
  });

  assert.deepEqual(state.usage.calls[0], {
    purpose: "coach",
    model: "test-model",
    inputTokens: 1200,
    outputTokens: 300,
    costMicrousd: 2400
  });
  assert.equal(state.usage.inputTokens, 2200);
  assert.equal(state.usage.outputTokens, 450);
  assert.equal(state.usage.costMicrousd, 4100);
});

test("micro-US-dollar formatting is exact without floating-point accumulation", () => {
  const formatCostMicrousd = required("formatCostMicrousd");
  assert.equal(formatCostMicrousd(0), "$0.000000");
  assert.equal(formatCostMicrousd(2400), "$0.002400");
  assert.equal(formatCostMicrousd(2_400_001), "$2.400001");
});

test("public settings normalization never retains or prefills a returned API key", () => {
  const normalizePublicSettings = required("normalizePublicSettings");
  const settings = normalizePublicSettings({
    provider: "openai-compatible",
    endpoint: "http://127.0.0.1:1234/v1",
    name: "local-model",
    api_key_configured: true,
    api_key: "sk-must-not-survive",
    context_length: 32768,
    max_output_tokens: 4096,
    reasoning_mode: "medium",
    input_price_microusd_per_million: 2000000,
    output_price_microusd_per_million: 8000000
  });

  assert.equal(Object.hasOwn(settings, "api_key"), false);
  assert.equal(JSON.stringify(settings).includes("sk-must-not-survive"), false);
  assert.equal(settings.apiKeyConfigured, true);
});

test("reconnect URL replays strictly after the latest accepted sequence", () => {
  const buildRunEventsUrl = required("buildRunEventsUrl");
  assert.equal(
    buildRunEventsUrl("0123456789abcdef0123456789abcdef", 17),
    "/api/runs/0123456789abcdef0123456789abcdef/events?after_seq=17"
  );
  assert.equal(
    buildRunEventsUrl("0123456789abcdef0123456789abcdef", 0, "https://coach.test/"),
    "https://coach.test/api/runs/0123456789abcdef0123456789abcdef/events"
  );
});

test("GET Run snapshot is authoritative for usage totals and enforced budgets", () => {
  const reduceRunSnapshot = required("reduceRunSnapshot");
  const state = reduceRunSnapshot(initial(), {
    run_id: "0123456789abcdef0123456789abcdef",
    session_id: "fedcba9876543210fedcba9876543210",
    status: "running",
    current_step: "call_model",
    max_steps: 32,
    token_budget: 12000,
    cost_budget_microusd: 900000,
    input_tokens: 2200,
    output_tokens: 450,
    cost_microusd: 4100,
    cancel_reason: null,
    error_message: null,
    created_at: null,
    started_at: null,
    finished_at: null
  });

  assert.equal(state.usage.inputTokens, 2200);
  assert.equal(state.usage.outputTokens, 450);
  assert.equal(state.usage.costMicrousd, 4100);
  assert.equal(state.budgets.tokenBudget, 12000);
  assert.equal(state.budgets.costBudgetMicrousd, 900000);
});

test("stale snapshots cannot regress lifecycle, usage, reconnect, answer, or step", () => {
  const reduceRunEvent = required("reduceRunEvent");
  const reduceRunSnapshot = required("reduceRunSnapshot");
  const running = reduceRunEvent(initial(), {
    event: "model.usage",
    data: {
      seq: 8,
      kind: "model.usage",
      payload: {
        input_tokens: 120,
        output_tokens: 30,
        cost_microusd: 900,
        cumulative_input_tokens: 500,
        cumulative_output_tokens: 80,
        cumulative_cost_microusd: 1700
      }
    }
  });
  const completed = reduceRunEvent(running, {
    event: "run.completed",
    data: { seq: 9, kind: "run.completed", payload: { answer: "authoritative answer" } }
  });
  const stale = reduceRunSnapshot(completed, {
    run_id: "run",
    session_id: "session",
    status: "running",
    current_step: "old-step",
    max_steps: 32,
    token_budget: 100,
    cost_budget_microusd: 200,
    input_tokens: 100,
    output_tokens: 10,
    cost_microusd: 1000,
    cancel_reason: null,
    error_message: null,
    created_at: null,
    started_at: null,
    finished_at: null
  });

  assert.equal(stale.status, "completed");
  assert.equal(stale.shouldReconnect, false);
  assert.equal(stale.lastSeq, 9);
  assert.equal(stale.answer, "authoritative answer");
  assert.equal(stale.usage.inputTokens, 500);
  assert.equal(stale.usage.outputTokens, 80);
  assert.equal(stale.usage.costMicrousd, 1700);
  assert.notEqual(stale.currentStep, "old-step");
});

test("a terminal snapshot does not consume the authoritative terminal event answer", () => {
  const reduceRunEvent = required("reduceRunEvent");
  const reduceRunSnapshot = required("reduceRunSnapshot");
  const snapshot = reduceRunSnapshot(initial(), {
    run_id: "run",
    session_id: "session",
    status: "completed",
    current_step: "final",
    max_steps: 32,
    token_budget: 100,
    cost_budget_microusd: 200,
    input_tokens: 5,
    output_tokens: 2,
    cost_microusd: 9,
    cancel_reason: null,
    error_message: null,
    created_at: null,
    started_at: null,
    finished_at: null
  });
  const replayed = reduceRunEvent(snapshot, {
    event: "run.completed",
    data: { seq: 12, kind: "run.completed", payload: { answer: "persisted replay answer" } }
  });

  assert.equal(snapshot.answer, null);
  assert.equal(replayed.answer, "persisted replay answer");
  assert.equal(replayed.lastSeq, 12);
});

test("persisted Run inspection starts terminal and accepts usage plus terminal SSE replay", () => {
  const beginPersistedRunInspection = required("beginPersistedRunInspection");
  const reduceRunEvent = required("reduceRunEvent");
  const snapshot = beginPersistedRunInspection({
    run_id: "11111111111111111111111111111111",
    session_id: "22222222222222222222222222222222",
    status: "completed",
    current_step: "answer",
    max_steps: 32,
    token_budget: 1000,
    cost_budget_microusd: 5000,
    input_tokens: 13,
    output_tokens: 8,
    cost_microusd: 19,
    cancel_reason: null,
    error_message: null,
    created_at: "2026-08-26T10:00:00Z",
    started_at: "2026-08-26T10:00:01Z",
    finished_at: "2026-08-26T10:00:02Z"
  });
  const usage = reduceRunEvent(snapshot, {
    event: "model.usage",
    data: {
      seq: 4,
      kind: "model.usage",
      payload: {
        purpose: "answer",
        model: "fixture",
        input_tokens: 13,
        output_tokens: 8,
        cost_microusd: 19,
        cumulative_input_tokens: 13,
        cumulative_output_tokens: 8,
        cumulative_cost_microusd: 19
      }
    }
  });
  const replayed = reduceRunEvent(usage, {
    event: "run.completed",
    data: { seq: 5, kind: "run.completed", payload: { answer: "already in history" } }
  });

  assert.equal(snapshot.status, "completed");
  assert.equal(snapshot.shouldReconnect, false);
  assert.equal(replayed.status, "completed");
  assert.equal(replayed.events.length, 2);
  assert.equal(replayed.usage.calls.length, 1);
  assert.equal(replayed.answer, "already in history");
});

test("terminal assistant answer remains when canonical history refresh fails", () => {
  const appendTerminalAssistant = required("appendTerminalAssistant");
  const messages = [{ id: "user-1", role: "user", content: "hello" }];
  const afterTerminal = appendTerminalAssistant(messages, "run-1", {
    event: "run.completed",
    data: { payload: { answer: "answer before refresh" } }
  });
  const afterDuplicate = appendTerminalAssistant(afterTerminal, "run-1", {
    event: "run.completed",
    data: { payload: { answer: "duplicate" } }
  });

  assert.deepEqual(afterTerminal.at(-1), {
    id: "run-run-1",
    role: "assistant",
    content: "answer before refresh"
  });
  assert.strictEqual(afterDuplicate, afterTerminal);
});

test("startup restoration keeps a successful session list when remembered history fails", () => {
  const resolveStartupRestoration = required("resolveStartupRestoration");
  const resolved = resolveStartupRestoration(
    { status: "fulfilled", value: { sessions: [{ session_id: "available" }] } },
    { status: "rejected", reason: new Error("404") }
  );

  assert.deepEqual(resolved.sessions, [{ session_id: "available" }]);
  assert.equal(resolved.messages, null);
  assert.equal(resolved.rememberedFailed, true);
});

test("public settings normalize editable default budgets and update sends only supported fields", async () => {
  const normalizePublicSettings = required("normalizePublicSettings");
  const normalized = normalizePublicSettings({
    provider: "openai",
    endpoint: "http://127.0.0.1:1/v1",
    name: "model",
    api_key_env: "KEY",
    api_key_configured: false,
    context_length: 8000,
    max_output_tokens: 1000,
    reasoning_mode: "medium",
    input_price_microusd_per_million: 2,
    output_price_microusd_per_million: 3,
    default_token_budget: 4321,
    default_cost_budget_microusd: 9876
  });
  assert.equal(normalized.defaultTokenBudget, 4321);
  assert.equal(normalized.defaultCostBudgetMicrousd, 9876);

  const api = await loadApi();
  let body;
  const update = {
    default_token_budget: 2222,
    default_cost_budget_microusd: 3333
  };
  await api.updateModelSettings(update, async (_url, init) => {
    body = JSON.parse(init.body);
    return jsonResponse({
      provider: "openai",
      endpoint: "http://127.0.0.1:1/v1",
      name: "model",
      api_key_env: "KEY",
      api_key_configured: false,
      context_length: 8000,
      max_output_tokens: 1000,
      reasoning_mode: "medium",
      input_price_microusd_per_million: 2,
      output_price_microusd_per_million: 3,
      ...update
    });
  });
  assert.deepEqual(body, update);
});

test("settings client rejects invalid default budgets before making a request", async () => {
  const api = await loadApi();
  let requests = 0;
  const fakeFetch = async () => {
    requests += 1;
    return jsonResponse({});
  };
  await assert.rejects(
    api.updateModelSettings({ default_token_budget: 0 }, fakeFetch),
    /正整数/
  );
  await assert.rejects(
    api.updateModelSettings({ default_cost_budget_microusd: Number.MAX_SAFE_INTEGER + 1 }, fakeFetch),
    /正整数/
  );
  assert.equal(requests, 0);
});

test("decision warnings are registered SSE progress with a student-facing label", async () => {
  const api = await loadApi();
  let source;
  const accepted = [];
  api.subscribeRunEvents("warning-run", { onEvent: (event) => accepted.push(event) }, {
    eventSourceFactory: (url) => {
      source = new FakeEventSource(url);
      return source;
    }
  });
  source.emit("decision.warning", {
    seq: 2,
    kind: "decision.warning",
    payload: { message: "needs clarification" }
  });

  assert.equal(accepted[0]?.event, "decision.warning");
  assert.match(required("runEventLabel")("decision.warning"), /提醒|注意|判断/);
});

test("decision warning detail is bounded, control-free, and ignores non-string payloads", () => {
  const progressEventDetail = required("progressEventDetail");
  const detail = progressEventDetail({
    seq: 2,
    kind: "decision.warning",
    payload: { message: `  路由\u0000\n提醒 ${"x".repeat(300)}  ` },
    createdAt: null
  });

  assert.match(detail, /^路由 提醒 /);
  assert.equal(detail.length, 240);
  assert.doesNotMatch(detail, /[\u0000-\u001f\u007f-\u009f]/);
  assert.equal(progressEventDetail({
    seq: 3,
    kind: "decision.warning",
    payload: { message: { secret: "must-not-render" } },
    createdAt: null
  }), "");
});

test("EventSource reconnects from latest sequence with bounded timers and complete cleanup", async () => {
  const api = await loadApi();
  const sources = [];
  const timers = new Map();
  const delays = [];
  let timerId = 0;
  const accepted = [];
  const subscription = api.subscribeRunEvents(
    "0123456789abcdef0123456789abcdef",
    { onEvent: (event) => accepted.push(event) },
    {
      eventSourceFactory: (url) => {
        const source = new FakeEventSource(url);
        sources.push(source);
        return source;
      },
      setTimer: (callback, delay) => {
        timerId += 1;
        delays.push(delay);
        timers.set(timerId, callback);
        return timerId;
      },
      clearTimer: (id) => timers.delete(id),
      initialBackoffMs: 250,
      maxBackoffMs: 1000
    }
  );

  assert.equal(sources[0].url, "/api/runs/0123456789abcdef0123456789abcdef/events");
  sources[0].emit("step.started", {
    seq: 4,
    kind: "step.started",
    payload: { step: "route_skill" }
  });
  sources[0].fail();
  sources[0].fail();
  assert.deepEqual(delays, [250]);
  assert.equal(timers.size, 1);
  const reconnect = timers.values().next().value;
  timers.clear();
  reconnect();

  assert.equal(
    sources[1].url,
    "/api/runs/0123456789abcdef0123456789abcdef/events?after_seq=4"
  );
  assert.equal(accepted.length, 1);
  subscription.close();
  subscription.close();
  assert.equal(sources[1].closeCount, 1);
  assert.equal(timers.size, 0);
});

test("EventSource ignores duplicate sequence and closes for every terminal event", async () => {
  const api = await loadApi();
  for (const terminal of [
    "run.completed",
    "run.cancelled",
    "run.budget_exceeded",
    "run.failed"
  ]) {
    const accepted = [];
    let source;
    api.subscribeRunEvents(
      `run-${terminal}`,
      { onEvent: (event) => accepted.push(event) },
      {
        eventSourceFactory: (url) => {
          source = new FakeEventSource(url);
          return source;
        }
      }
    );
    source.emit("step.started", { seq: 2, kind: "step.started", payload: {} });
    source.emit("step.started", { seq: 2, kind: "step.started", payload: {} });
    source.emit("step.completed", { seq: 1, kind: "step.completed", payload: {} });
    source.emit(terminal, { seq: 3, kind: terminal, payload: {} });
    source.fail();
    assert.deepEqual(accepted.map((event) => event.data.seq), [2, 3]);
    assert.equal(source.closeCount, 1);
  }
});

test("cancelRun coalesces concurrent and repeated cancellation requests", async () => {
  const api = await loadApi();
  let requestCount = 0;
  const fakeFetch = async () => {
    requestCount += 1;
    return jsonResponse({ status: "cancelled" });
  };
  const runId = "cancel-once-0123456789abcdef";

  const [first, second] = await Promise.all([
    api.cancelRun(runId, "student_stop", fakeFetch),
    api.cancelRun(runId, "student_stop", fakeFetch)
  ]);
  const third = await api.cancelRun(runId, "ignored", fakeFetch);

  assert.equal(first.status, "cancelled");
  assert.deepEqual(second, first);
  assert.deepEqual(third, first);
  assert.equal(requestCount, 1);
});

test("session import validates JSON shape and export download always revokes its object URL", async () => {
  const api = await loadApi();
  const validExport = {
    schema: "writing-coach.session",
    version: 1,
    source_session_id: "0123456789abcdef0123456789abcdef",
    session: { id: "0123456789abcdef0123456789abcdef" },
    state: {},
    messages: [],
    documents: [],
    skill_events: [],
    runs: [],
    run_events: [],
    model_calls: []
  };
  const file = {
    name: "session.json",
    size: JSON.stringify(validExport).length,
    type: "application/json",
    text: async () => JSON.stringify(validExport)
  };
  assert.deepEqual(await api.readSessionImportFile(file), validExport);
  await assert.rejects(
    api.readSessionImportFile({ ...file, size: 8 * 1024 * 1024 + 1 }),
    /8 MiB/
  );
  await assert.rejects(
    api.readSessionImportFile({ ...file, text: async () => "{}" }),
    /会话导出/
  );
  await assert.rejects(
    api.readSessionImportFile({
      ...file,
      text: async () => JSON.stringify({ ...validExport, session: [] })
    }),
    /会话导出/
  );

  const actions = [];
  api.downloadSessionExport(validExport, validExport.source_session_id, {
    createObjectUrl: () => {
      actions.push("create");
      return "blob:test";
    },
    revokeObjectUrl: (url) => actions.push(`revoke:${url}`),
    clickDownload: (url, filename) => actions.push(`click:${url}:${filename}`)
  });
  assert.deepEqual(actions, [
    "create",
    "click:blob:test:writing-coach-session-0123456789abcdef0123456789abcdef.json",
    "revoke:blob:test"
  ]);
});

test("session import client preserves imported Run IDs in stable response order", async () => {
  const api = await loadApi();
  const requests = [];
  const exported = {
    schema: "writing-coach.session",
    version: 1,
    source_session_id: "11111111111111111111111111111111",
    session: { id: "11111111111111111111111111111111" },
    state: {},
    messages: [],
    documents: [],
    skill_events: [],
    runs: [],
    run_events: [],
    model_calls: []
  };
  const imported = await api.importSession(exported, async (url, init) => {
    requests.push([url, init.method]);
    return jsonResponse({
      session_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      run_ids: [
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "cccccccccccccccccccccccccccccccc"
      ]
    });
  });

  assert.deepEqual(requests, [["/api/sessions/import", "POST"]]);
  assert.deepEqual(imported.run_ids, [
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    "cccccccccccccccccccccccccccccccc"
  ]);
});

test("student document upload validates UTF-8 and sends only bounded text to the Rust session route", async () => {
  const api = await loadApi();
  const requests = [];
  const bytes = new TextEncoder().encode("# 访谈\n学生观察");
  const uploaded = await api.uploadSessionDocument(
    "0123456789abcdef0123456789abcdef",
    {
      name: "field notes.md",
      size: bytes.byteLength,
      type: "text/markdown",
      arrayBuffer: async () => bytes.buffer
    },
    async (url, init) => {
      requests.push({ url, init });
      return jsonResponse({
        document_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        session_id: "0123456789abcdef0123456789abcdef",
        filename: "field notes.md",
        content_type: "text/markdown",
        size_bytes: bytes.byteLength
      });
    }
  );

  assert.equal(
    requests[0].url,
    "/api/sessions/0123456789abcdef0123456789abcdef/documents?filename=field%20notes.md"
  );
  assert.equal(requests[0].init.method, "POST");
  assert.equal(requests[0].init.headers["content-type"], "text/markdown");
  assert.equal(new TextDecoder().decode(requests[0].init.body), "# 访谈\n学生观察");
  assert.equal(uploaded.document_id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
});

test("student document upload rejects unsafe paths, types, invalid UTF-8, and oversize before fetch", async () => {
  const api = await loadApi();
  let requests = 0;
  const fakeFetch = async () => {
    requests += 1;
    return jsonResponse({});
  };
  const validBytes = new TextEncoder().encode("notes");
  const fixture = (overrides = {}) => ({
    name: "notes.txt",
    size: validBytes.byteLength,
    type: "text/plain",
    arrayBuffer: async () => validBytes.buffer,
    ...overrides
  });

  await assert.rejects(
    api.uploadSessionDocument("session", fixture({ name: "../notes.md" }), fakeFetch),
    /文件名/
  );
  await assert.rejects(
    api.uploadSessionDocument("session", fixture({ name: "notes.pdf" }), fakeFetch),
    /TXT 或 Markdown/
  );
  await assert.rejects(
    api.uploadSessionDocument("session", fixture({ name: "notes.md", type: "text/plain" }), fakeFetch),
    /类型/
  );
  await assert.rejects(
    api.uploadSessionDocument("session", fixture({ size: 256 * 1024 + 1 }), fakeFetch),
    /256 KiB/
  );
  await assert.rejects(
    api.uploadSessionDocument(
      "session",
      fixture({ size: 2, arrayBuffer: async () => Uint8Array.from([0xff, 0xfe]).buffer }),
      fakeFetch
    ),
    /UTF-8/
  );
  assert.equal(requests, 0);
});

class FakeEventSource {
  constructor(url) {
    this.url = url;
    this.closeCount = 0;
    this.listeners = new Map();
  }

  addEventListener(kind, listener) {
    const listeners = this.listeners.get(kind) || [];
    listeners.push(listener);
    this.listeners.set(kind, listeners);
  }

  emit(kind, data) {
    for (const listener of this.listeners.get(kind) || []) {
      listener({ data: JSON.stringify(data) });
    }
  }

  fail() {
    this.onerror?.(new Event("error"));
  }

  close() {
    this.closeCount += 1;
  }
}

function jsonResponse(value, ok = true) {
  return {
    ok,
    status: ok ? 200 : 400,
    json: async () => value,
    text: async () => JSON.stringify(value)
  };
}
