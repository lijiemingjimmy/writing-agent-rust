const basePath = (import.meta.env.BASE_URL || "/").replace(/\/$/, "");

export function appPath(path: string) {
  const normalizedPath = path.startsWith("/") ? path : `/${path}`;
  return `${basePath || ""}/#${normalizedPath}`;
}
