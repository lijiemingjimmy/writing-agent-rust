import assert from "node:assert/strict";
import test from "node:test";
import { parseTeacherRoute, teacherRoutes } from "../src/teacher/teacherRoutes.mjs";
import { pathFromLocation } from "../src/locationPath.mjs";

test("supports every teacher route and the teacher root", () => {
  assert.equal(parseTeacherRoute("/teacher"), "overview");
  for (const route of teacherRoutes) {
    assert.equal(parseTeacherRoute(`/teacher/${route.id}`), route.id);
  }
});

test("falls unknown teacher routes back to overview and excludes student routes", () => {
  assert.equal(parseTeacherRoute("/teacher/unknown"), "overview");
  assert.equal(parseTeacherRoute("/student"), null);
});

test("resolves hash routes and direct deployment entry paths", () => {
  assert.equal(pathFromLocation("/teacher/overview", "/writing-agent-rust/teacher/", "/writing-agent-rust/"), "/teacher/overview");
  assert.equal(pathFromLocation("", "/writing-agent-rust/teacher/", "/writing-agent-rust/"), "/teacher/");
  assert.equal(pathFromLocation("", "/writing-agent-rust/student/", "/writing-agent-rust/"), "/student/");
});
