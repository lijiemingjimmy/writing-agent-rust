import {
  buildRunEventsUrl,
  isTerminalRunEvent,
  normalizePublicSettings,
  type PublicModelSettings,
  type RunClientEvent,
  type RunSnapshot
} from "./run-state.ts";
import { agentApiUrl, normalizeAgentApiBase } from "./api-base.mjs";

const agentApiBase = normalizeAgentApiBase(import.meta.env?.VITE_AGENT_API_BASE_URL || "");
export const studentAccessStorageKey = "writingCoach.studentAccess";

export type StudentAccess = {
  access_token: string;
  principal_id: string;
  student_name: string;
  student_id: string;
};

function studentApiUrl(path: string) {
  return agentApiUrl(path, agentApiBase);
}

export type ChatResponse = {
  session_id: string;
  reply: string;
  current_skill: string | null;
  awaiting_slots: string[];
  metadata: Record<string, unknown>;
};

export type CreateRunRequest = {
  session_id?: string | null;
  user_id?: string | null;
  student_name?: string | null;
  student_id?: string | null;
  message: string;
  action?: "synthesize";
  enable_web_search?: boolean;
};

export type CreateRunResponse = {
  run_id: string;
  session_id: string;
};

export type ImportSessionResponse = {
  session_id: string;
  run_ids: string[];
};

export type ModelSettingsUpdate = {
  provider?: string;
  endpoint?: string;
  name?: string;
  api_key?: string;
  context_length?: number;
  max_output_tokens?: number;
  reasoning_mode?: string;
  input_price_microusd_per_million?: number;
  output_price_microusd_per_million?: number;
  default_token_budget?: number;
  default_cost_budget_microusd?: number;
};

export type SessionExportV1 = {
  schema: "writing-coach.session";
  version: 1;
  source_session_id: string;
  session: Record<string, unknown>;
  state: Record<string, unknown>;
  messages: unknown[];
  documents: unknown[];
  skill_events: unknown[];
  runs: unknown[];
  run_events: unknown[];
  model_calls: unknown[];
};

type EventSourceLike = {
  onerror: ((event: Event) => void) | null;
  onmessage?: ((event: MessageEvent<string>) => void) | null;
  addEventListener(type: string, listener: (event: MessageEvent<string>) => void): void;
  close(): void;
};

type TimerToken = ReturnType<typeof setTimeout> | number;

export type RunEventSubscription = {
  close: () => void;
  getLatestSeq: () => number;
};

export type RunEventCallbacks = {
  onEvent: (event: RunClientEvent) => void;
  onError?: (message: string) => void;
  onTerminal?: (event: RunClientEvent) => void;
};

export type RunEventOptions = {
  eventSourceFactory?: (url: string) => EventSourceLike;
  setTimer?: (callback: () => void, delay: number) => TimerToken;
  clearTimer?: (token: TimerToken) => void;
  initialBackoffMs?: number;
  maxBackoffMs?: number;
};

type Fetcher = typeof fetch;

export async function bootstrapStudentAccess(
  studentName: string,
  studentId: string,
  fetcher: Fetcher = fetch
): Promise<StudentAccess> {
  const response = await fetcher(studentApiUrl("/api/student/access/bootstrap"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ student_name: studentName, student_id: studentId })
  });
  if (!response.ok) throw new Error("学生身份凭据创建失败。");
  const access = await response.json() as StudentAccess;
  if (!access.access_token || !access.student_id || !access.student_name) {
    throw new Error("学生身份凭据无效。");
  }
  window.localStorage.setItem(studentAccessStorageKey, JSON.stringify(access));
  return access;
}

export function hasStudentAccess(): boolean {
  return readStudentAccess() !== null;
}

export function clearStudentAccess(): void {
  window.localStorage.removeItem(studentAccessStorageKey);
}

function readStudentAccess(): StudentAccess | null {
  try {
    const raw = window.localStorage.getItem(studentAccessStorageKey);
    if (!raw) return null;
    const value = JSON.parse(raw) as Partial<StudentAccess>;
    return typeof value.access_token === "string" && value.access_token.length > 0
      && typeof value.student_id === "string" && value.student_id.length > 0
      && typeof value.student_name === "string" && value.student_name.length > 0
      ? value as StudentAccess
      : null;
  } catch {
    return null;
  }
}

async function studentFetch(url: string, init: RequestInit = {}, fetcher: Fetcher = fetch): Promise<Response> {
  const normalizedHeaders = new Headers(init.headers);
  const access = readStudentAccess();
  if (access) normalizedHeaders.set("Authorization", `Bearer ${access.access_token}`);
  const headers: Record<string, string> = {};
  normalizedHeaders.forEach((value, key) => { headers[key] = value; });
  const response = await fetcher(url, { ...init, headers });
  if (response.status === 401) {
    clearStudentAccess();
    window.dispatchEvent(new Event("writing-coach:student-auth-invalid"));
  }
  return response;
}

const runEventKinds = [
  "run.started",
  "step.started",
  "step.completed",
  "step.failed",
  "tool.started",
  "search.completed",
  "model.started",
  "model.usage",
  "answer.delta",
  "decision.warning",
  "run.completed",
  "run.cancelled",
  "run.budget_exceeded",
  "run.failed"
];

const cancellationRequests = new Map<string, Promise<RunSnapshot>>();
const maximumRememberedCancellations = 128;
const maximumSessionImportBytes = 8 * 1024 * 1024;
const maximumDocumentUploadBytes = 256 * 1024;

export type DocumentUploadResponse = {
  document_id: string;
  session_id: string;
  filename: string;
  content_type: "text/plain" | "text/markdown";
  size_bytes: number;
  chunk_count: number;
  index_status: "ready";
};

export type SessionDocument = Omit<DocumentUploadResponse, "size_bytes" | "index_status"> & {
  size_bytes: number | null;
  index_status: "ready" | "legacy";
  created_at: string | null;
};

export async function createRun(payload: CreateRunRequest): Promise<CreateRunResponse> {
  return requestJson<CreateRunResponse>(studentApiUrl("/api/runs"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload)
  }, "无法创建运行任务");
}

export async function getRun(runId: string): Promise<RunSnapshot> {
  return requestJson<RunSnapshot>(studentApiUrl(`/api/runs/${encodeURIComponent(runId)}`), {}, "无法读取运行状态");
}

export function subscribeRunEvents(
  runId: string,
  callbacks: RunEventCallbacks,
  options: RunEventOptions = {}
): RunEventSubscription {
  const makeEventSource = options.eventSourceFactory
    ?? ((url: string) => new EventSource(url) as EventSourceLike);
  const schedule = options.setTimer ?? ((callback, delay) => window.setTimeout(callback, delay));
  const unschedule = options.clearTimer ?? ((token) => window.clearTimeout(token as number));
  const initialBackoff = Math.max(100, options.initialBackoffMs ?? 500);
  const maximumBackoff = Math.max(initialBackoff, options.maxBackoffMs ?? 4000);
  let latestSeq = 0;
  let retryCount = 0;
  let source: EventSourceLike | null = null;
  let retryTimer: TimerToken | null = null;
  let closed = false;
  let terminal = false;

  function clearRetryTimer() {
    if (retryTimer === null) return;
    unschedule(retryTimer);
    retryTimer = null;
  }

  function closeSource(candidate?: EventSourceLike) {
    const target = candidate ?? source;
    if (!target) return;
    if (source === target) source = null;
    target.onerror = null;
    target.close();
  }

  function close() {
    if (closed) return;
    closed = true;
    clearRetryTimer();
    closeSource();
  }

  function accept(kind: string, rawData: string, candidate: EventSourceLike) {
    if (closed || terminal || source !== candidate) return;
    let data: unknown;
    try {
      data = JSON.parse(rawData);
    } catch {
      callbacks.onError?.("运行进度数据无法解析。");
      return;
    }
    if (!data || typeof data !== "object") return;
    const envelope = data as Record<string, unknown>;
    const sequence = typeof envelope.seq === "number" && Number.isSafeInteger(envelope.seq)
      ? envelope.seq
      : 0;
    if (sequence <= latestSeq) return;
    latestSeq = sequence;
    retryCount = 0;
    const event: RunClientEvent = {
      event: typeof envelope.kind === "string" ? envelope.kind : kind,
      data: envelope
    };
    callbacks.onEvent(event);
    if (isTerminalRunEvent(event.event)) {
      terminal = true;
      clearRetryTimer();
      closeSource(candidate);
      callbacks.onTerminal?.(event);
    }
  }

  function connect() {
    if (closed || terminal || source) return;
    clearRetryTimer();
    const candidate = makeEventSource(buildRunEventsUrl(runId, latestSeq, agentApiBase));
    source = candidate;
    for (const kind of runEventKinds) {
      candidate.addEventListener(kind, (event) => accept(kind, event.data, candidate));
    }
    candidate.onmessage = (event) => accept("message", event.data, candidate);
    candidate.onerror = () => {
      if (closed || terminal || source !== candidate) return;
      closeSource(candidate);
      if (retryTimer !== null) return;
      const delay = Math.min(initialBackoff * (2 ** retryCount), maximumBackoff);
      retryCount += 1;
      retryTimer = schedule(() => {
        retryTimer = null;
        connect();
      }, delay);
    };
  }

  connect();
  return { close, getLatestSeq: () => latestSeq };
}

export function cancelRun(
  runId: string,
  reason = "student_stop",
  fetcher: Fetcher = fetch
): Promise<RunSnapshot> {
  const existing = cancellationRequests.get(runId);
  if (existing) return existing;
  const request = requestJson<RunSnapshot>(
    studentApiUrl(`/api/runs/${encodeURIComponent(runId)}/cancel`),
    {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ reason })
    },
    "无法停止运行任务",
    fetcher
  ).catch((error) => {
    cancellationRequests.delete(runId);
    throw error;
  });
  cancellationRequests.set(runId, request);
  while (cancellationRequests.size > maximumRememberedCancellations) {
    const oldest = cancellationRequests.keys().next().value;
    if (typeof oldest !== "string") break;
    cancellationRequests.delete(oldest);
  }
  return request;
}

export async function fetchModelSettings(): Promise<PublicModelSettings> {
  const value = await requestJson<unknown>(studentApiUrl("/api/settings/model"), {}, "无法读取模型设置");
  return normalizePublicSettings(value);
}

export async function updateModelSettings(
  update: ModelSettingsUpdate,
  fetcher: Fetcher = fetch
): Promise<PublicModelSettings> {
  validateModelSettingsUpdate(update);
  const value = await requestJson<unknown>(studentApiUrl("/api/settings/model"), {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(update)
  }, "模型设置未保存", fetcher);
  return normalizePublicSettings(value);
}

function validateModelSettingsUpdate(update: ModelSettingsUpdate): void {
  for (const value of [
    update.context_length,
    update.max_output_tokens,
    update.default_token_budget,
    update.default_cost_budget_microusd
  ]) {
    if (value !== undefined && (!Number.isSafeInteger(value) || value <= 0)) {
      throw new Error("上下文、输出与默认预算必须为安全的正整数。");
    }
  }
  for (const value of [
    update.input_price_microusd_per_million,
    update.output_price_microusd_per_million
  ]) {
    if (value !== undefined && (!Number.isSafeInteger(value) || value < 0)) {
      throw new Error("模型价格必须为安全的非负整数。");
    }
  }
}

export async function exportSession(sessionId: string): Promise<SessionExportV1> {
  return requestJson<SessionExportV1>(
    studentApiUrl(`/api/sessions/${encodeURIComponent(sessionId)}/export`),
    {},
    "无法导出会话"
  );
}

export async function importSession(
  value: SessionExportV1,
  fetcher: Fetcher = fetch
): Promise<ImportSessionResponse> {
  return requestJson<ImportSessionResponse>(studentApiUrl("/api/sessions/import"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(value)
  }, "无法导入会话", fetcher);
}

export async function uploadSessionDocument(
  sessionId: string,
  file: Pick<File, "name" | "size" | "type" | "arrayBuffer">,
  fetcher: Fetcher = fetch
): Promise<DocumentUploadResponse> {
  if (file.size <= 0 || file.size > maximumDocumentUploadBytes) {
    throw new Error("资料必须是不超过 256 KiB 的文本文件。");
  }
  const filename = file.name;
  if (
    !filename
    || filename !== filename.trim()
    || new TextEncoder().encode(filename).byteLength > 255
    || filename.includes("/")
    || filename.includes("\\")
    || [...filename].some((character) => /[\u0000-\u001f\u007f]/.test(character))
  ) {
    throw new Error("资料文件名不安全。");
  }
  const lowerName = filename.toLowerCase();
  const contentType = lowerName.endsWith(".txt")
    ? "text/plain"
    : lowerName.endsWith(".md")
      ? "text/markdown"
      : null;
  if (!contentType || filename.startsWith(".")) {
    throw new Error("请选择 TXT 或 Markdown 资料文件。");
  }
  if (file.type && file.type !== contentType) {
    throw new Error("资料文件的类型与扩展名不匹配。");
  }
  const bytes = new Uint8Array(await file.arrayBuffer());
  if (bytes.byteLength !== file.size) {
    throw new Error("资料文件大小在读取期间发生了变化。");
  }
  try {
    new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error("资料文件必须是有效的 UTF-8 文本。");
  }
  return requestJson<DocumentUploadResponse>(
    studentApiUrl(
      `/api/sessions/${encodeURIComponent(sessionId)}/documents?filename=${encodeURIComponent(filename)}`
    ),
    {
      method: "POST",
      headers: { "content-type": contentType },
      body: bytes
    },
    "资料上传失败",
    fetcher
  );
}

export async function fetchSessionDocuments(
  sessionId: string,
  fetcher: Fetcher = fetch
): Promise<{ documents: SessionDocument[] }> {
  return requestJson<{ documents: SessionDocument[] }>(
    studentApiUrl(`/api/sessions/${encodeURIComponent(sessionId)}/documents`),
    {},
    "无法读取会话资料",
    fetcher
  );
}

export async function deleteSessionDocument(
  sessionId: string,
  documentId: string,
  fetcher: Fetcher = fetch
): Promise<void> {
  const response = await studentFetch(
    studentApiUrl(`/api/sessions/${encodeURIComponent(sessionId)}/documents/${encodeURIComponent(documentId)}`),
    { method: "DELETE" },
    fetcher
  );
  if (!response.ok) throw new Error("无法删除会话资料");
}

export async function readSessionImportFile(
  file: Pick<File, "name" | "size" | "type" | "text">
): Promise<SessionExportV1> {
  if (file.size <= 0 || file.size > maximumSessionImportBytes) {
    throw new Error("导入文件必须是不超过 8 MiB 的 JSON 文件。");
  }
  if (!file.name.toLowerCase().endsWith(".json") || (file.type && !file.type.includes("json"))) {
    throw new Error("请选择一个 JSON 会话导出文件。");
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(await file.text());
  } catch {
    throw new Error("会话导出 JSON 无法解析。");
  }
  if (!isBasicSessionExport(parsed)) {
    throw new Error("会话导出的格式或版本不受支持。");
  }
  return parsed;
}

type DownloadDependencies = {
  createObjectUrl: (blob: Blob) => string;
  revokeObjectUrl: (url: string) => void;
  clickDownload: (url: string, filename: string) => void;
};

export function downloadSessionExport(
  value: SessionExportV1,
  sessionId: string,
  dependencies?: DownloadDependencies
): void {
  const defaults: DownloadDependencies = {
    createObjectUrl: (blob) => URL.createObjectURL(blob),
    revokeObjectUrl: (url) => URL.revokeObjectURL(url),
    clickDownload: (url, filename) => {
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = filename;
      anchor.click();
    }
  };
  const actions = dependencies ?? defaults;
  const blob = new Blob([`${JSON.stringify(value, null, 2)}\n`], { type: "application/json" });
  const objectUrl = actions.createObjectUrl(blob);
  try {
    actions.clickDownload(objectUrl, `writing-coach-session-${sessionId}.json`);
  } finally {
    actions.revokeObjectUrl(objectUrl);
  }
}

function isBasicSessionExport(value: unknown): value is SessionExportV1 {
  if (!isRecord(value)) return false;
  const candidate = value;
  const sourceId = candidate.source_session_id;
  const session = candidate.session;
  return candidate.schema === "writing-coach.session"
    && candidate.version === 1
    && typeof sourceId === "string"
    && /^[0-9a-f]{32}$/.test(sourceId)
    && isRecord(session)
    && session.id === sourceId
    && isRecord(candidate.state)
    && ["messages", "documents", "skill_events", "runs", "run_events", "model_calls"]
      .every((key) => Array.isArray(candidate[key]));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

async function requestJson<T>(
  url: string,
  init: RequestInit,
  fallbackMessage: string,
  fetcher: Fetcher = fetch
): Promise<T> {
  const response = await studentFetch(url, init, fetcher);
  if (!response.ok) {
    let message = fallbackMessage;
    try {
      const value = await response.json() as { error?: unknown };
      if (typeof value.error === "string" && value.error.length <= 200) message = value.error;
    } catch {
      // Keep the fixed client message; never surface raw provider or secret-bearing bodies.
    }
    throw new Error(message);
  }
  return response.json() as Promise<T>;
}

export type StudentProgress = {
  stage?: string;
  intent?: string;
  current_skill?: string | null;
  thinking_stage?: string | null;
  thinking_task?: string | null;
  topic?: string | null;
  context_summary?: string | null;
  research_question?: string | null;
  selected_path?: string | null;
  choice_reason?: string | null;
  socratic_rounds?: number;
  pending_questions?: string[];
  next_task?: string;
  needs_teacher_confirmation?: boolean;
};

export type SessionSummary = {
  session_id: string;
  user_id: string | null;
  student_name?: string | null;
  student_id?: string | null;
  task_type: string | null;
  stage: string;
  preview: string | null;
  message_count: number;
  created_at: string | null;
  updated_at: string | null;
};

export type HistoryMessage = {
  id: string;
  session_id: string;
  role: "user" | "assistant";
  content: string;
  metadata_json: Record<string, unknown>;
};

export async function sendChat(payload: {
  session_id?: string | null;
  user_id?: string | null;
  student_name?: string | null;
  student_id?: string | null;
  message: string;
  response_mode?: "chat" | "synthesize";
  enable_web_search?: boolean;
}): Promise<ChatResponse> {
  const response = await studentFetch(studentApiUrl("/api/chat"), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(payload)
  });
  if (!response.ok) throw new Error(await response.text());
  return response.json();
}

export async function fetchSessions(userId?: string | null): Promise<{ sessions: SessionSummary[] }> {
  const query = userId ? `?user_id=${encodeURIComponent(userId)}` : "";
  const response = await studentFetch(studentApiUrl(`/api/sessions${query}`));
  if (!response.ok) throw new Error(await response.text());
  return response.json();
}

export async function fetchSessionMessages(
  sessionId: string
): Promise<{ messages: HistoryMessage[] }> {
  const response = await studentFetch(studentApiUrl(`/api/sessions/${sessionId}/messages`));
  if (!response.ok) throw new Error(await response.text());
  return response.json();
}
