use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
};

use futures_util::FutureExt;
use serde_json::json;
use sqlx::SqlitePool;
use tokio::{
    sync::{Mutex as AsyncMutex, broadcast},
    task::JoinHandle,
    time::{Duration, sleep},
};
use tokio_util::sync::CancellationToken;

use crate::{
    AppError,
    agent::{AgentProgram, RunContext, UserTurn, run_context::RunContextControl},
    domain::{AgentRun, RunEvent, RunId, RunStatus, SessionId},
    llm::{ModelError, ModelGateway, ModelSettingsStore},
    store::runs::RunRepository,
};

const EVENT_CHANNEL_CAPACITY: usize = 256;
const RESTART_FAILURE_MESSAGE: &str = "interrupted by service restart";
const TERMINAL_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(10);
const TERMINAL_RETRY_MAX_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct RunEngine {
    inner: Arc<RunEngineInner>,
}

struct RunEngineInner {
    repository: RunRepository,
    program: Arc<dyn AgentProgram>,
    gateway: Arc<dyn ModelGateway>,
    settings: Arc<ModelSettingsStore>,
    live: Mutex<HashMap<RunId, LiveRun>>,
    #[cfg(test)]
    response_accounting_test_gate:
        Mutex<Option<Arc<crate::agent::run_context::ResponseAccountingTestGate>>>,
}

struct LiveRun {
    cancellation: CancellationToken,
    cancellation_reason: Arc<Mutex<Option<String>>>,
    lifecycle: Arc<AsyncMutex<()>>,
    events: broadcast::Sender<RunEvent>,
    _task: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct LiveControl {
    cancellation: CancellationToken,
    cancellation_reason: Arc<Mutex<Option<String>>>,
    lifecycle: Arc<AsyncMutex<()>>,
    events: broadcast::Sender<RunEvent>,
}

struct ExecutionControl {
    cancellation: CancellationToken,
    cancellation_reason: Arc<Mutex<Option<String>>>,
    lifecycle: Arc<AsyncMutex<()>>,
    events: broadcast::Sender<RunEvent>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RunHandle {
    pub run_id: RunId,
    pub session_id: SessionId,
}

#[derive(Clone, Debug, Default)]
pub struct SessionPreparation {
    pub create_session: bool,
    pub user_id: Option<String>,
    pub student_name: Option<String>,
}

pub struct RunSubscription {
    pub replay: Vec<RunEvent>,
    receiver: broadcast::Receiver<RunEvent>,
}

impl RunSubscription {
    pub async fn recv(&mut self) -> Result<RunEvent, broadcast::error::RecvError> {
        self.receiver.recv().await
    }
}

impl RunEngine {
    pub fn new(
        pool: SqlitePool,
        program: Arc<dyn AgentProgram>,
        gateway: Arc<dyn ModelGateway>,
        settings: Arc<ModelSettingsStore>,
    ) -> Self {
        Self {
            inner: Arc::new(RunEngineInner {
                repository: RunRepository::new(pool),
                program,
                gateway,
                settings,
                live: Mutex::new(HashMap::new()),
                #[cfg(test)]
                response_accounting_test_gate: Mutex::new(None),
            }),
        }
    }

    pub async fn start(&self, turn: UserTurn) -> Result<RunHandle, AppError> {
        self.start_prepared(turn, SessionPreparation::default())
            .await
    }

    pub async fn start_prepared(
        &self,
        turn: UserTurn,
        preparation: SessionPreparation,
    ) -> Result<RunHandle, AppError> {
        if turn.content.trim().is_empty() {
            return Err(AppError::InvalidRun(
                "user turn content must not be empty".to_owned(),
            ));
        }
        let settings = self
            .inner
            .settings
            .lease_for_run()
            .map_err(ModelError::from)?;
        let token_budget = turn.token_budget.or(Some(settings.default_token_budget));
        let cost_budget_microusd = turn
            .cost_budget_microusd
            .or(Some(settings.default_cost_budget_microusd));
        let run = self
            .inner
            .repository
            .create_prepared(
                turn.session_id,
                preparation.create_session,
                preparation.user_id.as_deref(),
                preparation.student_name.as_deref(),
                turn.max_steps,
                token_budget,
                cost_budget_microusd,
            )
            .await?;
        let cancellation = CancellationToken::new();
        let cancellation_reason = Arc::new(Mutex::new(None));
        let lifecycle = Arc::new(AsyncMutex::new(()));
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        self.inner.live.lock().unwrap().insert(
            run.id,
            LiveRun {
                cancellation: cancellation.clone(),
                cancellation_reason: cancellation_reason.clone(),
                lifecycle: lifecycle.clone(),
                events: events.clone(),
                _task: None,
            },
        );
        let engine = self.clone();
        let run_id = run.id;
        let task = tokio::spawn(async move {
            engine
                .execute_run(
                    run_id,
                    turn,
                    ExecutionControl {
                        cancellation,
                        cancellation_reason,
                        lifecycle,
                        events,
                    },
                    settings.call,
                )
                .await;
        });
        if let Some(live) = self.inner.live.lock().unwrap().get_mut(&run_id) {
            live._task = Some(task);
        }
        Ok(RunHandle {
            run_id,
            session_id: run.session_id,
        })
    }

    pub async fn get(&self, run_id: RunId) -> Result<AgentRun, AppError> {
        self.inner.repository.get(run_id).await
    }

    pub async fn reconcile_orphans(&self) -> Result<Vec<RunEvent>, AppError> {
        self.inner
            .repository
            .reconcile_orphans(RESTART_FAILURE_MESSAGE)
            .await
    }

    pub async fn events(&self, run_id: RunId, after_seq: u64) -> Result<Vec<RunEvent>, AppError> {
        self.inner.repository.get(run_id).await?;
        self.inner.repository.list_events(run_id, after_seq).await
    }

    pub async fn cancel(&self, run_id: RunId, reason: &str) -> Result<(), AppError> {
        let reason = normalized_cancel_reason(reason);
        let live = self
            .inner
            .live
            .lock()
            .unwrap()
            .get(&run_id)
            .map(|live| LiveControl {
                cancellation: live.cancellation.clone(),
                cancellation_reason: live.cancellation_reason.clone(),
                lifecycle: live.lifecycle.clone(),
                events: live.events.clone(),
            });
        if let Some(live) = &live {
            *live.cancellation_reason.lock().unwrap() = Some(reason.clone());
            live.cancellation.cancel();
        }
        let _lifecycle = match &live {
            Some(live) => Some(live.lifecycle.lock().await),
            None => None,
        };
        let event = if live.is_some() {
            self.finish_with_retry(
                run_id,
                RunStatus::Cancelled,
                Some(reason.clone()),
                json!({"reason": reason.clone()}),
            )
            .await
        } else {
            self.inner
                .repository
                .finish(
                    run_id,
                    RunStatus::Cancelled,
                    Some(&reason),
                    json!({"reason": reason}),
                )
                .await?
        };
        if let (Some(live), Some(event)) = (&live, event) {
            let _ = live.events.send(event);
        }
        Ok(())
    }

    pub async fn subscribe(
        &self,
        run_id: RunId,
        after_seq: u64,
    ) -> Result<RunSubscription, AppError> {
        let initial = self.inner.repository.get(run_id).await?;
        if initial.status.is_terminal() {
            return Ok(RunSubscription {
                replay: self.inner.repository.list_events(run_id, after_seq).await?,
                receiver: closed_event_receiver(),
            });
        }
        let receiver = {
            let mut live = self.inner.live.lock().unwrap();
            live.entry(run_id)
                .or_insert_with(|| {
                    let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
                    LiveRun {
                        cancellation: CancellationToken::new(),
                        cancellation_reason: Arc::new(Mutex::new(None)),
                        lifecycle: Arc::new(AsyncMutex::new(())),
                        events,
                        _task: None,
                    }
                })
                .events
                .subscribe()
        };
        let mut replay = self.inner.repository.list_events(run_id, after_seq).await?;
        let after_replay = self.inner.repository.get(run_id).await?;
        if after_replay.status.is_terminal() {
            let high_water = replay.last().map_or(after_seq, |event| event.seq);
            replay.extend(
                self.inner
                    .repository
                    .list_events(run_id, high_water)
                    .await?,
            );
            self.inner.live.lock().unwrap().remove(&run_id);
        }
        Ok(RunSubscription { replay, receiver })
    }

    async fn execute_run(
        &self,
        run_id: RunId,
        turn: UserTurn,
        control: ExecutionControl,
        settings: crate::llm::ModelCallSettings,
    ) {
        let ExecutionControl {
            cancellation,
            cancellation_reason,
            lifecycle,
            events,
        } = control;
        if cancellation.is_cancelled() {
            self.finish_cancelled(run_id, &cancellation_reason, &events)
                .await;
            self.inner.live.lock().unwrap().remove(&run_id);
            return;
        }
        let started = self.inner.repository.mark_running(run_id).await;
        match started {
            Ok(Some(event)) => {
                let _ = events.send(event);
            }
            Ok(None) => {
                self.inner.live.lock().unwrap().remove(&run_id);
                return;
            }
            Err(_) => {
                let message = "agent execution could not start";
                if let Some(event) = self
                    .finish_with_retry(
                        run_id,
                        RunStatus::Failed,
                        Some(message.to_owned()),
                        json!({"message": message}),
                    )
                    .await
                {
                    let _ = events.send(event);
                }
                self.inner.live.lock().unwrap().remove(&run_id);
                return;
            }
        }

        let context = RunContext::new(
            self.inner.repository.clone(),
            self.inner.gateway.clone(),
            RunContextControl::new(cancellation.clone(), events.clone(), lifecycle),
            run_id,
            settings,
            turn.max_steps,
        );
        #[cfg(test)]
        let context = match self
            .inner
            .response_accounting_test_gate
            .lock()
            .unwrap()
            .clone()
        {
            Some(gate) => context.with_response_accounting_test_gate(gate),
            None => context,
        };
        let result = AssertUnwindSafe(self.inner.program.execute(context, turn))
            .catch_unwind()
            .await;
        let terminal = match result {
            Err(_) => {
                let message = "agent program panicked";
                self.finish_with_retry(
                    run_id,
                    RunStatus::Failed,
                    Some(message.to_owned()),
                    json!({"message": message}),
                )
                .await
            }
            Ok(Ok(_)) if cancellation.is_cancelled() => {
                self.finish_cancelled_result(run_id, &cancellation_reason)
                    .await
            }
            Ok(Ok(answer)) => {
                self.finish_with_retry(
                    run_id,
                    RunStatus::Completed,
                    None,
                    json!({"answer": answer.content, "metadata": answer.metadata}),
                )
                .await
            }
            Ok(Err(AppError::RunCancelled)) => {
                self.finish_cancelled_result(run_id, &cancellation_reason)
                    .await
            }
            Ok(Err(AppError::RunTerminal)) => None,
            Ok(Err(AppError::RunBudgetExceeded(reason))) => {
                self.finish_with_retry(
                    run_id,
                    RunStatus::BudgetExceeded,
                    Some(reason.clone()),
                    json!({"reason": reason}),
                )
                .await
            }
            Ok(Err(AppError::RunMaxSteps)) => {
                let message = "run reached its maximum step count";
                self.finish_with_retry(
                    run_id,
                    RunStatus::Failed,
                    Some(message.to_owned()),
                    json!({"message": message}),
                )
                .await
            }
            Ok(Err(error)) => {
                let message = safe_failure_message(&error);
                if let Ok(run) = self.inner.repository.get(run_id).await
                    && let Some(step) = run.current_step
                    && let Ok(event) = self
                        .inner
                        .repository
                        .append_event(
                            run_id,
                            "step.failed",
                            json!({"step": step, "message": message}),
                            None,
                        )
                        .await
                {
                    let _ = events.send(event);
                }
                self.finish_with_retry(
                    run_id,
                    RunStatus::Failed,
                    Some(message.to_owned()),
                    json!({"message": message}),
                )
                .await
            }
        };
        if let Some(event) = terminal {
            let _ = events.send(event);
        }
        self.inner.live.lock().unwrap().remove(&run_id);
    }

    async fn finish_cancelled(
        &self,
        run_id: RunId,
        reason: &Mutex<Option<String>>,
        events: &broadcast::Sender<RunEvent>,
    ) {
        if let Some(event) = self.finish_cancelled_result(run_id, reason).await {
            let _ = events.send(event);
        }
    }

    async fn finish_cancelled_result(
        &self,
        run_id: RunId,
        reason: &Mutex<Option<String>>,
    ) -> Option<RunEvent> {
        let reason = reason
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "user_requested".to_owned());
        self.finish_with_retry(
            run_id,
            RunStatus::Cancelled,
            Some(reason.clone()),
            json!({"reason": reason}),
        )
        .await
    }

    async fn finish_with_retry(
        &self,
        run_id: RunId,
        status: RunStatus,
        reason: Option<String>,
        payload: serde_json::Value,
    ) -> Option<RunEvent> {
        let mut delay = TERMINAL_RETRY_INITIAL_DELAY;
        let mut attempt = 0_u64;
        loop {
            attempt = attempt.saturating_add(1);
            match self
                .inner
                .repository
                .finish(run_id, status, reason.as_deref(), payload.clone())
                .await
            {
                Ok(event) => return event,
                Err(error) => {
                    tracing::warn!(
                        run_id = %run_id.to_legacy_hex(),
                        terminal_status = status.as_str(),
                        attempt,
                        error_kind = terminal_error_kind(&error),
                        "terminal persistence failed; retrying"
                    );
                    sleep(delay).await;
                    delay = delay.saturating_mul(2).min(TERMINAL_RETRY_MAX_DELAY);
                }
            }
        }
    }
}

fn closed_event_receiver() -> broadcast::Receiver<RunEvent> {
    let (sender, receiver) = broadcast::channel(1);
    drop(sender);
    receiver
}

fn normalized_cancel_reason(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        "user_requested".to_owned()
    } else {
        reason.chars().take(256).collect()
    }
}

fn safe_failure_message(error: &AppError) -> String {
    match error {
        AppError::Model(_) => "model provider request failed".to_owned(),
        AppError::Pricing(_) => "model usage cost could not be calculated".to_owned(),
        AppError::ContextCapacityExceeded {
            estimated_tokens,
            context_limit,
        } => format!(
            "输入内容过长：本轮预计需要约 {estimated_tokens} tokens，超过模型上下文上限 {context_limit} tokens。请缩短本次输入或删除部分会话资料后重试。"
        ),
        _ => "agent execution failed".to_owned(),
    }
}

fn terminal_error_kind(error: &AppError) -> &'static str {
    match error {
        AppError::Sqlx(_) => "database",
        AppError::CorruptData(_) => "corrupt_data",
        AppError::NotFound(_) => "not_found",
        AppError::InvalidRun(_) => "invalid_run",
        AppError::ContextCapacityExceeded { .. } => "input_too_long",
        _ => "internal",
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use async_trait::async_trait;
    use sqlx::{Row, sqlite::SqlitePoolOptions};
    use tokio::sync::Notify;

    use super::RunEngine;
    use crate::{
        AppError,
        agent::{AgentAnswer, AgentProgram, RunContext, UserTurn},
        config::ModelConfig,
        domain::{RunStatus, Usage},
        llm::{
            ModelCallSettings, ModelError, ModelGateway, ModelRequest, ModelResponse,
            ModelSettingsStore,
        },
        store::{sessions::SessionRepository, sqlite},
    };

    struct ImmediateProgram;

    #[async_trait]
    impl AgentProgram for ImmediateProgram {
        async fn execute(
            &self,
            _context: RunContext,
            _turn: UserTurn,
        ) -> Result<AgentAnswer, AppError> {
            Ok(AgentAnswer::new("finished"))
        }
    }

    struct UnusedGateway;

    #[async_trait]
    impl ModelGateway for UnusedGateway {
        async fn complete(
            &self,
            _request: ModelRequest,
            _settings: ModelCallSettings,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> Result<ModelResponse, ModelError> {
            Ok(ModelResponse {
                content: "unused".to_owned(),
                reasoning: None,
                provider: "unused".to_owned(),
                model: "unused".to_owned(),
                usage: Usage {
                    input_tokens: 13,
                    output_tokens: 8,
                },
                stop_reason: None,
                response_id: None,
                latency_ms: 0,
            })
        }
    }

    struct CallingProgram;

    #[async_trait]
    impl AgentProgram for CallingProgram {
        async fn execute(
            &self,
            context: RunContext,
            turn: UserTurn,
        ) -> Result<AgentAnswer, AppError> {
            let response = context
                .call_model("answer", ModelRequest::from_user(turn.content))
                .await?;
            Ok(AgentAnswer::new(response.content))
        }
    }

    struct BlockingProgram {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    #[async_trait]
    impl AgentProgram for BlockingProgram {
        async fn execute(
            &self,
            _context: RunContext,
            _turn: UserTurn,
        ) -> Result<AgentAnswer, AppError> {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(AgentAnswer::new("released"))
        }
    }

    #[tokio::test]
    async fn transient_terminal_persistence_failure_is_retried_without_stranding_the_run() {
        // Break caught: execute_run discards Err from the terminal transaction and removes the
        // only live owner, leaving a running row that wedges the Session until process restart.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlite::migrate(&pool).await.unwrap();
        let session = SessionRepository::new(pool.clone())
            .create(None)
            .await
            .unwrap();
        let settings = Arc::new(
            ModelSettingsStore::new(ModelConfig {
                provider: "openai".to_owned(),
                endpoint: "http://127.0.0.1:1/v1".to_owned(),
                name: "fixture".to_owned(),
                api_key_env: "WRITING_COACH_UNUSED_TEST_KEY".to_owned(),
                context_length: 8_192,
                max_output_tokens: 512,
                reasoning_mode: "medium".to_owned(),
                input_price_microusd_per_million: 1,
                output_price_microusd_per_million: 1,
            })
            .unwrap(),
        );
        let engine = RunEngine::new(
            pool,
            Arc::new(ImmediateProgram),
            Arc::new(UnusedGateway),
            settings,
        );
        engine.inner.repository.inject_terminal_failures_for_test(1);

        let handle = engine
            .start(UserTurn::new(
                session.id,
                "finish despite one transient write error",
            ))
            .await
            .unwrap();
        let run = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let run = engine.get(handle.run_id).await.unwrap();
                if run.status.is_terminal() {
                    return run;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("terminal persistence was retried");
        let events = engine.events(handle.run_id, 0).await.unwrap();

        assert_eq!(run.status, RunStatus::Completed);
        assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
        assert_eq!(events.last().unwrap().kind, "run.completed");
    }

    #[tokio::test]
    async fn live_cancel_retries_a_transient_terminal_persistence_failure() {
        // Break caught: the HTTP cancellation path performs only one terminal write, so a
        // transient SQLite error returns 500 while the live Run remains nonterminal.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlite::migrate(&pool).await.unwrap();
        let session = SessionRepository::new(pool.clone())
            .create(None)
            .await
            .unwrap();
        let settings = Arc::new(
            ModelSettingsStore::new(ModelConfig {
                provider: "openai".to_owned(),
                endpoint: "http://127.0.0.1:1/v1".to_owned(),
                name: "fixture".to_owned(),
                api_key_env: "WRITING_COACH_UNUSED_TEST_KEY".to_owned(),
                context_length: 8_192,
                max_output_tokens: 512,
                reasoning_mode: "medium".to_owned(),
                input_price_microusd_per_million: 1,
                output_price_microusd_per_million: 1,
            })
            .unwrap(),
        );
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let engine = RunEngine::new(
            pool,
            Arc::new(BlockingProgram {
                entered: entered.clone(),
                release: release.clone(),
            }),
            Arc::new(UnusedGateway),
            settings,
        );
        let handle = engine
            .start(UserTurn::new(session.id, "cancel this live run"))
            .await
            .unwrap();
        entered.notified().await;
        engine.inner.repository.inject_terminal_failures_for_test(1);

        engine.cancel(handle.run_id, "student_stop").await.unwrap();

        let run = engine.get(handle.run_id).await.unwrap();
        let events = engine.events(handle.run_id, 0).await.unwrap();
        assert_eq!(run.status, RunStatus::Cancelled);
        assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
        assert_eq!(events.last().unwrap().kind, "run.cancelled");
        release.notify_one();
    }

    #[tokio::test]
    async fn returned_response_usage_commits_before_concurrent_cancellation_terminalizes() {
        // Break caught: checking cancellation after a measured gateway response lets cancel claim
        // the terminal row first, so record_model_call rejects the real usage as inactive.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlite::migrate(&pool).await.unwrap();
        let session = SessionRepository::new(pool.clone())
            .create(None)
            .await
            .unwrap();
        let settings = Arc::new(
            ModelSettingsStore::new(ModelConfig {
                provider: "openai".to_owned(),
                endpoint: "http://127.0.0.1:1/v1".to_owned(),
                name: "fixture".to_owned(),
                api_key_env: "WRITING_COACH_UNUSED_TEST_KEY".to_owned(),
                context_length: 8_192,
                max_output_tokens: 512,
                reasoning_mode: "medium".to_owned(),
                input_price_microusd_per_million: 1,
                output_price_microusd_per_million: 1,
            })
            .unwrap(),
        );
        let engine = RunEngine::new(
            pool.clone(),
            Arc::new(CallingProgram),
            Arc::new(UnusedGateway),
            settings,
        );
        let gate = Arc::new(crate::agent::run_context::ResponseAccountingTestGate::default());
        *engine.inner.response_accounting_test_gate.lock().unwrap() = Some(gate.clone());

        let handle = engine
            .start(UserTurn::new(session.id, "account this response"))
            .await
            .unwrap();
        gate.response_returned.notified().await;
        let cancellation = engine
            .inner
            .live
            .lock()
            .unwrap()
            .get(&handle.run_id)
            .unwrap()
            .cancellation
            .clone();
        let cancel = {
            let engine = engine.clone();
            tokio::spawn(async move { engine.cancel(handle.run_id, "response_race").await })
        };
        cancellation.cancelled().await;
        gate.release_accounting.notify_one();
        cancel.await.unwrap().unwrap();

        let run = engine.get(handle.run_id).await.unwrap();
        let calls =
            sqlx::query("SELECT input_tokens, output_tokens FROM model_calls WHERE run_id = ?")
                .bind(handle.run_id.to_legacy_hex())
                .fetch_all(&pool)
                .await
                .unwrap();
        let events = engine.events(handle.run_id, 0).await.unwrap();

        assert_eq!(run.status, RunStatus::Cancelled);
        assert_eq!((run.input_tokens, run.output_tokens), (13, 8));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].get::<i64, _>("input_tokens"), 13);
        assert_eq!(calls[0].get::<i64, _>("output_tokens"), 8);
        let usage = events
            .iter()
            .position(|event| event.kind == "model.usage")
            .expect("returned usage is persisted");
        let terminal = events
            .iter()
            .position(|event| event.kind == "run.cancelled")
            .expect("cancellation terminal is persisted");
        assert!(usage < terminal);
    }
}
