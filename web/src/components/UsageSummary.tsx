import { formatCostMicrousd, type RunClientState } from "../run-state.ts";

type UsageSummaryProps = {
  state: RunClientState;
};

export function UsageSummary({ state }: UsageSummaryProps) {
  if (state.status === "idle" && state.usage.calls.length === 0) return null;
  const totalTokens = state.usage.inputTokens + state.usage.outputTokens;

  return (
    <section className="usage-summary" aria-labelledby="usage-summary-title">
      <div className="run-panel-heading">
        <h2 id="usage-summary-title">Token 与费用</h2>
        <span>服务端精确计费</span>
      </div>
      <dl className="usage-totals">
        <div><dt>输入 Token</dt><dd>{formatTokens(state.usage.inputTokens)}</dd></div>
        <div><dt>输出 Token</dt><dd>{formatTokens(state.usage.outputTokens)}</dd></div>
        <div><dt>总费用</dt><dd>{formatCostMicrousd(state.usage.costMicrousd)} USD</dd></div>
      </dl>
      <div className="budget-summary" aria-label="服务端执行预算">
        <strong>服务端执行预算（只读）</strong>
        <span>
          Token：{state.budgets.tokenBudget === null
            ? "等待运行状态"
            : `${formatTokens(totalTokens)} / ${formatTokens(state.budgets.tokenBudget)}`}
        </span>
        <span>
          费用：{state.budgets.costBudgetMicrousd === null
            ? "等待运行状态"
            : `${formatCostMicrousd(state.usage.costMicrousd)} / ${formatCostMicrousd(state.budgets.costBudgetMicrousd)} USD`}
        </span>
      </div>
      {state.usage.calls.length > 0 ? (
        <details className="usage-calls">
          <summary>查看每次模型调用（{state.usage.calls.length}）</summary>
          <ol>
            {state.usage.calls.map((call, index) => (
              <li key={`${call.purpose}-${call.model}-${index}`}>
                <strong>{call.purpose || `调用 ${index + 1}`}</strong>
                <span>{call.model || "未知模型"}</span>
                <span>输入 {formatTokens(call.inputTokens)} · 输出 {formatTokens(call.outputTokens)}</span>
                <span>{formatCostMicrousd(call.costMicrousd)} USD</span>
              </li>
            ))}
          </ol>
        </details>
      ) : null}
    </section>
  );
}

function formatTokens(value: number) {
  return value.toLocaleString("zh-CN");
}
