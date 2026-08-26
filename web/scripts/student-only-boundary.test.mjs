import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import test from "node:test";

async function exists(url) {
  try {
    await access(url);
    return true;
  } catch {
    return false;
  }
}

test("the shipped Web application contains no teacher application", async () => {
  const teacherDirectory = new URL("../src/teacher/", import.meta.url);
  const teacherEntry = new URL("../src/pages/TeacherDashboard.tsx", import.meta.url);

  assert.equal(await exists(teacherDirectory), false, "teacher source directory must not ship");
  assert.equal(await exists(teacherEntry), false, "teacher entry point must not ship");
});

test("the shipped package exposes only student development and build commands", async () => {
  const packageJson = JSON.parse(
    await readFile(new URL("../package.json", import.meta.url), "utf8")
  );

  assert.deepEqual(Object.keys(packageJson.scripts).sort(), ["build", "dev", "preview", "test"]);
  assert.equal(packageJson.scripts.build, "vite build");
});
