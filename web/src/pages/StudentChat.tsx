import { FormEvent, KeyboardEvent, useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  SessionSummary,
  bootstrapStudentAccess,
  cancelRun,
  clearStudentAccess,
  createRun,
  deleteSessionDocument,
  downloadSessionExport,
  exportSession,
  fetchSessionMessages,
  fetchSessionDocuments,
  fetchSessions,
  getRun,
  hasStudentAccess,
  importSession,
  readSessionImportFile,
  subscribeRunEvents,
  uploadSessionDocument,
  type HistoryMessage,
  type SessionDocument,
  type RunEventSubscription
} from "../api";
import { AgentProgress } from "../components/AgentProgress";
import { ModelSettings } from "../components/ModelSettings";
import { UsageSummary } from "../components/UsageSummary";
import {
  initialRunState,
  appendTerminalAssistant,
  beginPersistedRunInspection,
  isTerminalRunStatus,
  reduceRunEvent,
  reduceRunSnapshot,
  resolveStartupRestoration,
  type RunClientEvent,
  type RunClientState
} from "../run-state.ts";
import { normalizeStudentProfile, parseStudentProfile } from "../student-profile.mjs";

type Message = {
  id: string;
  role: "user" | "assistant";
  content: string;
  metadata_json?: Record<string, unknown>;
};

const studentProfileStorageKey = "writingCoach.studentProfile";

type StudentProfile = {
  name: string;
  studentId: string;
};

export function StudentChat() {
  const [savedProfile] = useState<StudentProfile | null>(readSavedProfile);
  const savedAccessValid = Boolean(savedProfile && hasStudentAccess());
  const initialStudentId = savedAccessValid ? savedProfile?.studentId || "" : "";
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [history, setHistory] = useState<SessionSummary[]>([]);
  const [historyError, setHistoryError] = useState("");
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [restoring, setRestoring] = useState(Boolean(initialStudentId));
  const [loadingSession, setLoadingSession] = useState(false);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [webSearchEnabled, setWebSearchEnabled] = useState(false);
  const [studentName, setStudentName] = useState(savedAccessValid ? savedProfile?.name || "" : "");
  const [studentId, setStudentId] = useState(initialStudentId);
  const [profileConfirmed, setProfileConfirmed] = useState(savedAccessValid);
  const [profileError, setProfileError] = useState("");
  const [runId, setRunId] = useState<string | null>(null);
  const [importedRunIds, setImportedRunIds] = useState<string[]>([]);
  const [inspectingImportedRun, setInspectingImportedRun] = useState(false);
  const [runState, setRunState] = useState<RunClientState>(freshRunState);
  const [stopping, setStopping] = useState(false);
  const [finalizing, setFinalizing] = useState(false);
  const [runPending, setRunPending] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [transferBusy, setTransferBusy] = useState(false);
  const [transferMessage, setTransferMessage] = useState("");
  const [transferError, setTransferError] = useState("");
  const [documentBusy, setDocumentBusy] = useState(false);
  const [documentMessage, setDocumentMessage] = useState("");
  const [documentError, setDocumentError] = useState("");
  const [documents, setDocuments] = useState<SessionDocument[]>([]);
  const composerRef = useRef<HTMLTextAreaElement | null>(null);
  const settingsButtonRef = useRef<HTMLButtonElement | null>(null);
  const importRef = useRef<HTMLInputElement | null>(null);
  const documentRef = useRef<HTMLInputElement | null>(null);
  const subscriptionRef = useRef<RunEventSubscription | null>(null);
  const mountedRef = useRef(true);
  const runInFlightRef = useRef(false);
  const runGenerationRef = useRef(0);
  const finalizedRunsRef = useRef(new Set<string>());

  const runActive = runState.status === "queued" || runState.status === "running";
  const busy = restoring || loadingSession || runActive || runPending || finalizing || transferBusy || documentBusy || inspectingImportedRun;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      runGenerationRef.current += 1;
      subscriptionRef.current?.close();
      subscriptionRef.current = null;
    };
  }, []);

  useEffect(() => {
    function invalidateStudentAccess() {
      resetRunUi();
      window.localStorage.removeItem(studentProfileStorageKey);
      setStudentName("");
      setStudentId("");
      setProfileConfirmed(false);
      setProfileError("登录凭据已失效，请重新确认身份。");
      setSessionId(null);
      setHistory([]);
      setMessages([]);
    }
    window.addEventListener("writing-coach:student-auth-invalid", invalidateStudentAccess);
    return () => window.removeEventListener("writing-coach:student-auth-invalid", invalidateStudentAccess);
  }, []);

  useEffect(() => {
    if (!initialStudentId) return;
    let active = true;
    const generation = runGenerationRef.current + 1;
    runGenerationRef.current = generation;
    const savedSessionId = window.localStorage.getItem(activeSessionStorageKey(initialStudentId));
    setRestoring(true);
    setLoadingHistory(true);
    const sessionsRequest = fetchSessions(initialStudentId);
    const messagesRequest = savedSessionId
      ? fetchSessionMessages(savedSessionId)
      : Promise.resolve({ messages: [] as HistoryMessage[] });
    void Promise.allSettled([sessionsRequest, messagesRequest])
      .then(([sessionsResult, messagesResult]) => {
        if (!active || generation !== runGenerationRef.current) return;
        const restoration = resolveStartupRestoration(
          sessionsResult,
          savedSessionId ? messagesResult : null
        );
        if (restoration.sessions) {
          setHistory(restoration.sessions);
          setHistoryError("");
        } else {
          setHistoryError("无法加载历史会话。");
        }
        if (savedSessionId && restoration.messages) {
          setSessionId(savedSessionId);
          setMessages(historyMessages(restoration.messages));
          void loadDocumentsForSession(savedSessionId);
        } else if (savedSessionId && restoration.rememberedFailed) {
          window.localStorage.removeItem(activeSessionStorageKey(initialStudentId));
        }
      })
      .finally(() => {
        if (active && generation === runGenerationRef.current) {
          setLoadingHistory(false);
          setRestoring(false);
        }
      });
    return () => {
      active = false;
    };
  }, [initialStudentId]);

  async function loadHistory(nextStudentId: string) {
    setLoadingHistory(true);
    try {
      const currentStudentId = nextStudentId.trim();
      if (!currentStudentId) {
        setHistory([]);
        return;
      }
      const response = await fetchSessions(currentStudentId);
      setHistory(response.sessions);
      setHistoryError("");
    } catch {
      setHistoryError("无法加载历史会话。");
    } finally {
      setLoadingHistory(false);
    }
  }

  async function loadDocumentsForSession(nextSessionId: string) {
    try {
      const response = await fetchSessionDocuments(nextSessionId);
      setDocuments(response.documents);
      setDocumentError("");
    } catch (error) {
      setDocuments([]);
      setDocumentError(error instanceof Error ? error.message : "无法读取会话资料。");
    }
  }

  async function submit(text = input, options: { action?: "synthesize" } = {}) {
    const trimmed = text.trim();
    if (!trimmed || runInFlightRef.current || restoring || loadingSession || transferBusy || documentBusy) return;
    const profile = currentProfile();
    if (!profile) return;

    runInFlightRef.current = true;
    setRunPending(true);
    const generation = runGenerationRef.current + 1;
    runGenerationRef.current = generation;
    finalizedRunsRef.current.clear();
    subscriptionRef.current?.close();
    subscriptionRef.current = null;
    setRunId(null);
    setImportedRunIds([]);
    setRunState({ ...freshRunState(), status: "queued" });
    setStopping(false);
    setFinalizing(false);
    setTransferError("");
    if (!options.action) {
      setInput("");
      setMessages((current) => [
        ...current,
        { id: `client-user-${generation}`, role: "user", content: trimmed }
      ]);
    }

    try {
      const created = await createRun({
        session_id: sessionId,
        user_id: profile.studentId,
        student_id: profile.studentId,
        student_name: profile.name,
        message: trimmed,
        action: options.action,
        enable_web_search: webSearchEnabled
      });
      if (!mountedRef.current || generation !== runGenerationRef.current) return;
      setRunId(created.run_id);
      setSessionId(created.session_id);
      window.localStorage.setItem(activeSessionStorageKey(profile.studentId), created.session_id);

      subscriptionRef.current = subscribeRunEvents(created.run_id, {
        onEvent: (event) => {
          if (!mountedRef.current || generation !== runGenerationRef.current) return;
          setRunState((current) => reduceRunEvent(current, event));
        },
        onError: (message) => {
          if (mountedRef.current && generation === runGenerationRef.current) setTransferError(message);
        },
        onTerminal: (event) => {
          void finalizeRun(created.run_id, created.session_id, profile.studentId, generation, event);
        }
      });
      void getRun(created.run_id).then((snapshot) => {
        if (!mountedRef.current || generation !== runGenerationRef.current) return;
        setRunState((current) => reduceRunSnapshot(current, snapshot));
      }).catch(() => {
        // SSE remains the lifecycle authority when an initial snapshot cannot be read.
      });
    } catch {
      if (!mountedRef.current || generation !== runGenerationRef.current) return;
      runInFlightRef.current = false;
      setRunPending(false);
      setRunState({
        ...freshRunState(),
        status: "failed",
        shouldReconnect: false,
        terminalMessage: "运行任务未创建，请检查 Rust 服务。"
      });
      setMessages((current) => [
        ...current,
        { id: `client-error-${generation}`, role: "assistant", content: "请求失败：无法创建运行任务。" }
      ]);
      requestAnimationFrame(() => composerRef.current?.focus());
    }
  }

  function synthesize() {
    void submit("请根据当前对话形成完整思路。", { action: "synthesize" });
  }

  async function finalizeRun(
    completedRunId: string,
    completedSessionId: string,
    profileStudentId: string,
    generation: number,
    terminalEvent?: RunClientEvent
  ) {
    if (!mountedRef.current || generation !== runGenerationRef.current) return;
    if (finalizedRunsRef.current.has(completedRunId)) return;
    finalizedRunsRef.current.add(completedRunId);
    setFinalizing(true);
    setSessionId(completedSessionId);
    subscriptionRef.current?.close();
    subscriptionRef.current = null;

    setMessages((current) => appendTerminalAssistant(current, completedRunId, terminalEvent));

    try {
      const [snapshotResult, messagesResult, sessionsResult] = await Promise.allSettled([
        getRun(completedRunId),
        fetchSessionMessages(completedSessionId),
        fetchSessions(profileStudentId)
      ]);
      if (!mountedRef.current || generation !== runGenerationRef.current) return;
      setSessionId(completedSessionId);
      if (snapshotResult.status === "fulfilled") {
        setRunState((current) => reduceRunSnapshot(current, snapshotResult.value));
      }
      if (messagesResult.status === "fulfilled") {
        setMessages(historyMessages(messagesResult.value.messages));
      } else {
        setTransferError("运行已结束，但最新历史加载失败。可稍后刷新历史。");
      }
      if (sessionsResult.status === "fulfilled") {
        setHistory(sessionsResult.value.sessions);
        setHistoryError("");
      }
    } finally {
      if (mountedRef.current && generation === runGenerationRef.current) {
        setSessionId(completedSessionId);
        runInFlightRef.current = false;
        setRunPending(false);
        setStopping(false);
        setFinalizing(false);
        requestAnimationFrame(() => composerRef.current?.focus());
      }
    }
  }

  async function stopRun() {
    if (!runId || stopping || !runActive) return;
    const stoppingRunId = runId;
    const generation = runGenerationRef.current;
    setStopping(true);
    try {
      const snapshot = await cancelRun(stoppingRunId, "student_stop");
      if (!mountedRef.current || generation !== runGenerationRef.current || stoppingRunId !== snapshot.run_id) return;
      setRunState((current) => reduceRunSnapshot(current, snapshot));
    } catch {
      if (mountedRef.current && generation === runGenerationRef.current) {
        setStopping(false);
        setTransferError("停止请求未成功，请重试。");
      }
    }
  }

  function submitForm(event: FormEvent) {
    event.preventDefault();
    void submit();
  }

  function handleComposerKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void submit();
    }
  }

  function resetRunUi() {
    runGenerationRef.current += 1;
    runInFlightRef.current = false;
    setRunPending(false);
    finalizedRunsRef.current.clear();
    subscriptionRef.current?.close();
    subscriptionRef.current = null;
    setRunId(null);
    setImportedRunIds([]);
    setRunState(freshRunState());
    setStopping(false);
    setFinalizing(false);
    setInspectingImportedRun(false);
  }

  async function inspectImportedRun(nextRunId: string) {
    const generation = runGenerationRef.current + 1;
    runGenerationRef.current = generation;
    subscriptionRef.current?.close();
    subscriptionRef.current = null;
    setInspectingImportedRun(true);
    setRunId(nextRunId);
    setRunState(freshRunState());
    setStopping(false);
    setTransferError("");
    try {
      const snapshot = await getRun(nextRunId);
      if (!mountedRef.current || generation !== runGenerationRef.current) return;
      if (!isTerminalRunStatus(snapshot.status)) {
        throw new Error("导入轨迹包含非终态运行，无法只读回放。");
      }
      setRunState(beginPersistedRunInspection(snapshot));
      subscriptionRef.current = subscribeRunEvents(nextRunId, {
        onEvent: (event) => {
          if (!mountedRef.current || generation !== runGenerationRef.current) return;
          setRunState((current) => reduceRunEvent(current, event));
        },
        onError: (message) => {
          if (mountedRef.current && generation === runGenerationRef.current) setTransferError(message);
        }
      });
    } catch (error) {
      if (mountedRef.current && generation === runGenerationRef.current) {
        setRunState(freshRunState());
        setTransferError(error instanceof Error ? error.message : "导入运行轨迹加载失败。");
      }
    } finally {
      if (mountedRef.current && generation === runGenerationRef.current) {
        setInspectingImportedRun(false);
      }
    }
  }

  function newChat() {
    if (busy) return;
    const profile = currentProfile(false);
    resetRunUi();
    setSessionId(null);
    if (profile) window.localStorage.removeItem(activeSessionStorageKey(profile.studentId));
    setMessages([]);
    setDocuments([]);
    setInput("");
    setWebSearchEnabled(false);
    setTransferMessage("");
    setTransferError("");
    requestAnimationFrame(() => composerRef.current?.focus());
  }

  async function openSession(nextSessionId: string) {
    if (busy || nextSessionId === sessionId) return;
    setLoadingSession(true);
    setTransferError("");
    try {
      const [response, documentResponse] = await Promise.all([
        fetchSessionMessages(nextSessionId),
        fetchSessionDocuments(nextSessionId)
      ]);
      resetRunUi();
      setSessionId(nextSessionId);
      setMessages(historyMessages(response.messages));
      setDocuments(documentResponse.documents);
      const profile = currentProfile(false);
      if (profile) window.localStorage.setItem(activeSessionStorageKey(profile.studentId), nextSessionId);
      setInput("");
    } catch {
      const profile = currentProfile(false);
      if (profile && nextSessionId === window.localStorage.getItem(activeSessionStorageKey(profile.studentId))) {
        window.localStorage.removeItem(activeSessionStorageKey(profile.studentId));
      }
      setTransferError("加载历史会话失败。");
    } finally {
      setLoadingSession(false);
      requestAnimationFrame(() => composerRef.current?.focus());
    }
  }

  async function handleExport() {
    if (!sessionId || busy) return;
    setTransferBusy(true);
    setTransferError("");
    setTransferMessage("");
    try {
      const exported = await exportSession(sessionId);
      downloadSessionExport(exported, sessionId);
      setTransferMessage("会话已导出。");
    } catch {
      setTransferError("会话导出失败。");
    } finally {
      setTransferBusy(false);
    }
  }

  async function handleImport(files: FileList | null) {
    if (!files || files.length !== 1 || busy) {
      if (files && files.length !== 1) setTransferError("每次只能导入一个 JSON 文件。");
      return;
    }
    const profile = currentProfile();
    if (!profile) return;
    setTransferBusy(true);
    setTransferError("");
    setTransferMessage("");
    try {
      const exportValue = await readSessionImportFile(files[0]);
      const imported = await importSession(exportValue);
      const [messageResponse, sessionsResponse, documentResponse] = await Promise.all([
        fetchSessionMessages(imported.session_id),
        fetchSessions(profile.studentId),
        fetchSessionDocuments(imported.session_id)
      ]);
      resetRunUi();
      setImportedRunIds(imported.run_ids);
      setSessionId(imported.session_id);
      setMessages(historyMessages(messageResponse.messages));
      setDocuments(documentResponse.documents);
      setDocumentError("");
      setHistory(sessionsResponse.sessions);
      window.localStorage.setItem(activeSessionStorageKey(profile.studentId), imported.session_id);
      setTransferMessage("会话已导入并打开。");
      const latestRunId = imported.run_ids.at(-1);
      if (latestRunId) await inspectImportedRun(latestRunId);
    } catch (error) {
      setTransferError(error instanceof Error ? error.message : "会话导入失败。");
    } finally {
      setTransferBusy(false);
      if (importRef.current) importRef.current.value = "";
    }
  }

  async function handleDocumentUpload(files: FileList | null) {
    if (!files || files.length !== 1 || busy) {
      if (files && files.length !== 1) setDocumentError("每次只能上传一个资料文件。");
      return;
    }
    if (!sessionId) {
      setDocumentError("请先开始一次对话，再向当前会话上传资料。");
      if (documentRef.current) documentRef.current.value = "";
      return;
    }
    setDocumentBusy(true);
    setDocumentError("");
    setDocumentMessage("");
    try {
      const uploaded = await uploadSessionDocument(sessionId, files[0]);
      await loadDocumentsForSession(sessionId);
      setDocumentMessage(`${uploaded.filename} 已上传并完成索引，共 ${uploaded.chunk_count} 个可检索片段。`);
    } catch (error) {
      setDocumentError(error instanceof Error ? error.message : "资料上传失败。");
    } finally {
      setDocumentBusy(false);
      if (documentRef.current) documentRef.current.value = "";
    }
  }

  async function removeDocument(documentId: string) {
    if (!sessionId || documentBusy) return;
    setDocumentBusy(true);
    setDocumentError("");
    try {
      await deleteSessionDocument(sessionId, documentId);
      await loadDocumentsForSession(sessionId);
      setDocumentMessage("会话资料已删除。");
    } catch (error) {
      setDocumentError(error instanceof Error ? error.message : "资料删除失败。");
    } finally {
      setDocumentBusy(false);
    }
  }

  async function confirmProfile(event?: FormEvent) {
    event?.preventDefault();
    const profile = normalizeStudentProfile(studentName, studentId);
    if (!profile) {
      setProfileError("请填写姓名和学号。");
      return;
    }
    try {
      await bootstrapStudentAccess(profile.name, profile.studentId);
      window.localStorage.setItem(studentProfileStorageKey, JSON.stringify(profile));
      setStudentName(profile.name);
      setStudentId(profile.studentId);
      setProfileConfirmed(true);
      setProfileError("");
      void loadHistory(profile.studentId);
      requestAnimationFrame(() => composerRef.current?.focus());
    } catch (error) {
      clearStudentAccess();
      setProfileError(error instanceof Error ? error.message : "无法确认学生身份。");
    }
  }

  function switchStudent() {
    if (busy) return;
    resetRunUi();
    clearStudentAccess();
    window.localStorage.removeItem(studentProfileStorageKey);
    setStudentName("");
    setStudentId("");
    setProfileConfirmed(false);
    setProfileError("");
    setSessionId(null);
    setHistory([]);
    setHistoryError("");
    setMessages([]);
    setDocuments([]);
    setDocumentMessage("");
    setDocumentError("");
    setInput("");
  }

  function currentProfile(showError = true): StudentProfile | null {
    const name = studentName.trim();
    const id = studentId.trim();
    if (!profileConfirmed || !name || !id) {
      if (showError) setProfileError("请先确认姓名和学号，再开始对话。");
      return null;
    }
    return { name, studentId: id };
  }

  function closeSettings() {
    setSettingsOpen(false);
    requestAnimationFrame(() => settingsButtonRef.current?.focus());
  }

  if (!profileConfirmed) {
    return (
      <StudentIdentityEntry
        name={studentName}
        studentId={studentId}
        error={profileError}
        onNameChange={setStudentName}
        onStudentIdChange={setStudentId}
        onSubmit={confirmProfile}
      />
    );
  }

  return (
    <div className="student-shell">
      <aside className="chat-sidebar" aria-hidden={settingsOpen || undefined} inert={settingsOpen || undefined}>
        <div className="brand-mark"><strong>WAM · 写作主体性导师</strong></div>
        <button className="new-chat-button" onClick={newChat} disabled={busy}>
          <span>+</span>新建对话
        </button>
        <section className="history-panel" aria-label="历史对话">
          <div className="history-heading"><span>历史对话</span></div>
          <div className="history-list">
            {history.map((item) => (
              <button
                key={item.session_id}
                className={item.session_id === sessionId ? "active" : ""}
                onClick={() => void openSession(item.session_id)}
                disabled={busy}
                title={item.preview || item.session_id}
              >
                <strong>{historyTitle(item)}</strong>
                <span>{formatHistoryTime(item.updated_at)}{item.message_count ? ` · ${item.message_count} 条` : ""}</span>
              </button>
            ))}
            {!history.length ? (
              <p className="history-empty">
                {historyError ? `加载失败：${historyError}` : loadingHistory ? "加载中..." : "暂无历史对话"}
              </p>
            ) : null}
          </div>
        </section>
        <section className="student-profile-summary" aria-label="当前学生">
          <span>当前学生</span><strong>{studentName}</strong><small>{studentId}</small>
          <button type="button" onClick={switchStudent} disabled={busy}>切换学生</button>
        </section>
      </aside>

      <main className={`student-main ${messages.length ? "has-messages" : "is-empty"}`} aria-hidden={settingsOpen || undefined} inert={settingsOpen || undefined}>
        <nav className="student-run-actions" aria-label="会话工具">
          <button ref={settingsButtonRef} type="button" onClick={() => setSettingsOpen(true)} aria-haspopup="dialog" aria-controls="model-settings-dialog" aria-expanded={settingsOpen}>模型设置</button>
          <button type="button" onClick={synthesize} disabled={!sessionId || !messages.length || busy}>形成思路</button>
          <button type="button" onClick={() => void handleExport()} disabled={!sessionId || busy}>导出会话</button>
          <label className={busy ? "disabled" : ""}>
            <span>导入 JSON</span>
            <input ref={importRef} type="file" accept="application/json,.json" disabled={busy} onChange={(event) => void handleImport(event.target.files)} />
          </label>
          <label className={!sessionId || busy ? "disabled" : ""}>
            <span>上传资料</span>
            <input ref={documentRef} type="file" accept=".txt,.md,text/plain,text/markdown" disabled={!sessionId || busy} onChange={(event) => void handleDocumentUpload(event.target.files)} />
          </label>
        </nav>
        {transferMessage ? <p className="transfer-message" role="status">{transferMessage}</p> : null}
        {transferError ? <p className="transfer-message error" role="alert">{transferError}</p> : null}
        {documentMessage ? <p className="transfer-message" role="status">{documentMessage}</p> : null}
        {documentError ? <p className="transfer-message error" role="alert">{documentError}</p> : null}
        {sessionId ? (
          <details className="session-documents">
            <summary>会话资料{documents.length ? `（${documents.length}）` : ""}</summary>
            <p>当前支持 TXT、Markdown，单个文件不超过 256 KiB。上传后会按标题和段落建立本地索引。</p>
            {documents.length ? <ul>{documents.map((document) => (
              <li key={document.document_id}>
                <span><strong>{document.filename}</strong><small>{document.index_status === "ready" ? `已索引 · ${document.chunk_count} 个片段` : "等待重新索引"}</small></span>
                <button type="button" disabled={documentBusy} onClick={() => void removeDocument(document.document_id)}>删除</button>
              </li>
            ))}</ul> : <p>当前会话还没有资料。</p>}
          </details>
        ) : null}

        {importedRunIds.length ? (
          <label className="imported-run-selector" aria-label="导入运行轨迹">
            <span>导入轨迹</span>
            <select
              value={runId || importedRunIds.at(-1)}
              disabled={busy}
              onChange={(event) => void inspectImportedRun(event.target.value)}
            >
              {importedRunIds.map((importedRunId, index) => (
                <option key={importedRunId} value={importedRunId}>运行 {index + 1}</option>
              ))}
            </select>
          </label>
        ) : null}

        {runState.status !== "idle" ? (
          <div className="run-inspector">
            <AgentProgress state={runState} stopping={stopping} />
            <UsageSummary state={runState} />
          </div>
        ) : null}

        {!messages.length && !runActive ? (
          <section className="empty-chat" aria-label="开始对话"><h1>有什么可以帮忙的？</h1></section>
        ) : null}

        {messages.length ? (
          <section className="chat-thread" aria-label="聊天记录">
            {messages.map((message) => (
              <article key={message.id} className={`chat-message ${message.role}`}>
                <div className="avatar" aria-hidden="true">{message.role === "assistant" ? "教" : "我"}</div>
                <div className="message-stack"><div className="bubble">
                  {message.role === "assistant" ? <ReactMarkdown remarkPlugins={[remarkGfm]}>{message.content}</ReactMarkdown> : message.content}
                </div>{message.role === "assistant" ? <MessageSources metadata={message.metadata_json} /> : null}</div>
              </article>
            ))}
            {runActive ? (
              <article className="chat-message assistant" aria-label="Agent 正在回复">
                <div className="avatar" aria-hidden="true">教</div>
                <div className="message-stack"><div className="bubble typing" role="status" aria-live="polite"><span /><span /><span /></div></div>
              </article>
            ) : null}
          </section>
        ) : null}

        <footer className="composer-wrap">
          <form className="chat-composer" onSubmit={submitForm}>
            <textarea
              ref={composerRef}
              value={input}
              onChange={(event) => setInput(event.target.value)}
              onKeyDown={handleComposerKey}
              placeholder="我是写作与沟通智能体，可以帮助你选题、检索资料、课程答疑以及训练评价。"
              aria-label="输入消息"
              disabled={!profileConfirmed}
              rows={1}
            />
            <button type="button" className={`web-search-toggle ${webSearchEnabled ? "active" : ""}`} onClick={() => setWebSearchEnabled((value) => !value)} disabled={busy} aria-pressed={webSearchEnabled} title={webSearchEnabled ? "关闭联网检索" : "开启联网检索"}>联网</button>
            {runActive ? (
              <button type="button" className="stop-run-button" onClick={() => void stopRun()} disabled={!runId || stopping} aria-label="停止当前运行">{stopping ? "…" : "■"}</button>
            ) : (
              <button disabled={busy || !input.trim() || !profileConfirmed} aria-label="发送">↑</button>
            )}
          </form>
        </footer>
      </main>
      <ModelSettings open={settingsOpen} onClose={closeSettings} />
    </div>
  );
}

type StudentIdentityEntryProps = {
  name: string;
  studentId: string;
  error: string;
  onNameChange: (value: string) => void;
  onStudentIdChange: (value: string) => void;
  onSubmit: (event: FormEvent) => void;
};

function StudentIdentityEntry({ name, studentId, error, onNameChange, onStudentIdChange, onSubmit }: StudentIdentityEntryProps) {
  return (
    <main className="student-access-page">
      <section className="dashboard-panel student-access-card">
        <div className="student-access-heading"><h1>学生端访问</h1></div>
        <form className="student-access-form" onSubmit={onSubmit}>
          <label><span>姓名</span><input value={name} onChange={(event) => onNameChange(event.target.value)} placeholder="请输入姓名" autoComplete="name" autoFocus /></label>
          <label><span>学号</span><input value={studentId} onChange={(event) => onStudentIdChange(event.target.value)} placeholder="请输入学号" autoComplete="username" /></label>
          <button type="submit">进入学生端</button>
          {error ? <p className="error-text" role="alert">{error}</p> : null}
        </form>
      </section>
    </main>
  );
}

function historyMessages(messages: HistoryMessage[]): Message[] {
  return messages
    .filter((message) =>
      (message.role === "user" || message.role === "assistant")
      && !(message.role === "user" && message.metadata_json.action === "synthesize")
    )
    .map((message) => ({ id: message.id, role: message.role, content: message.content, metadata_json: message.metadata_json }));
}

function MessageSources({ metadata }: { metadata?: Record<string, unknown> }) {
  const sources = Array.isArray(metadata?.grounding_sources)
    ? metadata.grounding_sources.filter((source): source is Record<string, unknown> => (
      Boolean(source) && typeof source === "object" && (source as Record<string, unknown>).provider === "session_document"
    ))
    : [];
  if (!sources.length) return null;
  return (
    <details className="message-sources">
      <summary>本轮参考了 {sources.length} 个会话资料片段</summary>
      <ul>{sources.map((source, index) => (
        <li key={`${String(source.source)}-${index}`}><strong>{String(source.source || "会话资料")}</strong>{source.heading ? ` · ${String(source.heading)}` : ""}</li>
      ))}</ul>
    </details>
  );
}

function freshRunState(): RunClientState {
  return {
    ...initialRunState,
    usage: { ...initialRunState.usage, calls: [] },
    budgets: { ...initialRunState.budgets },
    events: []
  };
}

function historyTitle(item: SessionSummary) {
  const title = (item.preview || "新对话").replace(/\s+/g, " ").trim();
  return title.length > 18 ? `${title.slice(0, 18)}...` : title;
}

function formatHistoryTime(value: string | null) {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleString("zh-CN", { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" });
}

function activeSessionStorageKey(studentId: string) {
  return `writingCoach.activeSessionId.${studentId}`;
}

function readSavedProfile(): StudentProfile | null {
  return parseStudentProfile(window.localStorage.getItem(studentProfileStorageKey));
}
