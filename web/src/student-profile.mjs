export function normalizeStudentProfile(name, studentId) {
  const normalizedName = String(name || "").trim();
  const normalizedStudentId = String(studentId || "").trim();
  if (!normalizedName || !normalizedStudentId) return null;
  return { name: normalizedName, studentId: normalizedStudentId };
}

export function parseStudentProfile(raw) {
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw);
    return normalizeStudentProfile(parsed?.name, parsed?.studentId);
  } catch {
    return null;
  }
}
