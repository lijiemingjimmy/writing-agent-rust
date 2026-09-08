export const teacherRoutes = [
  { id: "overview", label: "总览" },
  { id: "students", label: "学生" },
  { id: "conversations", label: "对话记录" },
  { id: "insights", label: "班级洞察" },
  { id: "import", label: "数据导入" },
  { id: "settings", label: "设置" }
];

export function parseTeacherRoute(path) {
  if (!path.startsWith("/teacher")) return null;
  const id = path.split("/")[2] || "overview";
  return teacherRoutes.some((route) => route.id === id) ? id : "overview";
}
