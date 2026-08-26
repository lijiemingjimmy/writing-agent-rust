use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use serde_json::json;
use sqlx::{Row, SqlitePool, sqlite::SqlitePoolOptions};
use tokio::sync::{Barrier, Notify};
use tokio_util::sync::CancellationToken;
use writing_coach_server::{
    AppError,
    agent::{AgentAnswer, AgentProgram, RunContext, RunEngine, UserTurn},
    config::{ModelConfig, RunDefaults},
    domain::{RunId, RunStatus, Usage},
    llm::{
        ModelCallSettings, ModelError, ModelGateway, ModelRequest, ModelResponse,
        ModelSettingsStore,
    },
    store::{runs::RunRepository, sessions::SessionRepository, sqlite},
};

const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct FakeGateway {
    response: Arc<Mutex<Result<ModelResponse, ModelError>>>,
    calls: Arc<AtomicUsize>,
    entered: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
}

#[derive(Clone)]
struct MutableSettingsGateway;

#[async_trait]
impl ModelGateway for MutableSettingsGateway {
    async fn complete(
        &self,
        _request: ModelRequest,
        settings: ModelCallSettings,
        _cancellation: CancellationToken,
    ) -> Result<ModelResponse, ModelError> {
        Ok(ModelResponse {
            content: "leased answer".to_owned(),
            reasoning: None,
            provider: settings.provider,
            model: settings.name,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
            stop_reason: Some("stop".to_owned()),
            response_id: Some("leased-response".to_owned()),
            latency_ms: 1,
        })
    }
}

impl FakeGateway {
    fn success(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            response: Arc::new(Mutex::new(Ok(ModelResponse {
                content: "model answer".to_owned(),
                reasoning: None,
                provider: "fake".to_owned(),
                model: "fixture".to_owned(),
                usage: Usage {
                    input_tokens,
                    output_tokens,
                },
                stop_reason: Some("stop".to_owned()),
                response_id: Some("response-1".to_owned()),
                latency_ms: 7,
            }))),
            calls: Arc::new(AtomicUsize::new(0)),
            entered: None,
            release: None,
        }
    }

    fn provider_failure() -> Self {
        Self {
            response: Arc::new(Mutex::new(Err(ModelError::Provider))),
            calls: Arc::new(AtomicUsize::new(0)),
            entered: None,
            release: None,
        }
    }

    fn gated(
        input_tokens: u64,
        output_tokens: u64,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    ) -> Self {
        let mut gateway = Self::success(input_tokens, output_tokens);
        gateway.entered = Some(entered);
        gateway.release = Some(release);
        gateway
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ModelGateway for FakeGateway {
    async fn complete(
        &self,
        _request: ModelRequest,
        _settings: ModelCallSettings,
        cancellation: CancellationToken,
    ) -> Result<ModelResponse, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = &self.entered {
            entered.notify_one();
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ModelError::Cancelled),
                _ = self.release.as_ref().unwrap().notified() => {}
            }
        }
        self.response.lock().unwrap().clone()
    }
}

#[derive(Clone)]
struct CallingProgram;

#[async_trait]
impl AgentProgram for CallingProgram {
    async fn execute(&self, context: RunContext, turn: UserTurn) -> Result<AgentAnswer, AppError> {
        context
            .emit("step.started", json!({"step": "answer"}))
            .await?;
        let response = context
            .call_model("answer", ModelRequest::from_user(turn.content))
            .await?;
        context
            .emit("step.completed", json!({"step": "answer"}))
            .await?;
        Ok(AgentAnswer::new(response.content))
    }
}

#[derive(Clone)]
struct BlockingBeforeModelProgram {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    finished: Arc<Notify>,
}

#[async_trait]
impl AgentProgram for BlockingBeforeModelProgram {
    async fn execute(&self, context: RunContext, turn: UserTurn) -> Result<AgentAnswer, AppError> {
        context
            .emit("step.started", json!({"step": "answer"}))
            .await?;
        self.entered.notify_one();
        self.release.notified().await;
        let result = context
            .call_model("answer", ModelRequest::from_user(turn.content))
            .await
            .map(|response| AgentAnswer::new(response.content));
        self.finished.notify_one();
        result
    }
}

#[derive(Clone)]
struct TooManyStepsProgram;

#[async_trait]
impl AgentProgram for TooManyStepsProgram {
    async fn execute(&self, context: RunContext, _turn: UserTurn) -> Result<AgentAnswer, AppError> {
        context.emit("step.started", json!({"step": "one"})).await?;
        context.emit("step.started", json!({"step": "two"})).await?;
        Ok(AgentAnswer::new("unreachable"))
    }
}

#[derive(Clone)]
struct CompletionGateProgram {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Clone)]
struct PanicGateProgram {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Clone)]
struct ConcurrentStepsProgram;

#[async_trait]
impl AgentProgram for ConcurrentStepsProgram {
    async fn execute(&self, context: RunContext, _turn: UserTurn) -> Result<AgentAnswer, AppError> {
        let barrier = Arc::new(Barrier::new(2));
        let first = {
            let context = context.clone();
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                context.emit("step.started", json!({"step": "first"})).await
            }
        };
        let second = {
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                context
                    .emit("step.started", json!({"step": "second"}))
                    .await
            }
        };
        let (first, second) = tokio::join!(first, second);
        first?;
        second?;
        Ok(AgentAnswer::new("both steps admitted"))
    }
}

#[derive(Clone)]
struct ConcurrentModelCallsProgram;

#[async_trait]
impl AgentProgram for ConcurrentModelCallsProgram {
    async fn execute(&self, context: RunContext, _turn: UserTurn) -> Result<AgentAnswer, AppError> {
        context
            .emit("step.started", json!({"step": "concurrent calls"}))
            .await?;
        let barrier = Arc::new(Barrier::new(2));
        let first = {
            let context = context.clone();
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                context
                    .call_model("first", ModelRequest::from_user("first"))
                    .await
            }
        };
        let second = {
            let barrier = barrier.clone();
            async move {
                barrier.wait().await;
                context
                    .call_model("second", ModelRequest::from_user("second"))
                    .await
            }
        };
        let (first, second) = tokio::join!(first, second);
        match (first, second) {
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Ok(AgentAnswer::new("both calls admitted")),
        }
    }
}

#[async_trait]
impl AgentProgram for CompletionGateProgram {
    async fn execute(&self, context: RunContext, _turn: UserTurn) -> Result<AgentAnswer, AppError> {
        context
            .emit("step.started", json!({"step": "final"}))
            .await?;
        self.entered.notify_one();
        self.release.notified().await;
        Ok(AgentAnswer::new("finished"))
    }
}

#[async_trait]
impl AgentProgram for PanicGateProgram {
    async fn execute(
        &self,
        _context: RunContext,
        _turn: UserTurn,
    ) -> Result<AgentAnswer, AppError> {
        self.entered.notify_one();
        self.release.notified().await;
        panic!("intentional test panic");
    }
}

struct Harness {
    pool: SqlitePool,
    engine: RunEngine,
    gateway: FakeGateway,
    session_id: writing_coach_server::domain::SessionId,
    settings: Arc<ModelSettingsStore>,
}

struct TempSqliteFile {
    path: PathBuf,
}

impl TempSqliteFile {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "writing-coach-run-engine-{}.sqlite",
                RunId::new().to_legacy_hex()
            )),
        }
    }

    fn url(&self) -> String {
        format!("sqlite://{}", self.path.display())
    }
}

impl Drop for TempSqliteFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(format!("{}-shm", self.path.display()));
        let _ = std::fs::remove_file(format!("{}-wal", self.path.display()));
    }
}

impl Harness {
    async fn new(program: Arc<dyn AgentProgram>, gateway: FakeGateway) -> Self {
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
            ModelSettingsStore::new_with_run_defaults(model_config(), run_defaults()).unwrap(),
        );
        let engine = RunEngine::new(
            pool.clone(),
            program,
            Arc::new(gateway.clone()),
            settings.clone(),
        );
        Self {
            pool,
            engine,
            gateway,
            session_id: session.id,
            settings,
        }
    }

    fn turn(&self) -> UserTurn {
        UserTurn::new(self.session_id, "help me write").with_limits(8, None, None)
    }

    async fn wait_terminal(
        &self,
        run_id: writing_coach_server::domain::RunId,
    ) -> writing_coach_server::domain::AgentRun {
        let mut subscription = self.engine.subscribe(run_id, 0).await.unwrap();
        tokio::time::timeout(WAIT, async {
            loop {
                let run = self.engine.get(run_id).await.unwrap();
                if run.status.is_terminal() {
                    return run;
                }
                subscription.recv().await.unwrap();
            }
        })
        .await
        .expect("run reached a terminal state")
    }
}

#[tokio::test]
async fn cancellation_prevents_the_next_model_call() {
    // Mutation caught: checking cancellation only after starting the provider call.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let finished = Arc::new(Notify::new());
    let program = BlockingBeforeModelProgram {
        entered: entered.clone(),
        release: release.clone(),
        finished: finished.clone(),
    };
    let harness = Harness::new(Arc::new(program), FakeGateway::success(10, 5)).await;

    let handle = harness.engine.start(harness.turn()).await.unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("program reached the explicit pre-model gate");
    harness
        .engine
        .cancel(handle.run_id, "user_requested")
        .await
        .unwrap();
    release.notify_one();
    tokio::time::timeout(WAIT, finished.notified())
        .await
        .expect("program observed cancellation");

    let run = harness.wait_terminal(handle.run_id).await;
    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(run.cancel_reason.as_deref(), Some("user_requested"));
    assert_eq!(harness.gateway.calls(), 0);
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();
    assert_eq!(events.last().unwrap().kind, "run.cancelled");
}

#[tokio::test]
async fn start_returns_while_the_owned_execution_task_is_gated() {
    // Mutation caught: awaiting program execution inside start instead of spawning one owned task.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let harness = Harness::new(
        Arc::new(CompletionGateProgram {
            entered: entered.clone(),
            release: release.clone(),
        }),
        FakeGateway::success(1, 1),
    )
    .await;

    let handle = tokio::time::timeout(WAIT, harness.engine.start(harness.turn()))
        .await
        .expect("start returned without waiting for the gated program")
        .unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("owned execution task reached its explicit gate");
    assert!(
        !harness
            .engine
            .get(handle.run_id)
            .await
            .unwrap()
            .status
            .is_terminal()
    );

    release.notify_one();
    assert_eq!(
        harness.wait_terminal(handle.run_id).await.status,
        RunStatus::Completed
    );
}

#[tokio::test]
async fn program_panic_is_contained_and_persisted_as_failed() {
    // Mutation caught: allowing a program panic to unwind the owned task and strand a running row.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let harness = Harness::new(
        Arc::new(PanicGateProgram {
            entered: entered.clone(),
            release: release.clone(),
        }),
        FakeGateway::success(1, 1),
    )
    .await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("program reached the explicit panic gate");

    release.notify_one();
    let run = tokio::time::timeout(WAIT, async {
        loop {
            let run = harness.engine.get(handle.run_id).await.unwrap();
            if run.status.is_terminal() {
                return run;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("program panic was converted to a terminal state");
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(events.last().unwrap().kind, "run.failed");
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
}

#[tokio::test]
async fn exact_usage_crossing_budget_is_persisted_before_budget_terminal() {
    // Mutation caught: discarding the measured call or completing after actual usage exceeds budget.
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::success(800, 300)).await;
    let turn = UserTurn::new(harness.session_id, "budget test").with_limits(8, Some(1_000), None);

    let handle = harness.engine.start(turn).await.unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::BudgetExceeded);
    assert_eq!(run.input_tokens, 800);
    assert_eq!(run.output_tokens, 300);
    assert_eq!(harness.gateway.calls(), 1);
    let usage_seq = events
        .iter()
        .find(|event| event.kind == "model.usage")
        .unwrap()
        .seq;
    let terminal_seq = events
        .iter()
        .find(|event| event.kind == "run.budget_exceeded")
        .unwrap()
        .seq;
    assert!(usage_seq < terminal_seq);
    let stored = sqlx::query(
        "SELECT input_tokens, output_tokens, input_price_microusd_per_million, \
         output_price_microusd_per_million, cost_microusd FROM model_calls WHERE run_id = ?",
    )
    .bind(handle.run_id.to_legacy_hex())
    .fetch_one(&harness.pool)
    .await
    .unwrap();
    assert_eq!(stored.get::<i64, _>("input_tokens"), 800);
    assert_eq!(stored.get::<i64, _>("output_tokens"), 300);
    assert_eq!(
        stored.get::<i64, _>("input_price_microusd_per_million"),
        2_000_000
    );
    assert_eq!(
        stored.get::<i64, _>("output_price_microusd_per_million"),
        8_000_000
    );
    assert_eq!(
        stored.get::<i64, _>("cost_microusd"),
        run.cost_microusd as i64
    );
}

#[tokio::test]
async fn updated_store_defaults_are_leased_for_new_runs_and_crossing_stops() {
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::success(800, 300)).await;
    harness
        .settings
        .update(writing_coach_server::llm::ModelSettingsUpdate {
            default_token_budget: Some(1_000),
            default_cost_budget_microusd: Some(10_000_000),
            ..Default::default()
        })
        .unwrap();

    let handle = harness
        .engine
        .start(UserTurn::new(harness.session_id, "default budget test"))
        .await
        .unwrap();
    let run = harness.wait_terminal(handle.run_id).await;

    assert_eq!(run.token_budget, Some(1_000));
    assert_eq!(run.cost_budget_microusd, Some(10_000_000));
    assert_eq!(run.status, RunStatus::BudgetExceeded);
    assert_eq!((run.input_tokens, run.output_tokens), (800, 300));
}

#[tokio::test]
async fn active_run_keeps_its_start_budget_after_default_update() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let harness = Harness::new(
        Arc::new(CompletionGateProgram {
            entered: entered.clone(),
            release: release.clone(),
        }),
        FakeGateway::success(1, 1),
    )
    .await;
    let first = harness
        .engine
        .start(UserTurn::new(harness.session_id, "first"))
        .await
        .unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .unwrap();
    let first_budget = harness.engine.get(first.run_id).await.unwrap().token_budget;

    harness
        .settings
        .update(writing_coach_server::llm::ModelSettingsUpdate {
            default_token_budget: Some(99),
            default_cost_budget_microusd: Some(199),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        harness.engine.get(first.run_id).await.unwrap().token_budget,
        first_budget
    );
    release.notify_one();
    harness.wait_terminal(first.run_id).await;

    let second = harness
        .engine
        .start(UserTurn::new(harness.session_id, "second"))
        .await
        .unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .unwrap();
    let second_run = harness.engine.get(second.run_id).await.unwrap();
    assert_eq!(second_run.token_budget, Some(99));
    assert_eq!(second_run.cost_budget_microusd, Some(199));
    release.notify_one();
    harness.wait_terminal(second.run_id).await;
}

#[tokio::test]
async fn completed_run_has_monotonic_events_and_exactly_one_terminal_event() {
    // Mutations caught: non-transactional sequence allocation or duplicate terminal finalization.
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::success(21, 8)).await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();

    let run = harness.wait_terminal(handle.run_id).await;
    harness
        .engine
        .cancel(handle.run_id, "late_cancel")
        .await
        .unwrap();
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        (1..=events.len() as u64).collect::<Vec<_>>()
    );
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
    assert_eq!(events.last().unwrap().kind, "run.completed");
    assert_eq!(harness.gateway.calls(), 1);
}

#[tokio::test]
async fn pre_call_exhausted_budget_prevents_model_started_and_provider_call() {
    // Mutation caught: checking an exhausted budget only after an external call.
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::success(1, 1)).await;
    let turn = UserTurn::new(harness.session_id, "no budget").with_limits(8, Some(0), None);

    let handle = harness.engine.start(turn).await.unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::BudgetExceeded);
    assert_eq!(harness.gateway.calls(), 0);
    assert!(!events.iter().any(|event| event.kind == "model.started"));
    assert_eq!(events.last().unwrap().kind, "run.budget_exceeded");
}

#[tokio::test]
async fn estimated_prompt_and_reserved_output_reject_unaffordable_token_budget_pre_call() {
    // Break caught: a positive but obviously insufficient remaining token budget is treated as
    // affordable until measured usage arrives after the provider has already been called.
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::success(1, 1)).await;
    let turn = UserTurn::new(harness.session_id, "unaffordable token call").with_limits(
        8,
        Some(500),
        None,
    );

    let handle = harness.engine.start(turn).await.unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::BudgetExceeded);
    assert_eq!(harness.gateway.calls(), 0);
    assert!(!events.iter().any(|event| event.kind == "model.started"));
    assert_eq!(events.last().unwrap().kind, "run.budget_exceeded");
}

#[tokio::test]
async fn estimated_prompt_and_reserved_output_reject_unaffordable_cost_budget_pre_call() {
    // Break caught: cost budgets are checked only against prior measured spend, not a conservative
    // price estimate for the prompt plus the configured output reservation.
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::success(1, 1)).await;
    let turn =
        UserTurn::new(harness.session_id, "unaffordable cost call").with_limits(8, None, Some(100));

    let handle = harness.engine.start(turn).await.unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::BudgetExceeded);
    assert_eq!(harness.gateway.calls(), 0);
    assert!(!events.iter().any(|event| event.kind == "model.started"));
    assert_eq!(events.last().unwrap().kind, "run.budget_exceeded");
}

#[tokio::test]
async fn run_start_context_length_changes_runtime_admission_before_provider_call() {
    // Break caught: context_length is exposed in settings but omitted from the immutable Run lease
    // and therefore has no effect on the request actually admitted by RunContext.
    let message = "context capacity must affect this request";
    let constrained = Harness::new(Arc::new(CallingProgram), FakeGateway::success(1, 1)).await;
    constrained
        .settings
        .update(writing_coach_server::llm::ModelSettingsUpdate {
            context_length: Some(520),
            ..Default::default()
        })
        .unwrap();
    let rejected = constrained
        .engine
        .start(UserTurn::new(constrained.session_id, message))
        .await
        .unwrap();
    let rejected = constrained.wait_terminal(rejected.run_id).await;

    assert_eq!(rejected.status, RunStatus::Failed);
    assert_eq!(constrained.gateway.calls(), 0);

    let roomy = Harness::new(Arc::new(CallingProgram), FakeGateway::success(1, 1)).await;
    let admitted = roomy
        .engine
        .start(UserTurn::new(roomy.session_id, message))
        .await
        .unwrap();
    assert_eq!(
        roomy.wait_terminal(admitted.run_id).await.status,
        RunStatus::Completed
    );
    assert_eq!(roomy.gateway.calls(), 1);
}

#[tokio::test]
async fn persisted_model_identity_uses_the_normalized_response_not_configured_aliases() {
    // Break caught: price settings are snapshotted correctly but provider/model columns record the
    // configured request aliases instead of the gateway's actual normalized response identity.
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::success(1, 1)).await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();
    harness.wait_terminal(handle.run_id).await;
    let stored = sqlx::query("SELECT provider, model FROM model_calls WHERE run_id = ?")
        .bind(handle.run_id.to_legacy_hex())
        .fetch_one(&harness.pool)
        .await
        .unwrap();

    assert_eq!(stored.get::<String, _>("provider"), "fake");
    assert_eq!(stored.get::<String, _>("model"), "fixture");
}

#[tokio::test]
async fn exact_cost_crossing_budget_persists_cost_before_stopping() {
    // Mutation caught: enforcing only token limits or checking cost before storing measured usage.
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::success(25, 1)).await;
    harness
        .settings
        .update(writing_coach_server::llm::ModelSettingsUpdate {
            max_output_tokens: Some(1),
            ..Default::default()
        })
        .unwrap();
    let turn = UserTurn::new(harness.session_id, "cost budget").with_limits(8, None, Some(50));

    let handle = harness.engine.start(turn).await.unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::BudgetExceeded);
    assert_eq!(run.cost_microusd, 58);
    assert_eq!(events[events.len() - 2].kind, "model.usage");
    assert_eq!(events.last().unwrap().kind, "run.budget_exceeded");
}

#[tokio::test]
async fn maximum_steps_fails_before_persisting_the_next_step() {
    // Mutation caught: allowing step max + 1 to begin before enforcing the limit.
    let harness = Harness::new(Arc::new(TooManyStepsProgram), FakeGateway::success(1, 1)).await;
    let turn = UserTurn::new(harness.session_id, "step test").with_limits(1, None, None);

    let handle = harness.engine.start(turn).await.unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "step.started")
            .count(),
        1
    );
    assert_eq!(events.last().unwrap().kind, "run.failed");
}

#[tokio::test]
async fn concurrent_cloned_contexts_admit_only_one_step_at_the_limit() {
    // Mutation caught: separate atomic load/append/increment operations admitting max_steps + 1.
    let harness = Harness::new(Arc::new(ConcurrentStepsProgram), FakeGateway::success(1, 1)).await;
    let turn = UserTurn::new(harness.session_id, "concurrent steps").with_limits(1, None, None);

    let handle = harness.engine.start(turn).await.unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "step.started")
            .count(),
        1
    );
    assert_eq!(events.last().unwrap().kind, "run.failed");
}

#[tokio::test]
async fn concurrent_model_calls_stop_after_first_exact_budget_exhaustion() {
    // Mutation caught: letting a clone pass the pre-call budget check before first usage commits.
    let harness = Harness::new(
        Arc::new(ConcurrentModelCallsProgram),
        FakeGateway::success(1, 1),
    )
    .await;
    let turn =
        UserTurn::new(harness.session_id, "concurrent models").with_limits(2, Some(524), None);

    let handle = harness.engine.start(turn).await.unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();

    assert_eq!(run.status, RunStatus::BudgetExceeded);
    assert_eq!(run.input_tokens, 1);
    assert_eq!(run.output_tokens, 1);
    assert_eq!(harness.gateway.calls(), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "model.started")
            .count(),
        1
    );
}

#[tokio::test]
async fn provider_failure_becomes_failed_without_usage_record() {
    // Mutation caught: completing a run or recording zero usage after a provider failure.
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::provider_failure()).await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();

    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();
    let calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_calls WHERE run_id = ?")
        .bind(handle.run_id.to_legacy_hex())
        .fetch_one(&harness.pool)
        .await
        .unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(calls, 0);
    assert!(!events.iter().any(|event| event.kind == "model.usage"));
    assert_eq!(events.last().unwrap().kind, "run.failed");
}

#[tokio::test]
async fn model_usage_transaction_rolls_back_call_and_totals_when_event_insert_fails() {
    // Mutation caught: committing model_calls or cumulative totals before the usage event insert.
    let harness = Harness::new(Arc::new(CallingProgram), FakeGateway::success(7, 3)).await;
    sqlx::query(
        "CREATE TRIGGER inject_model_usage_failure BEFORE INSERT ON run_events \
         WHEN NEW.kind = 'model.usage' BEGIN SELECT RAISE(ABORT, 'injected usage failure'); END",
    )
    .execute(&harness.pool)
    .await
    .unwrap();

    let handle = harness.engine.start(harness.turn()).await.unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();
    let calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_calls WHERE run_id = ?")
        .bind(handle.run_id.to_legacy_hex())
        .fetch_one(&harness.pool)
        .await
        .unwrap();

    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.input_tokens, 0);
    assert_eq!(run.output_tokens, 0);
    assert_eq!(run.cost_microusd, 0);
    assert_eq!(calls, 0);
    assert!(!events.iter().any(|event| event.kind == "model.usage"));
    assert_eq!(events.last().unwrap().kind, "run.failed");
}

#[tokio::test]
async fn cancellation_during_model_call_produces_no_usage_or_later_event() {
    // Mutation caught: awaiting a pending provider without cancellation or persisting its response after cancel.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let gateway = FakeGateway::gated(50, 20, entered.clone(), release);
    let harness = Harness::new(Arc::new(CallingProgram), gateway).await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("model call reached the explicit provider gate");

    harness
        .engine
        .cancel(handle.run_id, "stop_pending_model")
        .await
        .unwrap();
    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();
    let calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM model_calls WHERE run_id = ?")
        .bind(handle.run_id.to_legacy_hex())
        .fetch_one(&harness.pool)
        .await
        .unwrap();

    assert_eq!(run.status, RunStatus::Cancelled);
    assert_eq!(harness.gateway.calls(), 1);
    assert_eq!(calls, 0);
    assert!(!events.iter().any(|event| event.kind == "model.usage"));
    assert_eq!(events.last().unwrap().kind, "run.cancelled");
}

#[tokio::test]
async fn price_snapshot_is_immutable_after_start_and_atomic_with_usage() {
    // Mutation caught: reading mutable prices after the provider returns or updating totals separately.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let gateway = FakeGateway::gated(1, 1, entered.clone(), release.clone());
    let harness = Harness::new(Arc::new(CallingProgram), gateway).await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("model call reached the explicit provider gate");
    harness
        .settings
        .update(writing_coach_server::llm::ModelSettingsUpdate {
            input_price_microusd_per_million: Some(100_000_000),
            output_price_microusd_per_million: Some(200_000_000),
            ..Default::default()
        })
        .unwrap();
    release.notify_one();

    let run = harness.wait_terminal(handle.run_id).await;
    let stored = sqlx::query(
        "SELECT input_price_microusd_per_million, output_price_microusd_per_million, \
         cost_microusd FROM model_calls WHERE run_id = ?",
    )
    .bind(handle.run_id.to_legacy_hex())
    .fetch_one(&harness.pool)
    .await
    .unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(
        stored.get::<i64, _>("input_price_microusd_per_million"),
        2_000_000
    );
    assert_eq!(
        stored.get::<i64, _>("output_price_microusd_per_million"),
        8_000_000
    );
    assert_eq!(stored.get::<i64, _>("cost_microusd"), 10);
    assert_eq!(run.cost_microusd, 10);
}

#[tokio::test]
async fn start_time_call_settings_keep_transport_identity_and_prices_coherent() {
    // Mutation caught: resolving mutable model identity at call time after snapshotting old prices.
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
    let settings = Arc::new(ModelSettingsStore::new(model_config()).unwrap());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let finished = Arc::new(Notify::new());
    let program = BlockingBeforeModelProgram {
        entered: entered.clone(),
        release: release.clone(),
        finished,
    };
    let engine = RunEngine::new(
        pool.clone(),
        Arc::new(program),
        Arc::new(MutableSettingsGateway),
        settings.clone(),
    );
    let handle = engine
        .start(UserTurn::new(session.id, "lease settings"))
        .await
        .unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("program reached the explicit pre-gateway gate");
    settings
        .update(writing_coach_server::llm::ModelSettingsUpdate {
            provider: Some("deepseek".to_owned()),
            name: Some("new-model".to_owned()),
            input_price_microusd_per_million: Some(100_000_000),
            output_price_microusd_per_million: Some(200_000_000),
            ..Default::default()
        })
        .unwrap();
    release.notify_one();

    let run = wait_terminal_with_engine(&engine, handle.run_id).await;
    let stored = sqlx::query(
        "SELECT provider, model, input_price_microusd_per_million, \
         output_price_microusd_per_million FROM model_calls WHERE run_id = ?",
    )
    .bind(handle.run_id.to_legacy_hex())
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(stored.get::<String, _>("provider"), "openai");
    assert_eq!(stored.get::<String, _>("model"), "configured-model");
    assert_eq!(
        stored.get::<i64, _>("input_price_microusd_per_million"),
        2_000_000
    );
    assert_eq!(
        stored.get::<i64, _>("output_price_microusd_per_million"),
        8_000_000
    );
}

#[tokio::test]
async fn completion_cancel_race_has_one_agreeing_terminal_status_and_event() {
    // Mutation caught: unconditional terminal inserts that let completion and cancellation both win.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let program = CompletionGateProgram {
        entered: entered.clone(),
        release: release.clone(),
    };
    let harness = Harness::new(Arc::new(program), FakeGateway::success(1, 1)).await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("program reached the explicit completion gate");

    let cancel = {
        let engine = harness.engine.clone();
        tokio::spawn(async move { engine.cancel(handle.run_id, "race_cancel").await })
    };
    release.notify_one();
    tokio::time::timeout(WAIT, cancel)
        .await
        .expect("cancel contender completed")
        .unwrap()
        .unwrap();

    let run = harness.wait_terminal(handle.run_id).await;
    let events = harness.engine.events(handle.run_id, 0).await.unwrap();
    let terminals = events
        .iter()
        .filter(|event| event.is_terminal())
        .collect::<Vec<_>>();
    assert_eq!(terminals.len(), 1);
    assert_eq!(terminals[0].kind, run.status.terminal_event_kind().unwrap());
    assert_eq!(events.last().unwrap().kind, terminals[0].kind);
}

#[tokio::test]
async fn persisted_get_and_replay_survive_a_new_engine_instance() {
    // Mutation caught: keeping run summaries or event history only in the live registry.
    let gateway = FakeGateway::success(2, 1);
    let harness = Harness::new(Arc::new(CallingProgram), gateway.clone()).await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();
    let completed = harness.wait_terminal(handle.run_id).await;

    let restarted = RunEngine::new(
        harness.pool.clone(),
        Arc::new(CallingProgram),
        Arc::new(gateway),
        Arc::new(ModelSettingsStore::new(model_config()).unwrap()),
    );
    let restored = restarted.get(handle.run_id).await.unwrap();
    let replay = restarted.subscribe(handle.run_id, 0).await.unwrap().replay;

    assert_eq!(restored.status, completed.status);
    assert_eq!(restored.input_tokens, 2);
    assert_eq!(replay.last().unwrap().kind, "run.completed");
}

#[tokio::test]
async fn startup_reconciliation_terminalizes_an_orphan_and_unblocks_its_session() {
    // Break caught: a restarted process leaves a persisted running row without an owner, so
    // subscriptions install an inert hub and every later Run for the Session conflicts forever.
    let gateway = FakeGateway::success(2, 1);
    let harness = Harness::new(Arc::new(CallingProgram), gateway.clone()).await;
    let repository = RunRepository::new(harness.pool.clone());
    let orphan = repository
        .create(harness.session_id, 8, None, None)
        .await
        .unwrap();
    repository.mark_running(orphan.id).await.unwrap();
    repository
        .append_event(
            orphan.id,
            "step.started",
            json!({"step": "call_model"}),
            Some("call_model"),
        )
        .await
        .unwrap();

    let restarted = RunEngine::new(
        harness.pool.clone(),
        Arc::new(CallingProgram),
        Arc::new(gateway),
        Arc::new(ModelSettingsStore::new(model_config()).unwrap()),
    );
    let reconciled = restarted.reconcile_orphans().await.unwrap();

    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].run_id, orphan.id);
    assert_eq!(reconciled[0].kind, "run.failed");
    let restored = restarted.get(orphan.id).await.unwrap();
    assert_eq!(restored.status, RunStatus::Failed);
    assert_eq!(
        restored.error_message.as_deref(),
        Some("interrupted by service restart")
    );
    let events = restarted.events(orphan.id, 0).await.unwrap();
    assert_eq!(events.iter().filter(|event| event.is_terminal()).count(), 1);
    assert_eq!(events.last().unwrap().kind, "run.failed");

    let later = restarted
        .start(UserTurn::new(harness.session_id, "continue after restart"))
        .await
        .unwrap();
    assert_eq!(
        wait_terminal_with_engine(&restarted, later.run_id)
            .await
            .status,
        RunStatus::Completed
    );
}

#[tokio::test]
async fn terminal_replay_subscription_closes_without_retaining_a_live_hub() {
    // Mutation caught: inserting every historical subscription into the live registry forever.
    let gateway = FakeGateway::success(2, 1);
    let harness = Harness::new(Arc::new(CallingProgram), gateway.clone()).await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();
    harness.wait_terminal(handle.run_id).await;
    let restarted = RunEngine::new(
        harness.pool.clone(),
        Arc::new(CallingProgram),
        Arc::new(gateway),
        Arc::new(ModelSettingsStore::new(model_config()).unwrap()),
    );

    let mut subscription = restarted.subscribe(handle.run_id, 0).await.unwrap();
    assert_eq!(subscription.replay.last().unwrap().kind, "run.completed");
    let closed = tokio::time::timeout(Duration::from_millis(100), subscription.recv())
        .await
        .expect("terminal subscription receiver closed immediately");
    assert!(matches!(
        closed,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));
}

#[tokio::test]
async fn running_subscription_is_not_retained_after_the_run_becomes_terminal() {
    // Mutation caught: retaining a hub installed for a live subscriber after task finalization.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let harness = Harness::new(
        Arc::new(CompletionGateProgram {
            entered: entered.clone(),
            release: release.clone(),
        }),
        FakeGateway::success(1, 1),
    )
    .await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("program reached the explicit completion gate");
    let live_subscription = harness.engine.subscribe(handle.run_id, 0).await.unwrap();

    release.notify_one();
    harness.wait_terminal(handle.run_id).await;
    drop(live_subscription);
    let mut historical = harness.engine.subscribe(handle.run_id, 0).await.unwrap();
    assert_eq!(historical.replay.last().unwrap().kind, "run.completed");
    assert!(matches!(
        historical.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));
}

#[tokio::test]
async fn file_backed_pools_admit_only_one_active_run_for_a_session() {
    // Mutation caught: checking active runs without a cross-connection writer serialization point.
    let database = TempSqliteFile::new();
    let pool_a = sqlite::open_database(&database.url()).await.unwrap();
    sqlite::migrate(&pool_a).await.unwrap();
    let pool_b = sqlite::open_database(&database.url()).await.unwrap();
    let session = SessionRepository::new(pool_a.clone())
        .create(None)
        .await
        .unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let first = {
        let repository = RunRepository::new(pool_a.clone());
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            repository.create(session.id, 8, None, None).await
        })
    };
    let second = {
        let repository = RunRepository::new(pool_b.clone());
        let barrier = barrier.clone();
        tokio::spawn(async move {
            barrier.wait().await;
            repository.create(session.id, 8, None, None).await
        })
    };

    barrier.wait().await;
    let outcomes = [first.await.unwrap(), second.await.unwrap()];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        outcomes
            .iter()
            .any(|result| matches!(result, Err(AppError::ActiveRunConflict)))
    );

    pool_a.close().await;
    pool_b.close().await;
}

#[tokio::test]
async fn dropping_external_engine_handle_does_not_imply_completion() {
    // Mutation caught: a Drop implementation that marks an in-flight run completed.
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let finished = Arc::new(Notify::new());
    let program = BlockingBeforeModelProgram {
        entered: entered.clone(),
        release: release.clone(),
        finished: finished.clone(),
    };
    let harness = Harness::new(Arc::new(program), FakeGateway::success(3, 2)).await;
    let handle = harness.engine.start(harness.turn()).await.unwrap();
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .expect("program reached the explicit pre-model gate");
    let restarted = RunEngine::new(
        harness.pool.clone(),
        Arc::new(CallingProgram),
        Arc::new(harness.gateway.clone()),
        Arc::new(ModelSettingsStore::new(model_config()).unwrap()),
    );

    drop(harness.engine);
    assert_eq!(
        restarted.get(handle.run_id).await.unwrap().status,
        RunStatus::Running
    );
    release.notify_one();
    tokio::time::timeout(WAIT, finished.notified())
        .await
        .expect("detached execution task continued after external handle drop");
    let mut subscription = restarted.subscribe(handle.run_id, 0).await.unwrap();
    let run = tokio::time::timeout(WAIT, async {
        loop {
            let run = restarted.get(handle.run_id).await.unwrap();
            if run.status.is_terminal() {
                return run;
            }
            subscription.recv().await.unwrap();
        }
    })
    .await
    .expect("original execution task finalized through persistence");
    assert_eq!(run.status, RunStatus::Completed);
}

fn model_config() -> ModelConfig {
    ModelConfig {
        provider: "openai".to_owned(),
        endpoint: "http://127.0.0.1:1/v1".to_owned(),
        name: "configured-model".to_owned(),
        api_key_env: "WRITING_COACH_TASK8_TEST_KEY_NOT_SET".to_owned(),
        context_length: 8_192,
        max_output_tokens: 512,
        reasoning_mode: "medium".to_owned(),
        input_price_microusd_per_million: 2_000_000,
        output_price_microusd_per_million: 8_000_000,
    }
}

fn run_defaults() -> RunDefaults {
    RunDefaults {
        max_input_tokens: 8_000,
        max_output_tokens: 512,
        max_cost_microusd: 10_000_000,
    }
}

async fn wait_terminal_with_engine(
    engine: &RunEngine,
    run_id: writing_coach_server::domain::RunId,
) -> writing_coach_server::domain::AgentRun {
    let mut subscription = engine.subscribe(run_id, 0).await.unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            let run = engine.get(run_id).await.unwrap();
            if run.status.is_terminal() {
                return run;
            }
            subscription.recv().await.unwrap();
        }
    })
    .await
    .expect("run reached a terminal state")
}
