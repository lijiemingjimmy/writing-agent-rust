export type RunStatus =
  | "queued"
  | "running"
  | "completed"
  | "cancelled"
  | "budget_exceeded"
  | "failed";

export type ClientRunStatus = "idle" | RunStatus;

export type ModelUsageCall = {
  purpose: string;
  model: string;
  inputTokens: number;
  outputTokens: number;
  costMicrousd: number;
};

export type RunUsage = {
  inputTokens: number;
  outputTokens: number;
  costMicrousd: number;
  calls: ModelUsageCall[];
};

export type RunBudgets = {
  tokenBudget: number | null;
  costBudgetMicrousd: number | null;
};

export type ProgressEvent = {
  seq: number;
  kind: string;
  payload: Record<string, unknown>;
  createdAt: string | null;
};

export type RunClientState = {
  status: ClientRunStatus;
  shouldReconnect: boolean;
  lastSeq: number;
  currentStep: string | null;
  answer: string | null;
  terminalMessage: string | null;
  usage: RunUsage;
  budgets: RunBudgets;
  events: ProgressEvent[];
};

export type RunEventEnvelope = {
  run_id?: string;
  seq?: number;
  kind?: string;
  payload?: Record<string, unknown>;
  created_at?: string | null;
};

export type RunClientEvent = {
  event: string;
  data: RunEventEnvelope | Record<string, unknown>;
};

export type RunSnapshot = {
  run_id: string;
  session_id: string;
  status: RunStatus;
  current_step: string | null;
  max_steps: number;
  token_budget: number | null;
  cost_budget_microusd: number | null;
  input_tokens: number;
  output_tokens: number;
  cost_microusd: number;
  cancel_reason: string | null;
  error_message: string | null;
  created_at: string | null;
  started_at: string | null;
  finished_at: string | null;
};

export type PublicModelSettings = {
  provider: string;
  endpoint: string;
  name: string;
  apiKeyEnvironment: string;
  apiKeyConfigured: boolean;
  contextLength: number;
  maxOutputTokens: number;
  reasoningMode: string;
  inputPriceMicrousdPerMillion: number;
  outputPriceMicrousdPerMillion: number;
  defaultTokenBudget: number;
  defaultCostBudgetMicrousd: number;
};

export type LifecycleMessage = {
  id: string;
  role: "user" | "assistant";
  content: string;
};

type Settled<T> =
  | { status: "fulfilled"; value: T }
  | { status: "rejected"; reason?: unknown };

export const initialRunState: RunClientState = Object.freeze({
  status: "idle",
  shouldReconnect: true,
  lastSeq: 0,
  currentStep: null,
  answer: null,
  terminalMessage: null,
  usage: Object.freeze({
    inputTokens: 0,
    outputTokens: 0,
    costMicrousd: 0,
    calls: Object.freeze([]) as unknown as ModelUsageCall[]
  }),
  budgets: Object.freeze({ tokenBudget: null, costBudgetMicrousd: null }),
  events: Object.freeze([]) as unknown as ProgressEvent[]
});

const terminalStatuses = new Set<RunStatus>([
  "completed",
  "cancelled",
  "budget_exceeded",
  "failed"
]);

const terminalEvents: Record<string, RunStatus> = {
  "run.completed": "completed",
  "run.cancelled": "cancelled",
  "run.budget_exceeded": "budget_exceeded",
  "run.failed": "failed"
};

const statusRanks: Record<ClientRunStatus, number> = {
  idle: 0,
  queued: 1,
  running: 2,
  completed: 3,
  cancelled: 3,
  budget_exceeded: 3,
  failed: 3
};

const eventLabels: Record<string, string> = {
  "run.started": "任务已开始",
  "step.started": "正在执行",
  "step.completed": "步骤完成",
  "step.failed": "步骤失败",
  "tool.started": "正在检索",
  "search.completed": "检索完成",
  "model.started": "正在请求模型",
  "model.usage": "模型调用完成",
  "answer.delta": "正在生成回复",
  "decision.warning": "判断提醒",
  "run.completed": "任务完成",
  "run.cancelled": "任务已停止",
  "run.budget_exceeded": "已达预算上限",
  "run.failed": "任务失败"
};

export function isTerminalRunStatus(status: ClientRunStatus): status is RunStatus {
  return terminalStatuses.has(status as RunStatus);
}

export function isTerminalRunEvent(kind: string): boolean {
  return Object.hasOwn(terminalEvents, kind);
}

export function reduceRunEvent(
  state: RunClientState,
  incoming: RunClientEvent
): RunClientState {
  const data = asRecord(incoming.data);
  const sequence = optionalNonnegativeInteger(data.seq) ?? 0;
  if (sequence > 0 && sequence <= state.lastSeq) return state;

  const kind = stringValue(data.kind) || incoming.event;
  const payload = asRecord(data.payload ?? data);
  const proposedStatus = terminalEvents[kind]
    ?? (kind === "run.started" || kind === "step.started" ? "running" : state.status);
  const nextStatus = monotonicStatus(state.status, proposedStatus);
  const nextUsage = reduceUsage(state.usage, kind, payload);
  const terminal = isTerminalRunEvent(kind);
  const currentStep = kind === "step.started"
    ? stringValue(payload.step) || state.currentStep
    : state.currentStep;
  const answer = kind === "run.completed"
    ? stringValue(payload.answer) || state.answer
    : state.answer;
  const terminalMessage = terminal ? terminalReason(kind, payload) : state.terminalMessage;
  const progressEvent: ProgressEvent = {
    seq: sequence,
    kind,
    payload,
    createdAt: stringValue(data.created_at) || null
  };

  return {
    ...state,
    status: nextStatus,
    shouldReconnect: state.shouldReconnect && !terminal,
    lastSeq: sequence > 0 ? sequence : state.lastSeq,
    currentStep,
    answer,
    terminalMessage,
    usage: nextUsage,
    events: [...state.events, progressEvent]
  };
}

export function reduceRunSnapshot(
  state: RunClientState,
  snapshot: RunSnapshot
): RunClientState {
  const nextStatus = monotonicStatus(state.status, snapshot.status);
  const snapshotTerminal = isTerminalRunStatus(snapshot.status);
  const stateTerminal = isTerminalRunStatus(state.status);
  return {
    ...state,
    status: nextStatus,
    shouldReconnect: state.shouldReconnect && !snapshotTerminal,
    currentStep: stateTerminal
      ? state.currentStep
      : state.currentStep ?? snapshot.current_step,
    terminalMessage: snapshot.cancel_reason || snapshot.error_message || state.terminalMessage,
    usage: {
      ...state.usage,
      inputTokens: Math.max(state.usage.inputTokens, nonnegativeInteger(snapshot.input_tokens)),
      outputTokens: Math.max(state.usage.outputTokens, nonnegativeInteger(snapshot.output_tokens)),
      costMicrousd: Math.max(state.usage.costMicrousd, nonnegativeInteger(snapshot.cost_microusd))
    },
    budgets: {
      tokenBudget: optionalNonnegativeInteger(snapshot.token_budget),
      costBudgetMicrousd: optionalNonnegativeInteger(snapshot.cost_budget_microusd)
    }
  };
}

export function beginPersistedRunInspection(snapshot: RunSnapshot): RunClientState {
  return reduceRunSnapshot({
    ...initialRunState,
    usage: { ...initialRunState.usage, calls: [] },
    budgets: { ...initialRunState.budgets },
    events: []
  }, snapshot);
}

export function normalizePublicSettings(value: unknown): PublicModelSettings {
  const settings = asRecord(value);
  return {
    provider: stringValue(settings.provider),
    endpoint: stringValue(settings.endpoint),
    name: stringValue(settings.name),
    apiKeyEnvironment: stringValue(settings.api_key_env),
    apiKeyConfigured: settings.api_key_configured === true,
    contextLength: nonnegativeInteger(settings.context_length),
    maxOutputTokens: nonnegativeInteger(settings.max_output_tokens),
    reasoningMode: stringValue(settings.reasoning_mode),
    inputPriceMicrousdPerMillion: nonnegativeInteger(
      settings.input_price_microusd_per_million
    ),
    outputPriceMicrousdPerMillion: nonnegativeInteger(
      settings.output_price_microusd_per_million
    ),
    defaultTokenBudget: nonnegativeInteger(settings.default_token_budget),
    defaultCostBudgetMicrousd: nonnegativeInteger(settings.default_cost_budget_microusd)
  };
}

export function runEventLabel(kind: string): string {
  return eventLabels[kind] || kind;
}

export function progressEventDetail(event: ProgressEvent): string {
  if (event.kind === "decision.warning") {
    return safeProgressText(event.payload.message);
  }
  const step = safeProgressText(event.payload.step);
  const tool = safeProgressText(event.payload.tool);
  const purpose = safeProgressText(event.payload.purpose);
  const hitCount = typeof event.payload.hit_count === "number" ? event.payload.hit_count : null;
  if (step) return step;
  if (tool) return tool;
  if (purpose) return purpose;
  if (hitCount !== null) return `${hitCount} 条结果`;
  return event.createdAt ? new Date(event.createdAt).toLocaleTimeString("zh-CN") : "";
}

export function appendTerminalAssistant(
  messages: LifecycleMessage[],
  runId: string,
  event?: RunClientEvent
): LifecycleMessage[] {
  const answer = terminalAnswer(event);
  const id = `run-${runId}`;
  if (!answer || messages.some((message) => message.id === id)) return messages;
  return [...messages, { id, role: "assistant", content: answer }];
}

export function resolveStartupRestoration<TSession, TMessage>(
  sessionResult: Settled<{ sessions: TSession[] }>,
  rememberedResult: Settled<{ messages: TMessage[] }> | null
): {
  sessions: TSession[] | null;
  messages: TMessage[] | null;
  rememberedFailed: boolean;
} {
  return {
    sessions: sessionResult.status === "fulfilled" ? sessionResult.value.sessions : null,
    messages: rememberedResult?.status === "fulfilled" ? rememberedResult.value.messages : null,
    rememberedFailed: rememberedResult?.status === "rejected"
  };
}

export function buildRunEventsUrl(runId: string, afterSeq = 0, baseUrl = ""): string {
  const root = baseUrl.replace(/\/$/, "");
  const path = `${root}/api/runs/${encodeURIComponent(runId)}/events`;
  return afterSeq > 0 ? `${path}?after_seq=${Math.trunc(afterSeq)}` : path;
}

export function formatCostMicrousd(value: number | bigint): string {
  const microusd = typeof value === "bigint"
    ? value
    : BigInt(Number.isSafeInteger(value) && value >= 0 ? value : 0);
  const dollars = microusd / 1_000_000n;
  const fraction = (microusd % 1_000_000n).toString().padStart(6, "0");
  return `$${dollars}.${fraction}`;
}

function reduceUsage(
  usage: RunUsage,
  kind: string,
  payload: Record<string, unknown>
): RunUsage {
  const input = optionalNonnegativeInteger(payload.input_tokens);
  const output = optionalNonnegativeInteger(payload.output_tokens);
  const cost = optionalNonnegativeInteger(payload.cost_microusd);
  if (input === null && output === null && cost === null) return usage;

  if (kind !== "model.usage") {
    return {
      ...usage,
      inputTokens: Math.max(usage.inputTokens, input ?? 0),
      outputTokens: Math.max(usage.outputTokens, output ?? 0),
      costMicrousd: Math.max(usage.costMicrousd, cost ?? 0)
    };
  }

  const call: ModelUsageCall = {
    purpose: stringValue(payload.purpose),
    model: stringValue(payload.model),
    inputTokens: input ?? 0,
    outputTokens: output ?? 0,
    costMicrousd: cost ?? 0
  };
  return {
    inputTokens: Math.max(
      usage.inputTokens,
      optionalNonnegativeInteger(payload.cumulative_input_tokens)
        ?? usage.inputTokens + call.inputTokens
    ),
    outputTokens: Math.max(
      usage.outputTokens,
      optionalNonnegativeInteger(payload.cumulative_output_tokens)
        ?? usage.outputTokens + call.outputTokens
    ),
    costMicrousd: Math.max(
      usage.costMicrousd,
      optionalNonnegativeInteger(payload.cumulative_cost_microusd)
        ?? usage.costMicrousd + call.costMicrousd
    ),
    calls: [...usage.calls, call]
  };
}

function terminalReason(kind: string, payload: Record<string, unknown>): string | null {
  if (kind === "run.completed") return null;
  return stringValue(payload.reason) || stringValue(payload.message) || null;
}

function terminalAnswer(event?: RunClientEvent): string {
  if (!event || event.event !== "run.completed") return "";
  const payload = asRecord(asRecord(event.data).payload);
  return stringValue(payload.answer);
}

function monotonicStatus(
  current: ClientRunStatus,
  proposed: ClientRunStatus
): ClientRunStatus {
  if (statusRanks[proposed] < statusRanks[current]) return current;
  if (isTerminalRunStatus(current)) return current;
  return proposed;
}

function asRecord(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function stringValue(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function safeProgressText(value: unknown): string {
  if (typeof value !== "string") return "";
  const normalized = value
    .replace(/[\u0000-\u001f\u007f-\u009f]/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return Array.from(normalized).slice(0, 240).join("");
}

function nonnegativeInteger(value: unknown): number {
  return optionalNonnegativeInteger(value) ?? 0;
}

function optionalNonnegativeInteger(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : null;
}
