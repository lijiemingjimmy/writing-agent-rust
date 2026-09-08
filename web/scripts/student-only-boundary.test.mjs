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

test("the shipped Web application contains independent student and teacher entries", async () => {
  const teacherDirectory = new URL("../src/teacher/", import.meta.url);
  const teacherEntry = new URL("../src/pages/TeacherDashboard.tsx", import.meta.url);
  const studentEntry = new URL("../src/pages/StudentChat.tsx", import.meta.url);
  const main = await readFile(new URL("../src/main.tsx", import.meta.url), "utf8");

  assert.equal(await exists(teacherDirectory), true, "teacher source directory must ship");
  assert.equal(await exists(teacherEntry), true, "teacher entry point must ship");
  assert.equal(await exists(studentEntry), true, "student entry point must remain available");
  assert.match(main, /path\.startsWith\("\/teacher"\)/);
  assert.match(main, /<StudentChat/);
});

test("the package exposes student and teacher development and build commands", async () => {
  const packageJson = JSON.parse(
    await readFile(new URL("../package.json", import.meta.url), "utf8")
  );

  assert.deepEqual(Object.keys(packageJson.scripts).sort(), ["build", "build:teacher", "dev", "dev:teacher", "preview", "test"]);
  assert.equal(packageJson.scripts.build, "vite build");
  assert.equal(packageJson.scripts["dev:teacher"], "vite --host 127.0.0.1 --port 5174");
  assert.equal(packageJson.scripts["build:teacher"], "vite build --outDir dist-teacher");
});
