import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const installUrl = new URL("scripts/install-launchd.sh", root);
const uninstallUrl = new URL("scripts/uninstall-launchd.sh", root);
const verifyUrl = new URL("scripts/verify-local-service.sh", root);
const plistUrl = new URL("deploy/macos/wam-rust-agent.plist.template", root);

test("macOS service assets are parameterized, syntactically valid, and dry-run safe", async () => {
  const files = await Promise.all(
    [installUrl, uninstallUrl, verifyUrl, plistUrl].map((url) => readFile(url, "utf8"))
  );
  for (const script of [installUrl, uninstallUrl, verifyUrl]) {
    const syntax = spawnSync("sh", ["-n", fileURLToPath(script)], { encoding: "utf8" });
    assert.equal(syntax.status, 0, syntax.stderr);
  }
  const install = spawnSync("sh", [fileURLToPath(installUrl),
    "--label", "edu.example.wam",
    "--binary", "/opt/wam/bin/writing-coach-server",
    "--config", "/opt/wam/config.toml",
    "--workdir", "/opt/wam",
    "--log-dir", "/opt/wam/logs"
  ], { encoding: "utf8", env: { ...process.env, DRY_RUN: "1" } });
  assert.equal(install.status, 0, install.stderr);
  assert.match(install.stdout, /DRY_RUN/);
  assert.match(files[3], /__BINARY__/);
  assert.match(files[3], /__CONFIG__/);
  assert.match(files[3], /__WORKDIR__/);
  assert.match(files[3], /__STDOUT__/);
  const combined = files.join("\n");
  assert.doesNotMatch(combined, /jiemingli\.top|github\.io|Mac mini|writing-coach-agent/iu);
});
