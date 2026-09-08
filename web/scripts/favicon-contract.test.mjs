import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

test("declares a bundled favicon instead of requesting the missing default icon", async () => {
  const html = await readFile(new URL("../index.html", import.meta.url), "utf8");
  assert.match(html, /rel="icon"/);
  assert.match(html, /data:image\/svg\+xml/);
});
