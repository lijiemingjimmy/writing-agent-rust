import assert from "node:assert/strict";
import test from "node:test";

async function loadApiBase() {
  try {
    return await import("../src/api-base.mjs");
  } catch {
    return null;
  }
}

test("one Rust API base resolves same-origin and HTTPS endpoints", async () => {
  const api = await loadApiBase();
  assert.ok(api, "student-only api-base module must exist");

  assert.equal(api.agentApiUrl("/api/runs", ""), "/api/runs");
  assert.equal(
    api.agentApiUrl("/api/runs", "https://rust-agent.example/"),
    "https://rust-agent.example/api/runs"
  );
});

test("the Rust API base rejects unsafe remote endpoints", async () => {
  const api = await loadApiBase();
  assert.ok(api, "student-only api-base module must exist");

  for (const unsafe of [
    "http://rust-agent.example",
    "https://user:secret@rust-agent.example",
    "https://rust-agent.example/api?token=secret",
    "https://rust-agent.example/api#fragment"
  ]) {
    assert.throws(() => api.normalizeAgentApiBase(unsafe), /HTTPS|credentials|query|fragment/);
  }
});
