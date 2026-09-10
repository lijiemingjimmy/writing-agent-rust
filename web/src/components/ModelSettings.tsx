import { FormEvent, useEffect, useRef, useState } from "react";
import {
  fetchModelSettings,
  updateModelSettings,
  type ModelSettingsUpdate
} from "../api";
import { type PublicModelSettings } from "../run-state.ts";

type ModelSettingsProps = {
  open: boolean;
  onClose: () => void;
};

type SettingsForm = {
  provider: string;
  endpoint: string;
  name: string;
  apiKey: string;
  contextLength: string;
  maxOutputTokens: string;
  reasoningMode: string;
  inputPrice: string;
  outputPrice: string;
  defaultTokenBudget: string;
  defaultCostBudgetMicrousd: string;
};

const emptyForm: SettingsForm = {
  provider: "DeepSeek",
  endpoint: "https://api.deepseek.com",
  name: "deepseek-v4-flash",
  apiKey: "",
  contextLength: "",
  maxOutputTokens: "",
  reasoningMode: "",
  inputPrice: "",
  outputPrice: "",
  defaultTokenBudget: "",
  defaultCostBudgetMicrousd: ""
};

export function ModelSettings({ open, onClose }: ModelSettingsProps) {
  const closeHandlerRef = useRef(onClose);
  const dialogRef = useRef<HTMLElement | null>(null);
  closeHandlerRef.current = onClose;
  const [form, setForm] = useState<SettingsForm>(emptyForm);
  const [configuredKey, setConfiguredKey] = useState(false);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [accessCode, setAccessCode] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");

  useEffect(() => {
    if (!open) {
      setAccessCode("");
      setForm((current) => ({ ...current, apiKey: "" }));
      return;
    }
    let active = true;
    setLoading(true);
    setError("");
    setNotice("");
    void fetchModelSettings()
      .then((settings) => {
        if (!active) return;
        setConfiguredKey(settings.apiKeyConfigured);
        setForm(formFromSettings(settings));
      })
      .catch(() => {
        if (active) setError("无法读取模型设置，请检查 Rust 服务。");
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => {
      active = false;
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    function handleKeyDown(event: globalThis.KeyboardEvent) {
      if (event.key === "Escape") {
        closeHandlerRef.current();
        return;
      }
      if (event.key !== "Tab") return;
      const focusable = dialogRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
      );
      if (!focusable?.length) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && (document.activeElement === first || !dialogRef.current?.contains(document.activeElement))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [open]);

  if (!open) return null;

  function updateField(field: keyof SettingsForm, value: string) {
    setForm((current) => ({ ...current, [field]: value }));
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    setError("");
    setNotice("");
    const update = settingsUpdateFromForm(form);
    if (!update) {
      setError("上下文、最大输出和默认预算必须为正整数，价格必须为非负整数。");
      return;
    }
    setSaving(true);
    try {
      const saved = await updateModelSettings(update, fetch, accessCode);
      setConfiguredKey(saved.apiKeyConfigured);
      setForm(formFromSettings(saved));
      setNotice("设置已保存；新运行将使用这些配置。");
    } catch (error) {
      setForm((current) => ({ ...current, apiKey: "" }));
      setError(error instanceof Error ? error.message : "设置未保存，请检查配置。");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="settings-backdrop" onMouseDown={(event) => {
      if (event.target === event.currentTarget) onClose();
    }}>
      <section
        ref={dialogRef}
        id="model-settings-dialog"
        className="model-settings-drawer"
        role="dialog"
        aria-modal="true"
        aria-labelledby="model-settings-title"
      >
        <header>
          <div>
            <h2 id="model-settings-title">模型与费用设置</h2>
            <p>临时 Key 仅保存在 Rust 服务内存中，页面不会回填。</p>
          </div>
          <button type="button" className="drawer-close" onClick={onClose} aria-label="关闭模型设置" autoFocus>×</button>
        </header>
        <form className="model-settings-form" onSubmit={submit} aria-busy={loading || saving}>
          <p className="field-wide model-quick-start">首次使用可直接填入 DeepSeek API Key，其余连接信息已提供默认值。<button type="button" disabled={loading || saving} onClick={() => setForm((current) => ({ ...current, provider: "DeepSeek", endpoint: "https://api.deepseek.com", name: "deepseek-v4-flash", reasoningMode: "high" }))}>使用 DeepSeek 默认配置</button></p>
          <label><span>Provider</span><input list="model-providers" value={form.provider} onChange={(event) => updateField("provider", event.target.value)} required /><datalist id="model-providers"><option value="DeepSeek" /><option value="OpenAI" /><option value="openai-compatible" /></datalist></label>
          <label className="field-wide"><span>API Endpoint</span><input type="url" value={form.endpoint} onChange={(event) => updateField("endpoint", event.target.value)} required /></label>
          <label><span>Model</span><input value={form.name} onChange={(event) => updateField("name", event.target.value)} required /></label>
          <label>
            <span>临时 API Key</span>
            <input type="password" value={form.apiKey} onChange={(event) => updateField("apiKey", event.target.value)} autoComplete="new-password" placeholder={configuredKey ? "已配置（留空则不变）" : "粘贴你的 API Key"} />
          </label>
          <label className="field-wide"><span>共享配置访问码（教师访问码）</span>
            <input type="password" value={accessCode} onChange={(event) => setAccessCode(event.target.value)} autoComplete="off" placeholder="已在教师端登录可留空" />
          </label>
          <p className="field-wide">修改共享配置需要服务端设置教师访问码；更换服务地址或 Provider 时，请重新填写 API Key。</p>
          <details className="field-wide model-advanced-settings"><summary>高级设置：上下文、输出长度与费用预算</summary><div className="model-settings-form">
          <label><span>上下文长度</span><input type="number" min="1" step="1" value={form.contextLength} onChange={(event) => updateField("contextLength", event.target.value)} required /></label>
          <label><span>最大输出 Token</span><input type="number" min="1" step="1" value={form.maxOutputTokens} onChange={(event) => updateField("maxOutputTokens", event.target.value)} required /></label>
          <label><span>Thinking / Reasoning</span><input value={form.reasoningMode} onChange={(event) => updateField("reasoningMode", event.target.value)} required /></label>
          <label><span>输入价格（微美元/百万 Token）</span><input type="number" min="0" step="1" value={form.inputPrice} onChange={(event) => updateField("inputPrice", event.target.value)} required /></label>
          <label><span>输出价格（微美元/百万 Token）</span><input type="number" min="0" step="1" value={form.outputPrice} onChange={(event) => updateField("outputPrice", event.target.value)} required /></label>
          <label><span>新运行默认 Token 预算</span><input type="number" min="1" step="1" value={form.defaultTokenBudget} onChange={(event) => updateField("defaultTokenBudget", event.target.value)} required /></label>
          <label><span>新运行默认费用预算（微美元）</span><input type="number" min="1" step="1" value={form.defaultCostBudgetMicrousd} onChange={(event) => updateField("defaultCostBudgetMicrousd", event.target.value)} required /></label>
          <p className="server-budget-note field-wide">预算由 Rust 服务端强制执行；保存后仅影响新建运行。价格为手动估算值，请按模型服务商的实际价格调整。</p>
          </div></details>
          {error ? <p className="settings-message error" role="alert">{error}</p> : null}
          {notice ? <p className="settings-message success" role="status">{notice}</p> : null}
          <div className="settings-actions field-wide">
            <button type="button" onClick={onClose}>取消</button>
            <button type="submit" className="primary" disabled={loading || saving}>{saving ? "保存中…" : "保存设置"}</button>
          </div>
        </form>
      </section>
    </div>
  );
}

function formFromSettings(settings: PublicModelSettings): SettingsForm {
  return {
    provider: settings.provider.toLowerCase() === "deepseek" ? "DeepSeek" : settings.provider,
    endpoint: settings.endpoint,
    name: settings.name,
    apiKey: "",
    contextLength: String(settings.contextLength),
    maxOutputTokens: String(settings.maxOutputTokens),
    reasoningMode: settings.reasoningMode,
    inputPrice: String(settings.inputPriceMicrousdPerMillion),
    outputPrice: String(settings.outputPriceMicrousdPerMillion),
    defaultTokenBudget: String(settings.defaultTokenBudget),
    defaultCostBudgetMicrousd: String(settings.defaultCostBudgetMicrousd)
  };
}

function settingsUpdateFromForm(form: SettingsForm): ModelSettingsUpdate | null {
  const contextLength = positiveInteger(form.contextLength);
  const maxOutputTokens = positiveInteger(form.maxOutputTokens);
  const inputPrice = nonnegativeInteger(form.inputPrice);
  const outputPrice = nonnegativeInteger(form.outputPrice);
  const defaultTokenBudget = positiveInteger(form.defaultTokenBudget);
  const defaultCostBudgetMicrousd = positiveInteger(form.defaultCostBudgetMicrousd);
  if (contextLength === null || maxOutputTokens === null || inputPrice === null || outputPrice === null || defaultTokenBudget === null || defaultCostBudgetMicrousd === null) return null;
  const trimmedApiKey = form.apiKey.trim();
  return {
    provider: form.provider.trim(),
    endpoint: form.endpoint.trim(),
    name: form.name.trim(),
    ...(trimmedApiKey ? { api_key: form.apiKey } : {}),
    context_length: contextLength,
    max_output_tokens: maxOutputTokens,
    reasoning_mode: form.reasoningMode.trim(),
    input_price_microusd_per_million: inputPrice,
    output_price_microusd_per_million: outputPrice,
    default_token_budget: defaultTokenBudget,
    default_cost_budget_microusd: defaultCostBudgetMicrousd
  };
}

function positiveInteger(value: string) {
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : null;
}

function nonnegativeInteger(value: string) {
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : null;
}
