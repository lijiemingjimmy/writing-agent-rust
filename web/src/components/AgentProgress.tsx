import {
  progressEventDetail,
  runEventLabel,
  type ProgressEvent,
  type RunClientState
} from "../run-state.ts";

type AgentProgressProps = {
  state: RunClientState;
  stopping: boolean;
};

const statusLabels: Record<RunClientState["status"], string> = {
  idle: "等待开始",
  queued: "正在排队",
  running: "正在运行",
  completed: "已完成",
  cancelled: "已停止",
  budget_exceeded: "已达预算上限",
  failed: "运行失败"
};

export function AgentProgress({ state, stopping }: AgentProgressProps) {
  if (state.status === "idle" && state.events.length === 0) return null;
  const visibleEvents = state.events.slice(-12);
  const liveStatus = stopping ? "正在停止…" : statusLabels[state.status];

  return (
    <section className="agent-progress" aria-labelledby="agent-progress-title">
      <div className="run-panel-heading">
        <h2 id="agent-progress-title">Agent 运行轨迹</h2>
        <span className={`run-status status-${state.status}`} role="status" aria-live="polite">
          {liveStatus}
        </span>
      </div>
      {visibleEvents.length > 0 ? (
        <ol className="agent-timeline">
          {visibleEvents.map((event, index) => (
            <li key={eventKey(event, index)} className={event.kind.startsWith("run.") ? "run-event" : ""}>
              <span aria-hidden="true" />
              <div>
                <strong>{runEventLabel(event.kind)}</strong>
                <small>{progressEventDetail(event)}</small>
              </div>
            </li>
          ))}
        </ol>
      ) : (
        <p className="run-muted">正在等待第一个进度事件…</p>
      )}
      {state.terminalMessage ? (
        <p className="run-terminal-message" role={state.status === "failed" ? "alert" : "status"}>
          {state.terminalMessage}
        </p>
      ) : null}
    </section>
  );
}

function eventKey(event: ProgressEvent, index: number) {
  return event.seq > 0 ? `${event.seq}-${event.kind}` : `${event.kind}-${index}`;
}
