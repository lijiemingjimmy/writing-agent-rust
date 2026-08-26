import assert from "node:assert/strict";
import test from "node:test";

import { normalizeStudentProfile, parseStudentProfile } from "../src/student-profile.mjs";

test("normalizes a complete student profile", () => {
  assert.deepEqual(normalizeStudentProfile("  张三  ", " 20260001 "), {
    name: "张三",
    studentId: "20260001"
  });
});

test("rejects profiles with a missing name or student ID", () => {
  assert.equal(normalizeStudentProfile("", "20260001"), null);
  assert.equal(normalizeStudentProfile("张三", "  "), null);
});

test("parses a valid saved student profile", () => {
  assert.deepEqual(parseStudentProfile('{"name":" 李四 ","studentId":" 20260002 "}'), {
    name: "李四",
    studentId: "20260002"
  });
});

test("ignores malformed or incomplete saved profiles", () => {
  assert.equal(parseStudentProfile(null), null);
  assert.equal(parseStudentProfile("not-json"), null);
  assert.equal(parseStudentProfile('{"name":"王五"}'), null);
});
