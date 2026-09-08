export function pathFromLocation(hashPath, pathname, baseUrl = "/") {
  if (hashPath) return hashPath.startsWith("/") ? hashPath : `/${hashPath}`;

  const basePath = baseUrl === "/" ? "" : baseUrl.replace(/\/$/, "");
  const relativePath = basePath && pathname.startsWith(basePath)
    ? pathname.slice(basePath.length)
    : pathname;

  return relativePath.startsWith("/") ? relativePath : `/${relativePath}`;
}
