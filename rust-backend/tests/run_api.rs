use std::{fs, path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::{sync::Notify, time::timeout};
use tower::ServiceExt;
use uuid::Uuid;
use writing_coach_server::{
    AppConfig, AppState,
    agent::{AgentAnswer, AgentProgram, RunContext, RunEngine, UserTurn},
    config::{ModelConfig, RunDefaults},
    domain::{RunId, RunStatus},
    llm::{GenaiModelGateway, ModelSettingsStore},
    store::{
        sessions::{DocumentRepository, SkillEventRepository},
        sqlite,
    },
};

const WAIT: Duration = Duration::from_secs(5);
const IMPORT_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug)]
struct SseEvent {
    id: u64,
    event: String,
    data: Value,
}

type ExportMutation<'a> = Box<dyn Fn(&mut Value) + 'a>;

struct Harness {
    app: Router,
    database_path: PathBuf,
}

struct ControlledHarness {
    app: Router,
    engine: RunEngine,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    database_path: PathBuf,
}

struct GatedProgram {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    event_count: usize,
}

struct ReloadFailureProgram {
    start: Arc<Notify>,
    emit: Arc<Notify>,
    overflowed: Arc<Notify>,
    finish: Arc<Notify>,
}

#[async_trait]
impl AgentProgram for GatedProgram {
    async fn execute(
        &self,
        context: RunContext,
        _turn: UserTurn,
    ) -> Result<AgentAnswer, writing_coach_server::AppError> {
        context
            .emit("step.started", json!({"step": "gated"}))
            .await?;
        self.entered.notify_one();
        self.release.notified().await;
        for index in 0..self.event_count {
            context
                .emit("answer.delta", json!({"index": index}))
                .await?;
        }
        context
            .emit("step.completed", json!({"step": "gated"}))
            .await?;
        Ok(AgentAnswer::new("controlled answer"))
    }
}

#[async_trait]
impl AgentProgram for ReloadFailureProgram {
    async fn execute(
        &self,
        context: RunContext,
        _turn: UserTurn,
    ) -> Result<AgentAnswer, writing_coach_server::AppError> {
        context
            .emit("step.started", json!({"step": "reload"}))
            .await?;
        self.start.notify_one();
        self.emit.notified().await;
        for index in 0..300 {
            context
                .emit("answer.delta", json!({"index": index}))
                .await?;
        }
        self.overflowed.notify_one();
        self.finish.notified().await;
        Ok(AgentAnswer::new("done"))
    }
}

impl ControlledHarness {
    async fn new(event_count: usize) -> Self {
        let database_path = std::env::temp_dir().join(format!(
            "writing-coach-controlled-run-api-{}.db",
            Uuid::new_v4().simple()
        ));
        let database_url = format!("sqlite://{}", database_path.display());
        let pool = sqlite::open_database(&database_url).await.unwrap();
        sqlite::migrate(&pool).await.unwrap();
        let settings = Arc::new(
            ModelSettingsStore::new_with_run_defaults(test_model_config(), test_run_defaults())
                .unwrap(),
        );
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let program = Arc::new(GatedProgram {
            entered: entered.clone(),
            release: release.clone(),
            event_count,
        });
        let gateway = Arc::new(GenaiModelGateway::new(settings.clone()));
        let engine = RunEngine::new(pool.clone(), program, gateway, settings.clone());
        let state = AppState {
            pool,
            run_engine: engine.clone(),
            model_settings: settings,
        };
        Self {
            app: writing_coach_server::api::router(state),
            engine,
            entered,
            release,
            database_path,
        }
    }

    async fn create(&self) -> Value {
        request_json(
            self.app.clone(),
            Method::POST,
            "/api/runs",
            json!({"message": "controlled"}),
        )
        .await
        .1
    }
}

impl Drop for ControlledHarness {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.database_path);
        let _ = fs::remove_file(self.database_path.with_extension("db-shm"));
        let _ = fs::remove_file(self.database_path.with_extension("db-wal"));
    }
}

#[tokio::test]
async fn run_creation_rejects_unknown_actions() {
    let app = ControlledHarness::new(0).await;

    let (status, error) = request_json(
        app.app.clone(),
        Method::POST,
        "/api/runs",
        json!({"message": "形成思路", "action": "unknown_action"}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid JSON request");
}

impl Harness {
    async fn new() -> Self {
        let database_path = std::env::temp_dir().join(format!(
            "writing-coach-run-api-{}.db",
            Uuid::new_v4().simple()
        ));
        let database_url = format!("sqlite://{}", database_path.display());
        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let source = format!(
            r#"
bind_addr = "127.0.0.1:0"
database_url = {database_url:?}
skill_root = {skill_root:?}
corpus_root = {corpus_root:?}

[model]
provider = "openai-compatible"
endpoint = "http://127.0.0.1:1234/v1"
name = "local-writing-model"
api_key_env = "WRITING_COACH_RUN_API_TEST_KEY"
context_length = 32768
max_output_tokens = 4096
reasoning_mode = "medium"
input_price_microusd_per_million = 500000
output_price_microusd_per_million = 1500000

[run_defaults]
max_input_tokens = 24000
max_output_tokens = 4096
max_cost_microusd = 5000000
"#,
            skill_root = project_root.join("skills").display().to_string(),
            corpus_root = project_root.join("corpus").display().to_string(),
        );
        let mut config = AppConfig::from_toml(&source).unwrap();
        config.database_url = database_url;
        let app = writing_coach_server::build_app(config).await.unwrap();
        Self { app, database_path }
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        headers: HeaderMap,
        body: Body,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut request = Request::builder().method(method).uri(uri);
        *request.headers_mut().unwrap() = headers;
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = timeout(WAIT, response.into_body().collect())
            .await
            .expect("response body terminates")
            .unwrap()
            .to_bytes()
            .to_vec();
        (status, headers, body)
    }

    async fn json(&self, method: Method, uri: &str, payload: Value) -> (StatusCode, Value) {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let (status, _, body) = self
            .request(method, uri, headers, Body::from(payload.to_string()))
            .await;
        let body = serde_json::from_slice(&body)
            .unwrap_or_else(|error| panic!("expected JSON response, got {body:?}: {error}"));
        (status, body)
    }

    async fn get_json(&self, uri: &str) -> (StatusCode, Value) {
        let (status, _, body) = self
            .request(Method::GET, uri, HeaderMap::new(), Body::empty())
            .await;
        let body = serde_json::from_slice(&body).unwrap();
        (status, body)
    }

    async fn create_completed_run(&self) -> Value {
        let (status, created) = self
            .json(
                Method::POST,
                "/api/runs",
                json!({"message": "我没思路", "student_id": "20260001", "student_name": "张三"}),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let events = self
            .sse(
                &format!("/api/runs/{}/events", created["run_id"].as_str().unwrap()),
                HeaderMap::new(),
            )
            .await;
        assert_eq!(events.last().unwrap().event, "run.completed");
        created
    }

    async fn sse(&self, uri: &str, headers: HeaderMap) -> Vec<SseEvent> {
        let (status, response_headers, body) =
            self.request(Method::GET, uri, headers, Body::empty()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            response_headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        parse_sse(&String::from_utf8(body).unwrap())
    }

    async fn pool(&self) -> SqlitePool {
        sqlite::open_database(&format!("sqlite://{}", self.database_path.display()))
            .await
            .unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.database_path);
        let _ = fs::remove_file(self.database_path.with_extension("db-shm"));
        let _ = fs::remove_file(self.database_path.with_extension("db-wal"));
    }
}

#[tokio::test]
async fn create_get_and_stream_run_progress_to_a_terminal_event() {
    // Break caught: the Run HTTP adapter is absent, blocks until completion, or leaves SSE open.
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let run_id = created["run_id"].as_str().unwrap();
    let session_id = created["session_id"].as_str().unwrap();
    assert_lower_hex_id(run_id);
    assert_lower_hex_id(session_id);

    let (status, run) = app.get_json(&format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["run_id"], run_id);
    assert_eq!(run["session_id"], session_id);
    assert_eq!(run["status"], "completed");

    let events = app
        .sse(&format!("/api/runs/{run_id}/events"), HeaderMap::new())
        .await;
    assert!(events.iter().any(|event| event.event == "step.started"));
    assert_eq!(events.last().unwrap().event, "run.completed");
    assert!(events.windows(2).all(|pair| pair[0].id < pair[1].id));
    assert!(events.iter().all(|event| event.data["seq"] == event.id));
}

#[tokio::test]
async fn synchronous_chat_exposes_the_synthesize_action_contract() {
    let app = Harness::new().await;
    let (status, first) = app
        .json(
            Method::POST,
            "/api/chat",
            json!({"message": "我没思路", "student_id": "2025010468", "student_name": "李捷铭"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    let (status, synthesis) = app
        .json(
            Method::POST,
            "/api/chat",
            json!({
                "session_id": first["session_id"],
                "message": "请根据当前对话形成完整思路。",
                "action": "synthesize"
            }),
        )
        .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(synthesis["metadata"]["action"], "synthesize");
    assert!(synthesis["reply"].as_str().unwrap().contains("## 论证路径"));
    assert_eq!(synthesis["awaiting_slots"], json!([]));
    let run_id = synthesis["metadata"]["run_id"].as_str().unwrap();
    let (status, run) = app.get_json(&format!("/api/runs/{run_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["input_tokens"], 0);
    assert_eq!(run["output_tokens"], 0);
    assert_eq!(run["cost_microusd"], 0);
}

#[tokio::test]
async fn replay_honors_query_header_and_their_maximum_without_duplicates() {
    // Break caught: reconnect starts from only one cursor, includes the cursor, or duplicates overlap.
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let run_id = created["run_id"].as_str().unwrap();
    let all = app
        .sse(&format!("/api/runs/{run_id}/events"), HeaderMap::new())
        .await;
    assert!(all.len() >= 4);
    let first = all[0].id;
    let second = all[1].id;

    let query = app
        .sse(
            &format!("/api/runs/{run_id}/events?after_seq={first}"),
            HeaderMap::new(),
        )
        .await;
    assert_eq!(query.first().unwrap().id, second);

    let mut headers = HeaderMap::new();
    headers.insert(
        "last-event-id",
        HeaderValue::from_str(&second.to_string()).unwrap(),
    );
    let header_only = app
        .sse(&format!("/api/runs/{run_id}/events"), headers.clone())
        .await;
    assert!(header_only.iter().all(|event| event.id > second));

    let max = app
        .sse(
            &format!("/api/runs/{run_id}/events?after_seq={first}"),
            headers,
        )
        .await;
    assert_eq!(
        max.iter().map(|event| event.id).collect::<Vec<_>>(),
        header_only.iter().map(|event| event.id).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn invalid_cursors_are_safe_and_ids_are_strict_lowercase_hex() {
    // Break caught: malformed cursors become SQL errors or uppercase IDs bypass authorization checks.
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let run_id = created["run_id"].as_str().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("last-event-id", HeaderValue::from_static("not-a-sequence"));
    let invalid = app
        .sse(
            &format!("/api/runs/{run_id}/events?after_seq=9223372036854775808"),
            headers,
        )
        .await;
    assert_eq!(invalid.last().unwrap().event, "run.completed");

    let uppercase = run_id.to_ascii_uppercase();
    let (status, error) = app.get_json(&format!("/api/runs/{uppercase}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid identifier");
    assert!(!error.to_string().contains(&uppercase));
}

#[tokio::test]
async fn model_settings_are_public_secret_redacted_and_updated_atomically() {
    // Break caught: a temporary key is serialized/persisted or a failed update partially applies.
    let app = Harness::new().await;
    let (status, initial) = app.get_json("/api/settings/model").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initial["api_key_configured"], false);
    assert!(initial.get("api_key").is_none());

    let secret = "sk-task-ten-never-return";
    let (status, updated) = app
        .json(
            Method::PUT,
            "/api/settings/model",
            json!({
                "name": "updated-model",
                "api_key": secret,
                "context_length": 64000,
                "input_price_microusd_per_million": 0
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["name"], "updated-model");
    assert_eq!(updated["api_key_configured"], true);
    assert!(!updated.to_string().contains(secret));

    let rejected_secret = "sk-rejected-secret";
    let (status, error) = app
        .json(
            Method::PUT,
            "/api/settings/model",
            json!({"endpoint": "http://provider.example.test/v1", "api_key": rejected_secret}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!error.to_string().contains(rejected_secret));
    assert!(!error.to_string().contains("provider.example.test"));
    let (_, current) = app.get_json("/api/settings/model").await;
    assert_eq!(current["name"], "updated-model");
    assert_ne!(current["endpoint"], "http://provider.example.test/v1");

    let startup_env = current["api_key_env"].clone();
    let (status, error) = app
        .json(
            Method::PUT,
            "/api/settings/model",
            json!({"api_key_env": "HOME", "name": "must-not-stick"}),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid JSON request");
    assert!(!error.to_string().contains("HOME"));
    let (_, unchanged) = app.get_json("/api/settings/model").await;
    assert_eq!(unchanged["api_key_env"], startup_env);
    assert_eq!(unchanged["name"], "updated-model");
}

#[tokio::test]
async fn updated_default_budgets_are_applied_to_each_new_http_run() {
    let app = ControlledHarness::new(0).await;
    let (status, settings) = request_json(
        app.app.clone(),
        Method::PUT,
        "/api/settings/model",
        json!({
            "default_token_budget": 1234,
            "default_cost_budget_microusd": 5678
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["default_token_budget"], 1234);
    assert_eq!(settings["default_cost_budget_microusd"], 5678);

    let created = app.create().await;
    let run_id = created["run_id"].as_str().unwrap();
    timeout(WAIT, app.entered.notified()).await.unwrap();
    let (status, run) = request_json(
        app.app.clone(),
        Method::GET,
        &format!("/api/runs/{run_id}"),
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(run["token_budget"], 1234);
    assert_eq!(run["cost_budget_microusd"], 5678);
    app.release.notify_one();
}

#[tokio::test]
async fn json_request_rejections_are_fixed_safe_json_across_task_ten_routes() {
    // Break caught: Axum rejection details or secret-bearing malformed bodies escape an endpoint.
    let app = Harness::new().await;
    for (method, uri) in [
        (Method::POST, "/api/runs"),
        (Method::POST, "/api/chat"),
        (Method::PUT, "/api/settings/model"),
        (Method::POST, "/api/sessions/import"),
    ] {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let (status, _, body) = app
            .request(
                method,
                uri,
                headers,
                Body::from(r#"{"message":"sk-malformed-secret",BROKEN}"#),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"error": "invalid JSON request"})
        );
        assert!(!String::from_utf8_lossy(&body).contains("sk-malformed-secret"));
    }

    let controlled = ControlledHarness::new(0).await;
    let created = controlled.create().await;
    timeout(WAIT, controlled.entered.notified()).await.unwrap();
    let response = controlled
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/api/runs/{}/cancel",
                    created["run_id"].as_str().unwrap()
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{BROKEN"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = timeout(WAIT, response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"error": "invalid JSON request"})
    );
    controlled.release.notify_one();
}

#[tokio::test]
async fn wait_style_chat_maps_cancelled_terminal_state_deliberately() {
    // Break caught: non-completed chat terminals collapse into a misleading generic 400.
    let app = ControlledHarness::new(0).await;
    let chat = tokio::spawn({
        let router = app.app.clone();
        async move {
            request_json(
                router,
                Method::POST,
                "/api/chat",
                json!({"message": "controlled"}),
            )
            .await
        }
    });
    timeout(WAIT, app.entered.notified()).await.unwrap();
    let pool = sqlite::open_database(&format!("sqlite://{}", app.database_path.display()))
        .await
        .unwrap();
    let run_id: String = sqlx::query_scalar(
        "SELECT id FROM agent_runs WHERE status = 'running' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let (status, _) = request_json(
        app.app.clone(),
        Method::POST,
        &format!("/api/runs/{run_id}/cancel"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    app.release.notify_one();
    let (status, error) = timeout(WAIT, chat).await.unwrap().unwrap();
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"], "agent run was cancelled");
    pool.close().await;
}

#[tokio::test]
async fn wait_style_chat_returns_legacy_shape_from_the_one_persisted_run() {
    // Break caught: /api/chat calls the program directly or returns a second non-Run response shape.
    let app = Harness::new().await;
    let (status, chat) = app
        .json(
            Method::POST,
            "/api/chat",
            json!({"message": "我没思路", "user_id": "student-chat"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_lower_hex_id(chat["session_id"].as_str().unwrap());
    assert!(chat["reply"].is_string());
    assert!(chat.get("current_skill").is_some());
    assert!(chat["awaiting_slots"].is_array());
    assert!(chat["metadata"].is_object());
    let run_id = chat["metadata"]["run_id"].as_str().unwrap();
    assert_lower_hex_id(run_id);

    let (run_status, run) = app.get_json(&format!("/api/runs/{run_id}")).await;
    assert_eq!(run_status, StatusCode::OK);
    assert_eq!(run["session_id"], chat["session_id"]);
    let (_, history) = app
        .get_json(&format!(
            "/api/sessions/{}/messages",
            chat["session_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(history["messages"].as_array().unwrap().len(), 2);
    assert_eq!(history["messages"][1]["reply"], Value::Null);
    assert_eq!(history["messages"][1]["content"], chat["reply"]);
    assert_eq!(history["messages"][1]["metadata_json"]["run_id"], run_id);

    let pool = app.pool().await;
    let run_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_runs WHERE session_id = ?")
        .bind(chat["session_id"].as_str().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(run_count, 1);
    pool.close().await;
}

#[tokio::test]
async fn chat_preserves_legacy_identity_updates_on_an_existing_session() {
    // Break caught: an existing-session chat stores the profile but leaves session filtering on the old ID.
    let app = Harness::new().await;
    let (_, first) = app
        .json(
            Method::POST,
            "/api/chat",
            json!({"message": "我没思路", "student_id": "old-id", "student_name": "旧名"}),
        )
        .await;
    let session_id = first["session_id"].as_str().unwrap();
    let (status, second) = app
        .json(
            Method::POST,
            "/api/chat",
            json!({
                "session_id": session_id,
                "message": "我没思路",
                "user_id": "new-id",
                "student_name": "新名"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(second["session_id"], session_id);
    let (_, filtered) = app.get_json("/api/sessions?user_id=new-id").await;
    assert_eq!(filtered["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["sessions"][0]["student_name"], "新名");
    assert_eq!(filtered["sessions"][0]["student_id"], "new-id");
}

#[tokio::test]
async fn run_reservation_rolls_back_new_sessions_and_existing_identity_on_rejection() {
    // Break caught: API mutates/creates a Session before empty-content or active-run rejection.
    let app = ControlledHarness::new(0).await;
    let pool = sqlite::open_database(&format!("sqlite://{}", app.database_path.display()))
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (status, _) = request_json(
        app.app.clone(),
        Method::POST,
        "/api/runs",
        json!({"message": "   ", "student_id": "orphan-id"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let after_empty: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after_empty, before);

    let (status, created) = request_json(
        app.app.clone(),
        Method::POST,
        "/api/runs",
        json!({"message": "controlled", "student_id": "original-id", "student_name": "原名"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    timeout(WAIT, app.entered.notified()).await.unwrap();
    let session_id = created["session_id"].as_str().unwrap();
    let (status, error) = request_json(
        app.app.clone(),
        Method::POST,
        "/api/runs",
        json!({
            "session_id": session_id,
            "message": "second",
            "user_id": "must-not-stick",
            "student_name": "不应写入"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"], "session already has an active run");
    let row = sqlx::query("SELECT user_id, state_json FROM sessions JOIN session_states ON sessions.id = session_states.session_id WHERE sessions.id = ?")
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let user_id: Option<String> = sqlx::Row::try_get(&row, "user_id").unwrap();
    let state: String = sqlx::Row::try_get(&row, "state_json").unwrap();
    assert_eq!(user_id.as_deref(), Some("original-id"));
    assert!(!state.contains("must-not-stick"));
    assert!(!state.contains("不应写入"));

    let run_id = created["run_id"].as_str().unwrap();
    let (status, _) = request_json(
        app.app.clone(),
        Method::POST,
        &format!("/api/runs/{run_id}/cancel"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    app.release.notify_one();
    pool.close().await;
}

#[tokio::test]
async fn queued_run_insert_failure_leaves_no_new_session() {
    // Break caught: Session/profile commits independently before a queued-run database failure.
    let app = ControlledHarness::new(0).await;
    let pool = sqlite::open_database(&format!("sqlite://{}", app.database_path.display()))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_queued_run BEFORE INSERT ON agent_runs \
         BEGIN SELECT RAISE(ABORT, 'private trigger detail'); END",
    )
    .execute(&pool)
    .await
    .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (status, error) = request_json(
        app.app.clone(),
        Method::POST,
        "/api/runs",
        json!({"message": "controlled", "user_id": "rollback-id", "student_name": "rollback"}),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"], "internal server error");
    assert!(!error.to_string().contains("private trigger"));
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(after, before);
    pool.close().await;
}

#[tokio::test]
async fn session_transfer_round_trips_the_full_trajectory_with_new_foreign_keys() {
    // Break caught: export omits trajectory rows or import retains source IDs/breaks foreign keys.
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let session_id = created["session_id"].as_str().unwrap();
    let run_id = created["run_id"].as_str().unwrap();
    let (second_status, second) = app
        .json(
            Method::POST,
            "/api/runs",
            json!({"session_id": session_id, "message": "谢谢"}),
        )
        .await;
    assert_eq!(second_status, StatusCode::ACCEPTED);
    let second_run_id = second["run_id"].as_str().unwrap();
    app.sse(
        &format!("/api/runs/{second_run_id}/events"),
        HeaderMap::new(),
    )
    .await;
    let pool = app.pool().await;
    let parsed_session = writing_coach_server::domain::SessionId::parse_legacy(session_id).unwrap();
    DocumentRepository::new(pool.clone())
        .add(
            parsed_session,
            "notes.md",
            "text/markdown",
            None,
            Some("材料"),
            Some(json!({"tag": "evidence"})),
        )
        .await
        .unwrap();
    SkillEventRepository::new(pool.clone())
        .add(
            parsed_session,
            "socratic_review",
            "selected",
            Some(json!({"reason": "test"})),
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO model_calls (id, run_id, purpose, provider, model, input_tokens, output_tokens, \
         input_price_microusd_per_million, output_price_microusd_per_million, cost_microusd, \
         duration_ms, finish_reason, response_id, created_at) \
         VALUES (?, ?, 'answer', 'fake', 'fixture', 3, 2, 10, 20, 2, 1, 'stop', 'response-1', CURRENT_TIMESTAMP)",
    )
    .bind(Uuid::new_v4().simple().to_string())
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE agent_runs SET input_tokens = 3, output_tokens = 2, cost_microusd = 2 WHERE id = ?",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let (status, mut export) = app
        .get_json(&format!("/api/sessions/{session_id}/export"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(export["schema"], "writing-coach.session");
    assert_eq!(export["version"], 1);
    assert_eq!(export["source_session_id"], session_id);
    assert_eq!(export["messages"].as_array().unwrap().len(), 4);
    assert_eq!(export["documents"].as_array().unwrap().len(), 1);
    // Both no-model Socratic slot-question Runs record their selected skill, in addition to the
    // explicit fixture event inserted above.
    assert_eq!(export["skill_events"].as_array().unwrap().len(), 3);
    assert_eq!(export["runs"].as_array().unwrap().len(), 2);
    assert_eq!(export["runs"][0]["id"], run_id);
    assert_eq!(export["runs"][1]["id"], second_run_id);
    assert!(!export["run_events"].as_array().unwrap().is_empty());
    assert_eq!(export["model_calls"].as_array().unwrap().len(), 1);
    export["messages"][1]["metadata_json"]["nested"]["run_id"] = json!(run_id);
    export["skill_events"][0]["metadata_json"]["trajectory"]["run-id"] = json!(run_id);

    let (status, imported) = app
        .json(Method::POST, "/api/sessions/import", export.clone())
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let imported_session = imported["session_id"].as_str().unwrap();
    assert_lower_hex_id(imported_session);
    assert_ne!(imported_session, session_id);
    let response_run_ids = imported["run_ids"].as_array().unwrap();
    assert_eq!(response_run_ids.len(), 2);
    assert!(response_run_ids.iter().all(|id| {
        id.as_str().is_some_and(|value| {
            assert_lower_hex_id(value);
            value != run_id && value != second_run_id
        })
    }));
    let (_, imported_export) = app
        .get_json(&format!("/api/sessions/{imported_session}/export"))
        .await;
    assert_eq!(imported_export["messages"].as_array().unwrap().len(), 4);
    assert_eq!(imported_export["documents"].as_array().unwrap().len(), 1);
    assert_eq!(imported_export["skill_events"].as_array().unwrap().len(), 3);
    assert_eq!(imported_export["runs"].as_array().unwrap().len(), 2);
    assert_eq!(imported_export["model_calls"].as_array().unwrap().len(), 1);
    assert_eq!(
        imported_export["state"]["state_json"]["imported_from_session_id"],
        session_id
    );
    let imported_run = imported_export["runs"][0]["id"].as_str().unwrap();
    assert_lower_hex_id(imported_run);
    assert_ne!(imported_run, run_id);
    assert_eq!(response_run_ids[0], imported_export["runs"][0]["id"]);
    assert_eq!(response_run_ids[1], imported_export["runs"][1]["id"]);
    let imported_run_ids = response_run_ids
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(
        imported_export["run_events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| imported_run_ids.contains(&event["run_id"].as_str().unwrap()))
    );
    assert!(
        imported_export["model_calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|call| call["run_id"] == imported_run)
    );
    assert_eq!(
        imported_export["messages"][1]["metadata_json"]["nested"]["run_id"],
        imported_run
    );
    assert_eq!(
        imported_export["skill_events"][0]["metadata_json"]["trajectory"]["run-id"],
        imported_run
    );
}

#[tokio::test]
async fn transfer_recomputes_each_model_cost_and_matches_call_sums_to_run_totals() {
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let session_id = created["session_id"].as_str().unwrap();
    let run_id = created["run_id"].as_str().unwrap();
    let pool = app.pool().await;
    sqlx::query(
        "INSERT INTO model_calls (id, run_id, purpose, provider, model, input_tokens, output_tokens, \
         input_price_microusd_per_million, output_price_microusd_per_million, cost_microusd, \
         duration_ms, finish_reason, response_id, created_at) \
         VALUES (?, ?, 'answer', 'fake', 'fixture', 3, 2, 10, 20, 2, 1, 'stop', 'response-1', CURRENT_TIMESTAMP)",
    )
    .bind(Uuid::new_v4().simple().to_string())
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE agent_runs SET input_tokens = 3, output_tokens = 2, cost_microusd = 2 WHERE id = ?",
    )
    .bind(run_id)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let (_, export) = app
        .get_json(&format!("/api/sessions/{session_id}/export"))
        .await;

    let (status, _) = app
        .json(Method::POST, "/api/sessions/import", export.clone())
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let mut repriced = export.clone();
    repriced["model_calls"][0]["cost_microusd"] = json!(1);
    let (status, error) = app
        .json(Method::POST, "/api/sessions/import", repriced)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid session export");

    let mut mismatched_totals = export;
    mismatched_totals["runs"][0]["input_tokens"] = json!(4);
    let (status, error) = app
        .json(Method::POST, "/api/sessions/import", mismatched_totals)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid session export");
}

#[tokio::test]
async fn transfer_secret_scan_covers_keys_and_string_fields_without_secretary_false_positive() {
    // Break caught: credentials bypass transfer in open JSON or string fields, or benign secretary text is blocked.
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let session_id = created["session_id"].as_str().unwrap();
    let run_id = created["run_id"].as_str().unwrap();
    let pool = app.pool().await;
    DocumentRepository::new(pool.clone())
        .add(
            writing_coach_server::domain::SessionId::parse_legacy(session_id).unwrap(),
            "safe.md",
            "text/markdown",
            Some("safe/path"),
            Some("ordinary document"),
            Some(json!({"secretary_notes": "bearer market basics"})),
        )
        .await
        .unwrap();
    pool.close().await;
    let (_, export) = app
        .get_json(&format!("/api/sessions/{session_id}/export"))
        .await;

    let (status, _) = app
        .json(Method::POST, "/api/sessions/import", export.clone())
        .await;
    assert_eq!(status, StatusCode::CREATED);

    let mut product_identifiers = export.clone();
    product_identifiers["messages"][0]["content"] =
        json!("PK-12345678901234567890 is a product identifier in an academic catalog");
    product_identifiers["documents"][0]["parsed_text"] =
        json!("Study accession token-style-identifier-2026 is not a credential");
    let (status, _) = app
        .json(Method::POST, "/api/sessions/import", product_identifiers)
        .await;
    assert_eq!(status, StatusCode::CREATED);

    for key in [
        "openaiApiKey",
        "client-secret",
        "access Token",
        "Authorization.Header",
    ] {
        let mut malicious = export.clone();
        malicious["state"]["state_json"][key] = json!("credential-material");
        let (status, error) = app
            .json(Method::POST, "/api/sessions/import", malicious)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "key={key}");
        assert_eq!(error["error"], "invalid session export");
        assert!(!error.to_string().contains("credential-material"));
    }

    let string_mutations: Vec<ExportMutation<'_>> = vec![
        Box::new(|value| {
            value["messages"][0]["content"] =
                json!("Authorization: Bearer abcdefghijklmnopqrstuvwxyz")
        }),
        Box::new(|value| {
            value["documents"][0]["raw_path"] =
                json!("/safe/path?access_token=tok_abcdefghijklmnopqrstuvwxyz")
        }),
        Box::new(|value| {
            value["runs"][0]["error_message"] = json!("client_secret=abcdefghijklmnopqrstuvwxyz")
        }),
        Box::new(|value| {
            value["messages"][0]["content"] = json!("api_key: abcdefghijklmnopqrstuvwxyz")
        }),
        Box::new(|value| {
            value["documents"][0]["parsed_text"] =
                json!("ClientSecret = \"abcdefghijklmnopqrstuvwxyz\"")
        }),
        Box::new(|value| {
            value["messages"][0]["content"] = json!("access.token : 'abcdefghijklmnopqrstuvwxyz'")
        }),
        Box::new(move |value| {
            let created_at = value["runs"][0]["finished_at"].clone();
            value["model_calls"].as_array_mut().unwrap().push(json!({
                "id": Uuid::new_v4().simple().to_string(),
                "run_id": run_id,
                "purpose": "answer",
                "provider": "fake",
                "model": "sk-abcdefghijklmnopqrstuvwxyz",
                "input_tokens": 0,
                "output_tokens": 0,
                "input_price_microusd_per_million": 0,
                "output_price_microusd_per_million": 0,
                "cost_microusd": 0,
                "duration_ms": 0,
                "finish_reason": "stop",
                "response_id": null,
                "created_at": created_at
            }));
        }),
    ];
    for mutate in string_mutations {
        let mut malicious = export.clone();
        mutate(&mut malicious);
        let (status, error) = app
            .json(Method::POST, "/api/sessions/import", malicious)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"], "invalid session export");
    }
}

#[tokio::test]
async fn transfer_rejects_unsafe_or_incoherent_run_trajectories_and_timestamps() {
    // Break caught: active, gapped, mismatched, duplicated, post-terminal, or malformed trajectories import.
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let session_id = created["session_id"].as_str().unwrap();
    let (_, export) = app
        .get_json(&format!("/api/sessions/{session_id}/export"))
        .await;

    let mut invalid_exports = Vec::new();
    let mut active = export.clone();
    active["runs"][0]["status"] = json!("running");
    invalid_exports.push(active);

    let mut missing_terminal = export.clone();
    missing_terminal["run_events"].as_array_mut().unwrap().pop();
    invalid_exports.push(missing_terminal);

    let mut mismatched = export.clone();
    mismatched["runs"][0]["status"] = json!("failed");
    invalid_exports.push(mismatched);

    let mut double_terminal = export.clone();
    let mut second_terminal = double_terminal["run_events"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    second_terminal["id"] = json!(Uuid::new_v4().simple().to_string());
    second_terminal["seq"] = json!(double_terminal["run_events"].as_array().unwrap().len() + 1);
    double_terminal["run_events"]
        .as_array_mut()
        .unwrap()
        .push(second_terminal);
    invalid_exports.push(double_terminal);

    let mut gap = export.clone();
    gap["run_events"].as_array_mut().unwrap().remove(1);
    invalid_exports.push(gap);

    let mut post_terminal = export.clone();
    let next_seq = post_terminal["run_events"].as_array().unwrap().len() + 1;
    let post_terminal_time = post_terminal["runs"][0]["finished_at"].clone();
    post_terminal["run_events"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": Uuid::new_v4().simple().to_string(),
            "run_id": created["run_id"],
            "seq": next_seq,
            "kind": "answer.delta",
            "payload": {},
            "created_at": post_terminal_time
        }));
    invalid_exports.push(post_terminal);

    let mut invalid_time = export.clone();
    invalid_time["runs"][0]["finished_at"] = json!("not-a-timestamp");
    invalid_exports.push(invalid_time);

    let mut reversed_time = export.clone();
    reversed_time["runs"][0]["created_at"] = json!("2099-01-01 00:00:00");
    invalid_exports.push(reversed_time);

    let mut orphan_embedded_run = export.clone();
    orphan_embedded_run["state"]["state_json"]["nested"]["run_id"] =
        json!(Uuid::new_v4().simple().to_string());
    invalid_exports.push(orphan_embedded_run);

    for invalid in invalid_exports {
        let (status, error) = app
            .json(Method::POST, "/api/sessions/import", invalid)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"], "invalid session export");
    }
}

#[tokio::test]
async fn invalid_duplicate_and_oversize_imports_fail_without_partial_sessions() {
    // Break caught: malformed transfer data partially commits or bypasses the request bound.
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let session_id = created["session_id"].as_str().unwrap();
    let (_, export) = app
        .get_json(&format!("/api/sessions/{session_id}/export"))
        .await;
    let before = session_count(&app).await;

    let mut wrong_version = export.clone();
    wrong_version["version"] = json!(2);
    let (status, _) = app
        .json(Method::POST, "/api/sessions/import", wrong_version)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut wrong_schema = export.clone();
    wrong_schema["schema"] = json!("unexpected.schema");
    let (status, _) = app
        .json(Method::POST, "/api/sessions/import", wrong_schema)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut unknown_field = export.clone();
    unknown_field["api_key"] = json!("must-not-be-accepted-or-echoed");
    let (status, error) = app
        .json(Method::POST, "/api/sessions/import", unknown_field)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid JSON request");
    assert!(!error.to_string().contains("must-not-be-accepted"));

    let mut camel_case_secret = export.clone();
    camel_case_secret["state"]["state_json"]["openaiApiKey"] =
        json!("sk-camel-case-must-not-transfer");
    let (status, error) = app
        .json(Method::POST, "/api/sessions/import", camel_case_secret)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid session export");
    assert!(!error.to_string().contains("sk-camel-case"));

    let mut duplicate = export.clone();
    let first = duplicate["messages"][0].clone();
    duplicate["messages"].as_array_mut().unwrap().push(first);
    let (status, _) = app
        .json(Method::POST, "/api/sessions/import", duplicate)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(session_count(&app).await, before);

    let oversized = json!({"padding": "x".repeat(IMPORT_LIMIT)}).to_string();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let (status, _, _) = app
        .request(
            Method::POST,
            "/api/sessions/import",
            headers,
            Body::from(oversized),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(session_count(&app).await, before);
}

#[tokio::test]
async fn cancellation_through_http_wins_at_a_deterministic_program_gate() {
    // Break caught: cancel only changes the HTTP response while the persisted Run completes later.
    let app = ControlledHarness::new(0).await;
    let created = app.create().await;
    let run_id = created["run_id"].as_str().unwrap();
    timeout(WAIT, app.entered.notified())
        .await
        .expect("program reached gate");

    let (status, cancelled) = request_json(
        app.app.clone(),
        Method::POST,
        &format!("/api/runs/{run_id}/cancel"),
        json!({"reason": "student_stop"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["status"], "cancelled");
    let (status, cancelled_again) = request_json(
        app.app.clone(),
        Method::POST,
        &format!("/api/runs/{run_id}/cancel"),
        json!({"reason": "ignored-second-reason"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled_again["status"], "cancelled");
    app.release.notify_one();

    let events = collect_sse_router(
        app.app.clone(),
        &format!("/api/runs/{run_id}/events"),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(events.last().unwrap().event, "run.cancelled");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event.starts_with("run.") && event.event != "run.started")
            .count(),
        1
    );
    assert_eq!(
        app.engine
            .get(RunId::parse_legacy(run_id).unwrap())
            .await
            .unwrap()
            .status,
        RunStatus::Cancelled
    );
}

#[tokio::test]
async fn lagged_sse_receiver_reloads_every_missing_sequence_from_sqlite() {
    // Break caught: broadcast overflow silently drops middle events instead of replaying persistence.
    let app = ControlledHarness::new(300).await;
    let created = app.create().await;
    let run_id = created["run_id"].as_str().unwrap();
    timeout(WAIT, app.entered.notified())
        .await
        .expect("program reached gate");

    let response = app
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/runs/{run_id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    app.release.notify_one();
    wait_engine_terminal(&app.engine, RunId::parse_legacy(run_id).unwrap()).await;

    let body = timeout(WAIT, response.into_body().collect())
        .await
        .expect("lagged SSE reload terminates")
        .unwrap()
        .to_bytes();
    let events = parse_sse(std::str::from_utf8(&body).unwrap());
    assert_eq!(events.last().unwrap().event, "run.completed");
    assert!(events.len() > 256);
    assert!(events.windows(2).all(|pair| pair[1].id == pair[0].id + 1));
}

#[tokio::test]
async fn lagged_sse_reload_database_failure_terminates_without_delivering_a_higher_sequence() {
    // Break caught: a failed lag reload is swallowed and the live receiver resumes after a gap.
    let database_path = std::env::temp_dir().join(format!(
        "writing-coach-sse-reload-failure-{}.db",
        Uuid::new_v4().simple()
    ));
    let pool = sqlite::open_database(&format!("sqlite://{}", database_path.display()))
        .await
        .unwrap();
    sqlite::migrate(&pool).await.unwrap();
    let settings = Arc::new(
        ModelSettingsStore::new_with_run_defaults(test_model_config(), test_run_defaults())
            .unwrap(),
    );
    let start = Arc::new(Notify::new());
    let emit = Arc::new(Notify::new());
    let overflowed = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let program = Arc::new(ReloadFailureProgram {
        start: start.clone(),
        emit: emit.clone(),
        overflowed: overflowed.clone(),
        finish: finish.clone(),
    });
    let engine = RunEngine::new(
        pool.clone(),
        program,
        Arc::new(GenaiModelGateway::new(settings.clone())),
        settings.clone(),
    );
    let app = writing_coach_server::api::router(AppState {
        pool: pool.clone(),
        run_engine: engine,
        model_settings: settings,
    });
    let (_, created) = request_json(
        app.clone(),
        Method::POST,
        "/api/runs",
        json!({"message": "reload"}),
    )
    .await;
    timeout(WAIT, start.notified()).await.unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/runs/{}/events",
                    created["run_id"].as_str().unwrap()
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    emit.notify_one();
    timeout(WAIT, overflowed.notified()).await.unwrap();
    pool.close().await;
    let body = timeout(WAIT, response.into_body().collect())
        .await
        .expect("reload failure terminates the SSE body")
        .unwrap()
        .to_bytes();
    let events = parse_sse(std::str::from_utf8(&body).unwrap());
    assert!(!events.is_empty());
    assert!(events.last().unwrap().id < 300);
    assert!(!events.iter().any(|event| event.event == "run.completed"));
    finish.notify_one();
    let _ = fs::remove_file(&database_path);
    let _ = fs::remove_file(database_path.with_extension("db-shm"));
    let _ = fs::remove_file(database_path.with_extension("db-wal"));
}

#[tokio::test]
async fn live_sse_heartbeat_is_a_comment_without_business_data() {
    // Break caught: heartbeat is a named/data event or the connection has no 15-second liveness frame.
    let app = ControlledHarness::new(0).await;
    let created = app.create().await;
    let run_id = created["run_id"].as_str().unwrap();
    timeout(WAIT, app.entered.notified())
        .await
        .expect("program reached gate");
    let response = app
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/runs/{run_id}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut body = response.into_body();
    for _ in 0..2 {
        timeout(WAIT, body.frame())
            .await
            .expect("initial SSE frame arrives")
            .unwrap()
            .unwrap();
    }
    tokio::time::pause();
    let heartbeat = tokio::spawn(async move {
        let frame = body.frame().await.unwrap().unwrap();
        (frame.into_data().unwrap(), body)
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(15)).await;
    let (heartbeat, body) = timeout(WAIT, heartbeat)
        .await
        .expect("heartbeat frame arrives")
        .unwrap();
    let heartbeat = String::from_utf8(heartbeat.to_vec()).unwrap();
    assert!(heartbeat.starts_with(": heartbeat"));
    assert!(!heartbeat.contains("data:"));
    assert!(!heartbeat.contains("event:"));

    let (status, _) = request_json(
        app.app.clone(),
        Method::POST,
        &format!("/api/runs/{run_id}/cancel"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    app.release.notify_one();
    let tail = timeout(WAIT, body.collect())
        .await
        .expect("heartbeat tail terminates")
        .unwrap()
        .to_bytes();
    assert!(
        String::from_utf8(tail.to_vec())
            .unwrap()
            .contains("run.cancelled")
    );
}

#[tokio::test]
async fn sqlite_constraint_failure_rolls_back_every_imported_row() {
    // Break caught: import commits the new session/state before a later child constraint fails.
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let session_id = created["session_id"].as_str().unwrap();
    let (_, mut export) = app
        .get_json(&format!("/api/sessions/{session_id}/export"))
        .await;
    export["messages"][0]["content"] = json!("reject-import-after-parent");
    let pool = app.pool().await;
    sqlx::query(
        "CREATE TRIGGER reject_import_message BEFORE INSERT ON messages \
         WHEN NEW.content = 'reject-import-after-parent' \
         BEGIN SELECT RAISE(ABORT, 'private database path must not escape'); END",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
    let before = session_count(&app).await;

    let (status, error) = app.json(Method::POST, "/api/sessions/import", export).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"], "internal server error");
    assert!(!error.to_string().contains("private database path"));
    assert_eq!(session_count(&app).await, before);
}

#[tokio::test]
async fn export_rejects_a_session_larger_than_the_transfer_bound() {
    // Break caught: request imports are bounded but a server-side export can allocate arbitrarily.
    let app = Harness::new().await;
    let created = app.create_completed_run().await;
    let session_id = created["session_id"].as_str().unwrap();
    let pool = app.pool().await;
    sqlx::query("UPDATE messages SET content = ? WHERE session_id = ? AND role = 'user'")
        .bind("x".repeat(IMPORT_LIMIT))
        .bind(session_id)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let (status, error) = app
        .get_json(&format!("/api/sessions/{session_id}/export"))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"], "invalid session export");
}

fn parse_sse(source: &str) -> Vec<SseEvent> {
    source
        .split("\n\n")
        .filter_map(|block| {
            let mut id = None;
            let mut event = None;
            let mut data = None;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("id:") {
                    id = value.trim().parse().ok();
                } else if let Some(value) = line.strip_prefix("event:") {
                    event = Some(value.trim().to_owned());
                } else if let Some(value) = line.strip_prefix("data:") {
                    data = serde_json::from_str(value.trim()).ok();
                }
            }
            Some(SseEvent {
                id: id?,
                event: event?,
                data: data?,
            })
        })
        .collect()
}

fn assert_lower_hex_id(value: &str) {
    assert_eq!(value.len(), 32);
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    );
}

async fn session_count(app: &Harness) -> usize {
    let (_, sessions) = app.get_json("/api/sessions").await;
    sessions["sessions"].as_array().unwrap().len()
}

async fn request_json(
    app: Router,
    method: Method,
    uri: &str,
    payload: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = timeout(WAIT, response.into_body().collect())
        .await
        .expect("JSON response terminates")
        .unwrap()
        .to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn collect_sse_router(app: Router, uri: &str, headers: HeaderMap) -> Vec<SseEvent> {
    let mut request = Request::builder().uri(uri);
    *request.headers_mut().unwrap() = headers;
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = timeout(WAIT, response.into_body().collect())
        .await
        .expect("SSE response terminates")
        .unwrap()
        .to_bytes();
    parse_sse(std::str::from_utf8(&body).unwrap())
}

async fn wait_engine_terminal(engine: &RunEngine, run_id: RunId) {
    timeout(WAIT, async {
        let mut subscription = engine.subscribe(run_id, 0).await.unwrap();
        if subscription.replay.iter().any(|event| event.is_terminal()) {
            return;
        }
        loop {
            match subscription.recv().await {
                Ok(event) if event.is_terminal() => return,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    assert!(engine.get(run_id).await.unwrap().status.is_terminal());
                    return;
                }
            }
        }
    })
    .await
    .expect("run reaches a terminal state");
}

fn test_model_config() -> ModelConfig {
    ModelConfig {
        provider: "openai-compatible".to_owned(),
        endpoint: "http://127.0.0.1:1234/v1".to_owned(),
        name: "unused-controlled-model".to_owned(),
        api_key_env: "WRITING_COACH_RUN_API_TEST_KEY".to_owned(),
        context_length: 32_768,
        max_output_tokens: 4_096,
        reasoning_mode: "medium".to_owned(),
        input_price_microusd_per_million: 0,
        output_price_microusd_per_million: 0,
    }
}

fn test_run_defaults() -> RunDefaults {
    RunDefaults {
        max_input_tokens: 24_000,
        max_output_tokens: 4_096,
        max_cost_microusd: 5_000_000,
    }
}
