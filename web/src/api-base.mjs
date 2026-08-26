export function normalizeAgentApiBase(value = "") {
  const normalized = value.trim().replace(/\/+$/, "");
  if (!normalized) return "";

  let url;
  try {
    url = new URL(normalized);
  } catch {
    throw new Error("Agent API base must be an absolute HTTPS URL");
  }
  if (url.protocol !== "https:") {
    throw new Error("Agent API base must use HTTPS");
  }
  if (url.username || url.password) {
    throw new Error("Agent API base must not contain credentials");
  }
  if (url.search) {
    throw new Error("Agent API base must not contain a query");
  }
  if (url.hash) {
    throw new Error("Agent API base must not contain a fragment");
  }
  return normalized;
}

export function agentApiUrl(path, base = "") {
  if (!path.startsWith("/")) {
    throw new Error("API path must be absolute");
  }
  const normalized = normalizeAgentApiBase(base);
  return normalized ? `${normalized}${path}` : path;
}
