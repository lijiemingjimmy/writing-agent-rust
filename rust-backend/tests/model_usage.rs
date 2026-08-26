use std::{
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use writing_coach_server::{
    AppConfig,
    config::{ModelConfig, RunDefaults},
    domain::{PriceSnapshot, Usage},
    llm::{
        GenaiModelGateway, ModelError, ModelGateway, ModelMessage, ModelRequest, ModelResponse,
        ModelSettingsError, ModelSettingsStore, ModelSettingsUpdate, calculate_cost,
    },
};

const TEST_CONFIG: &str = r#"
bind_addr = "127.0.0.1:0"
database_url = "sqlite::memory:"
skill_root = "../skills"
corpus_root = "../corpus"

[model]
provider = "openai"
endpoint = "http://127.0.0.1:1234/v1"
name = "local-writing-model"
api_key_env = "WRITING_COACH_TEST_API_KEY_THAT_IS_NOT_SET"
context_length = 32768
max_output_tokens = 4096
reasoning_mode = "medium"
input_price_microusd_per_million = 500000
output_price_microusd_per_million = 1500000

[run_defaults]
max_input_tokens = 24000
max_output_tokens = 4096
max_cost_microusd = 5000000
"#;

#[test]
fn price_is_rounded_up_in_integer_microusd() {
    // Mutation caught: floating-point/truncating arithmetic, or combining token classes before ceil.
    let usage = Usage {
        input_tokens: 501,
        output_tokens: 200,
    };
    let price = PriceSnapshot {
        input_microusd_per_million: 2_000_000,
        output_microusd_per_million: 8_000_000,
    };

    assert_eq!(calculate_cost(usage, price).unwrap().0, 2_602);
    assert_eq!(
        calculate_cost(
            Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
            PriceSnapshot {
                input_microusd_per_million: 1,
                output_microusd_per_million: 1,
            },
        )
        .unwrap()
        .0,
        2,
    );
}

#[test]
fn price_rejects_a_final_u64_overflow() {
    // Mutation caught: wrapping or saturating an unrepresentable microdollar total.
    let error = calculate_cost(
        Usage {
            input_tokens: u64::MAX,
            output_tokens: u64::MAX,
        },
        PriceSnapshot {
            input_microusd_per_million: u64::MAX,
            output_microusd_per_million: u64::MAX,
        },
    )
    .unwrap_err();

    assert!(error.to_string().contains("overflows"));
}

#[test]
fn config_rejects_negative_or_overflowing_prices() {
    // Mutation caught: signed/float price parsing or lossy narrowing into u64.
    for invalid_price in ["-1", "18446744073709551616"] {
        let source = TEST_CONFIG.replace(
            "input_price_microusd_per_million = 500000",
            &format!("input_price_microusd_per_million = {invalid_price}"),
        );
        assert!(AppConfig::from_toml(&source).is_err());
    }
}

#[test]
fn prices_above_sqlite_integer_range_are_rejected_atomically() {
    // Mutation caught: accepting a u64 rate that model_calls cannot persist as SQLite INTEGER.
    let too_large = u64::try_from(i64::MAX).unwrap() + 1;
    let mut startup_config = model_config("https://models.example.test/v1");
    startup_config.input_price_microusd_per_million = too_large;
    let startup_error = ModelSettingsStore::new(startup_config).unwrap_err();
    assert!(startup_error.to_string().contains("price"));
    assert!(!startup_error.to_string().contains(&too_large.to_string()));

    let store = settings_with_temporary_key("price-range-secret");
    let before = store.public();
    let update_error = store
        .update(ModelSettingsUpdate {
            name: Some("must-not-stick".to_owned()),
            output_price_microusd_per_million: Some(too_large),
            ..ModelSettingsUpdate::default()
        })
        .unwrap_err();

    assert!(update_error.to_string().contains("price"));
    assert_eq!(store.public(), before);
    let rendered = format!("{update_error:?} {update_error} {store:?}");
    assert!(!rendered.contains("price-range-secret"));
    assert!(!rendered.contains(&too_large.to_string()));
}

#[test]
fn default_budgets_are_positive_sqlite_safe_and_updated_atomically() {
    let store = settings_with_temporary_key("budget-range-secret");
    let before = store.public();
    assert!(before.default_token_budget > 0);
    assert!(before.default_cost_budget_microusd > 0);

    for update in [
        ModelSettingsUpdate {
            name: Some("must-not-stick-zero".to_owned()),
            default_token_budget: Some(0),
            ..ModelSettingsUpdate::default()
        },
        ModelSettingsUpdate {
            name: Some("must-not-stick-range".to_owned()),
            default_cost_budget_microusd: Some(i64::MAX as u64 + 1),
            ..ModelSettingsUpdate::default()
        },
    ] {
        let error = store.update(update).unwrap_err();
        assert!(error.to_string().contains("budget"));
        assert_eq!(store.public(), before);
        assert!(!format!("{error:?} {error} {store:?}").contains("budget-range-secret"));
    }
}

#[test]
fn maximum_output_must_fit_inside_the_model_context_atomically() {
    // Break caught: context_length is accepted as display-only metadata even when the configured
    // output reservation alone cannot fit in that context window.
    let mut invalid = model_config("https://models.example.test/v1");
    invalid.context_length = 256;
    invalid.max_output_tokens = 512;
    let error = ModelSettingsStore::new(invalid).unwrap_err();
    assert!(error.to_string().contains("context"));

    let store = settings_with_temporary_key("context-validation-secret");
    let before = store.public();
    let error = store
        .update(ModelSettingsUpdate {
            context_length: Some(256),
            name: Some("must-not-stick".to_owned()),
            ..ModelSettingsUpdate::default()
        })
        .unwrap_err();
    assert!(error.to_string().contains("context"));
    assert_eq!(store.public(), before);
}

#[test]
fn config_rejects_model_values_that_cannot_form_a_runtime_gateway() {
    // Mutations caught: deferring invalid endpoint/output/reasoning values until the first request.
    for (valid, invalid) in [
        (
            "endpoint = \"http://127.0.0.1:1234/v1\"",
            "endpoint = \"not-an-endpoint\"",
        ),
        (
            "endpoint = \"http://127.0.0.1:1234/v1\"",
            "endpoint = \"https://sk-endpoint-secret@example.test/v1\"",
        ),
        ("max_output_tokens = 4096", "max_output_tokens = 0"),
        (
            "reasoning_mode = \"medium\"",
            "reasoning_mode = \"unsupported\"",
        ),
    ] {
        let error = AppConfig::from_toml(&TEST_CONFIG.replace(valid, invalid)).unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("invalid configuration"));
        assert!(!rendered.contains("sk-"));
    }
}

#[test]
fn provider_whitelist_accepts_only_verified_course_transports() {
    // Mutation caught: rejecting either the DeepSeek path or the configured OpenAI-compatible path.
    for provider in [
        "openai",
        "openai-compatible",
        "openai_compatible",
        "deepseek",
    ] {
        let mut config = model_config("https://models.example.test/v1");
        config.provider = provider.to_owned();
        assert!(ModelSettingsStore::new(config).is_ok(), "{provider}");
    }
}

#[test]
fn provider_whitelist_rejects_genai_adapters_not_verified_for_direct_targets() {
    // Mutation caught: advertising every genai AdapterKind even when it ignores/rewrites direct targets.
    for provider in ["baidu", "anthropic", "gemini", "ollama"] {
        let mut config = model_config("https://models.example.test/v1");
        config.provider = provider.to_owned();
        assert_eq!(
            ModelSettingsStore::new(config).unwrap_err(),
            ModelSettingsError::UnsupportedProvider,
            "{provider}"
        );
    }
}

#[test]
fn endpoint_policy_requires_remote_https_and_allows_loopback_http() {
    // Mutation caught: sending API keys over cleartext to non-loopback endpoints.
    let remote_http = TEST_CONFIG.replace(
        "endpoint = \"http://127.0.0.1:1234/v1\"",
        "endpoint = \"http://models.example.test/v1\"",
    );
    assert!(AppConfig::from_toml(&remote_http).is_err());

    for endpoint in [
        "http://localhost:1234/v1",
        "http://127.0.0.1:1234/v1",
        "http://[::1]:1234/v1",
        "https://models.example.test/v1?api-version=2026-08-25",
    ] {
        assert!(
            ModelSettingsStore::new(model_config(endpoint)).is_ok(),
            "{endpoint}"
        );
    }
}

#[test]
fn endpoint_policy_rejects_fragments_and_non_public_query_keys() {
    // Mutation caught: serializing credentials smuggled into endpoint fragments or query parameters.
    for endpoint in [
        "https://models.example.test/v1#sk-fragment-secret",
        "https://models.example.test/v1?api_key=sk-query-secret",
        "https://models.example.test/v1?ApiKey=sk-query-secret",
        "https://models.example.test/v1?key=sk-query-secret",
        "https://models.example.test/v1?token=sk-query-secret",
        "https://models.example.test/v1?access_token=sk-query-secret",
        "https://models.example.test/v1?signature=sk-query-secret",
        "https://models.example.test/v1?sig=sk-query-secret",
        "https://models.example.test/v1?authorization=sk-query-secret",
        "https://models.example.test/v1?region=public-but-not-approved",
    ] {
        assert_eq!(
            ModelSettingsStore::new(model_config(endpoint)).unwrap_err(),
            ModelSettingsError::InvalidEndpoint,
            "{endpoint}"
        );
    }
}

#[test]
fn rejected_endpoint_update_is_atomic_and_redacted_before_validation() {
    // Mutations caught: Debug leaking an unvalidated candidate, or committing other candidate fields first.
    let store = ModelSettingsStore::new(model_config(
        "https://models.example.test/v1?api-version=stable",
    ))
    .unwrap();
    let before = store.public();
    let update: ModelSettingsUpdate = serde_json::from_value(json!({
        "endpoint": "https://models.example.test/v1?API_KEY=sk-query-secret#sk-fragment-secret",
        "api_key": "sk-body-secret",
        "name": "must-not-stick"
    }))
    .unwrap();

    let update_debug = format!("{update:?}");
    assert!(!update_debug.contains("sk-query-secret"));
    assert!(!update_debug.contains("sk-fragment-secret"));
    assert!(!update_debug.contains("sk-body-secret"));

    let error = store.update(update).unwrap_err();
    assert_eq!(error, ModelSettingsError::InvalidEndpoint);
    assert_eq!(store.public(), before);
    let rendered = format!(
        "{error:?} {error} {:?} {}",
        store,
        serde_json::to_string(&store.public()).unwrap()
    );
    assert!(!rendered.contains("sk-query-secret"));
    assert!(!rendered.contains("sk-fragment-secret"));
    assert!(!rendered.contains("sk-body-secret"));

    let remote_http = ModelSettingsUpdate {
        endpoint: Some("http://models.example.test/v1".to_owned()),
        name: Some("still-must-not-stick".to_owned()),
        ..ModelSettingsUpdate::default()
    };
    assert_eq!(
        store.update(remote_http).unwrap_err(),
        ModelSettingsError::InvalidEndpoint
    );
    assert_eq!(store.public(), before);
}

#[test]
fn model_settings_update_cannot_select_a_different_environment_secret_source() {
    // Break caught: a runtime update redirects secret resolution to an attacker-chosen process env.
    let store = ModelSettingsStore::new(model_config("https://models.example.test/v1")).unwrap();
    let before = store.public();

    let error = serde_json::from_value::<ModelSettingsUpdate>(json!({
        "api_key_env": "HOME",
        "name": "must-not-stick"
    }))
    .unwrap_err();

    assert!(error.to_string().contains("unknown field"));
    assert_eq!(store.public(), before);
    assert_eq!(
        store.public().api_key_env,
        "WRITING_COACH_TEST_API_KEY_THAT_IS_NOT_SET"
    );
}

#[derive(Clone)]
struct FakeGateway {
    response: ModelResponse,
}

#[async_trait]
impl ModelGateway for FakeGateway {
    async fn complete(
        &self,
        _request: ModelRequest,
        _settings: writing_coach_server::llm::ModelCallSettings,
        _cancellation: CancellationToken,
    ) -> Result<ModelResponse, ModelError> {
        Ok(self.response.clone())
    }
}

#[tokio::test]
async fn object_safe_fake_gateway_preserves_provider_usage_fields_separately() {
    // Mutation caught: total-token-only mapping or swapping provider input/output fields.
    let gateway: Box<dyn ModelGateway> = Box::new(FakeGateway {
        response: ModelResponse {
            content: "answer".to_owned(),
            reasoning: None,
            provider: "fake".to_owned(),
            model: "fixture".to_owned(),
            usage: Usage {
                input_tokens: 37,
                output_tokens: 11,
            },
            stop_reason: Some("stop".to_owned()),
            response_id: Some("fake-response".to_owned()),
            latency_ms: 4,
        },
    });

    let response = gateway
        .complete(
            ModelRequest::from_user("hello"),
            ModelSettingsStore::new(model_config("https://models.example.test/v1"))
                .unwrap()
                .lease_for_call()
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(response.usage.input_tokens, 37);
    assert_eq!(response.usage.output_tokens, 11);
}

#[test]
fn public_settings_and_debug_never_expose_a_temporary_api_key() {
    // Mutation caught: serializing or formatting the in-memory key from any settings surface.
    let store = settings_with_temporary_key("sk-task-seven-secret");

    let json = serde_json::to_string(&store.public()).unwrap();
    let debug = format!("{store:?}");

    assert!(!json.contains("sk-task-seven-secret"));
    assert!(!debug.contains("sk-task-seven-secret"));
    assert!(store.public().api_key_configured);
    assert_eq!(
        store.resolve_secret().unwrap().unwrap().expose_secret(),
        "sk-task-seven-secret"
    );
}

#[test]
fn settings_update_is_atomic_and_keeps_secret_out_of_errors() {
    // Mutation caught: applying valid fields before rejecting an invalid field, or echoing update secrets.
    let store = settings_with_temporary_key("old-secret");
    let before = store.public();
    let error = store
        .update(ModelSettingsUpdate {
            endpoint: Some(String::new()),
            name: Some("must-not-stick".to_owned()),
            api_key: Some("new-secret-that-must-not-leak".to_owned()),
            ..ModelSettingsUpdate::default()
        })
        .unwrap_err();

    assert_eq!(store.public(), before);
    assert!(!error.to_string().contains("new-secret-that-must-not-leak"));
    assert!(!format!("{error:?}").contains("new-secret-that-must-not-leak"));
    assert_eq!(
        store.resolve_secret().unwrap().unwrap().expose_secret(),
        "old-secret"
    );
}

#[test]
fn valid_settings_update_changes_public_values() {
    // Mutation caught: retaining immutable startup snapshots after a valid settings update.
    let store = settings_with_temporary_key("temporary-secret");

    store
        .update(ModelSettingsUpdate {
            name: Some("updated-model".to_owned()),
            context_length: Some(65_536),
            input_price_microusd_per_million: Some(42),
            ..ModelSettingsUpdate::default()
        })
        .unwrap();

    let public = store.public();
    assert_eq!(public.name, "updated-model");
    assert_eq!(public.context_length, 65_536);
    assert_eq!(public.input_price_microusd_per_million, 42);
    assert!(public.api_key_configured);
}

#[test]
fn temporary_key_clearing_and_environment_fallback_are_isolated_in_child_processes() {
    const CHILD_MODE: &str = "WRITING_COACH_TASK7_CLEAR_KEY_CHILD_MODE";
    const KEY_ENV: &str = "WRITING_COACH_TASK7_ISOLATED_API_KEY";
    const FAKE_SECRET: &str = "task-seven-isolated-fake-secret";

    if let Some(mode) = std::env::var_os(CHILD_MODE) {
        let mut config = model_config("https://models.example.test/v1");
        config.api_key_env = KEY_ENV.to_owned();
        let store = ModelSettingsStore::new(config).unwrap();
        store
            .update(ModelSettingsUpdate {
                api_key: Some("temporary-secret".to_owned()),
                ..ModelSettingsUpdate::default()
            })
            .unwrap();
        assert_eq!(
            store.resolve_secret().unwrap().unwrap().expose_secret(),
            "temporary-secret"
        );

        store
            .update(ModelSettingsUpdate {
                api_key: Some(String::new()),
                ..ModelSettingsUpdate::default()
            })
            .unwrap();

        match mode.to_str().unwrap() {
            "unset" => {
                assert!(!store.public().api_key_configured);
                assert!(store.resolve_secret().unwrap().is_none());
            }
            "fallback" => {
                assert!(store.public().api_key_configured);
                assert_eq!(
                    store.resolve_secret().unwrap().unwrap().expose_secret(),
                    FAKE_SECRET
                );
            }
            other => panic!("unexpected child mode: {other}"),
        }
        return;
    }

    for (mode, fallback) in [("unset", None), ("fallback", Some(FAKE_SECRET))] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "temporary_key_clearing_and_environment_fallback_are_isolated_in_child_processes",
                "--nocapture",
            ])
            .env(CHILD_MODE, mode)
            .env_remove(KEY_ENV);
        if let Some(fallback) = fallback {
            child.env(KEY_ENV, fallback);
        }
        let output = child.output().unwrap();

        assert!(
            output.status.success(),
            "{mode} child test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[derive(Clone, Copy)]
enum ServerMode {
    Complete,
    MissingUsage,
    PartialUsage,
    ZeroUsage,
    BlankResponseId,
    OversizedResponseId,
    ControlCharacterResponseId,
    WaitForCancellation,
    SecretError,
}

#[derive(Clone)]
struct ServerState {
    mode: ServerMode,
    request: Arc<Mutex<Option<RecordedRequest>>>,
    started: Arc<Notify>,
}

#[derive(Clone, Debug)]
struct RecordedRequest {
    path_and_query: String,
    authorization: Option<String>,
    body: Value,
}

struct ModelServer {
    endpoint: String,
    request: Arc<Mutex<Option<RecordedRequest>>>,
    started: Arc<Notify>,
    task: JoinHandle<()>,
}

impl ModelServer {
    async fn spawn(mode: ServerMode) -> Self {
        let request = Arc::new(Mutex::new(None));
        let started = Arc::new(Notify::new());
        let state = ServerState {
            mode,
            request: request.clone(),
            started: started.clone(),
        };
        let app = Router::new()
            .route("/custom/v1/chat/completions", post(model_handler))
            .fallback(post(model_handler))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            endpoint: format!("http://{address}/custom/v1"),
            request,
            started,
            task,
        }
    }

    fn recorded(&self) -> RecordedRequest {
        self.request.lock().unwrap().clone().unwrap()
    }
}

impl Drop for ModelServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn model_handler(
    State(state): State<ServerState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    *state.request.lock().unwrap() = Some(RecordedRequest {
        path_and_query: uri.to_string(),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body,
    });
    state.started.notify_one();

    match state.mode {
        ServerMode::Complete => (
            StatusCode::OK,
            Json(json!({
                "id": "chatcmpl-task-seven",
                "object": "chat.completion",
                "created": 1_787_616_000_u64,
                "model": "provider-model-id",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "provider answer",
                        "reasoning_content": "provider reasoning"
                    },
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 19,
                    "completion_tokens": 5,
                    "total_tokens": 24
                }
            })),
        )
            .into_response(),
        ServerMode::MissingUsage => (
            StatusCode::OK,
            Json(model_response_with_optional_usage(None)),
        )
            .into_response(),
        ServerMode::PartialUsage => (
            StatusCode::OK,
            Json(model_response_with_optional_usage(Some(json!({
                "prompt_tokens": 19
            })))),
        )
            .into_response(),
        ServerMode::ZeroUsage => (
            StatusCode::OK,
            Json(model_response_with_optional_usage(Some(json!({
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0
            })))),
        )
            .into_response(),
        ServerMode::BlankResponseId => {
            let mut response = model_response_with_optional_usage(Some(json!({
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            })));
            response["id"] = json!("   ");
            (StatusCode::OK, Json(response)).into_response()
        }
        ServerMode::OversizedResponseId => {
            let mut response = model_response_with_optional_usage(Some(json!({
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            })));
            response["id"] = json!("x".repeat(257));
            (StatusCode::OK, Json(response)).into_response()
        }
        ServerMode::ControlCharacterResponseId => {
            let mut response = model_response_with_optional_usage(Some(json!({
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            })));
            response["id"] = json!("chatcmpl-\u{0007}-control");
            (StatusCode::OK, Json(response)).into_response()
        }
        ServerMode::SecretError => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "rejected sk-transport-secret"}})),
        )
            .into_response(),
        ServerMode::WaitForCancellation => std::future::pending::<Response>().await,
    }
}

fn model_response_with_optional_usage(usage: Option<Value>) -> Value {
    let mut response = json!({
        "id": "chatcmpl-usage-edge",
        "object": "chat.completion",
        "created": 1_787_616_000_u64,
        "model": "provider-model-id",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": "provider answer"},
            "finish_reason": "stop"
        }]
    });
    if let Some(usage) = usage {
        response["usage"] = usage;
    }
    response
}

#[tokio::test]
async fn genai_transport_uses_custom_endpoint_and_maps_full_provider_response() {
    // Mutations caught: default-provider endpoint use, text-only mapping, or collapsed/swapped usage.
    let server = ModelServer::spawn(ServerMode::Complete).await;
    let endpoint = format!("{}?api-version=task-seven", server.endpoint);
    let gateway = gateway_for(&endpoint, "sk-transport-secret");

    let response = gateway
        .complete(
            ModelRequest {
                messages: vec![
                    ModelMessage::system("Be concise"),
                    ModelMessage::user("Explain the evidence"),
                ],
                temperature: Some(0.2),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(response.content, "provider answer");
    assert_eq!(response.reasoning.as_deref(), Some("provider reasoning"));
    assert_eq!(response.provider, "openai");
    assert_eq!(response.model, "provider-model-id");
    assert_eq!(response.usage.input_tokens, 19);
    assert_eq!(response.usage.output_tokens, 5);
    assert_eq!(response.stop_reason.as_deref(), Some("stop"));
    assert_eq!(response.response_id.as_deref(), Some("chatcmpl-task-seven"));

    let request = server.recorded();
    assert_eq!(
        request.path_and_query,
        "/custom/v1/chat/completions?api-version=task-seven"
    );
    assert_eq!(
        request.authorization.as_deref(),
        Some("Bearer sk-transport-secret")
    );
    assert_eq!(request.body["model"], "configured-model");
    assert_eq!(request.body["max_tokens"], 512);
}

#[tokio::test]
async fn genai_transport_cancellation_wins_while_provider_is_pending() {
    // Mutation caught: awaiting the provider future without selecting on cancellation.
    let server = ModelServer::spawn(ServerMode::WaitForCancellation).await;
    let gateway = gateway_for(&server.endpoint, "sk-transport-secret");
    let cancellation = CancellationToken::new();
    let call = tokio::spawn({
        let cancellation = cancellation.clone();
        async move {
            gateway
                .complete(ModelRequest::from_user("wait"), cancellation)
                .await
        }
    });

    tokio::time::timeout(Duration::from_secs(5), server.started.notified())
        .await
        .expect("provider request reached the local server");
    cancellation.cancel();

    let result = tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .expect("cancelled provider call completed")
        .unwrap();
    assert!(matches!(result, Err(ModelError::Cancelled)));
}

#[tokio::test]
async fn provider_error_does_not_expose_key_or_response_body() {
    // Mutation caught: forwarding genai/debug response details into user-visible errors.
    let server = ModelServer::spawn(ServerMode::SecretError).await;
    let gateway = gateway_for(&server.endpoint, "sk-transport-secret");

    let error = gateway
        .complete(
            ModelRequest::from_user("fail safely"),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    let rendered = format!("{error:?} {error}");

    assert!(!rendered.contains("sk-transport-secret"));
    assert!(!rendered.contains("rejected"));
}

#[tokio::test]
async fn missing_or_partial_provider_usage_is_rejected_instead_of_counted_as_zero() {
    // Mutation caught: treating an absent provider counter as a measured zero-token call.
    for mode in [ServerMode::MissingUsage, ServerMode::PartialUsage] {
        let server = ModelServer::spawn(mode).await;
        let gateway = gateway_for(&server.endpoint, "sk-transport-secret");

        let error = gateway
            .complete(
                ModelRequest::from_user("require exact usage"),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();

        assert_eq!(error, ModelError::InvalidResponse);
        assert_eq!(
            error.to_string(),
            "model provider returned an invalid response"
        );
    }
}

#[tokio::test]
async fn provider_reported_zero_usage_remains_a_valid_measured_zero() {
    // Mutation caught: rejecting a present zero counter together with a missing counter.
    let server = ModelServer::spawn(ServerMode::ZeroUsage).await;
    let gateway = gateway_for(&server.endpoint, "sk-transport-secret");

    let response = gateway
        .complete(
            ModelRequest::from_user("zero is still reported"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(response.usage.input_tokens, 0);
    assert_eq!(response.usage.output_tokens, 0);
}

#[tokio::test]
async fn invalid_raw_response_ids_are_dropped_before_persistence_boundaries() {
    // Mutations caught: retaining blank, unbounded, or control-bearing provider identifiers.
    for mode in [
        ServerMode::BlankResponseId,
        ServerMode::OversizedResponseId,
        ServerMode::ControlCharacterResponseId,
    ] {
        let server = ModelServer::spawn(mode).await;
        let gateway = gateway_for(&server.endpoint, "sk-transport-secret");

        let response = gateway
            .complete(
                ModelRequest::from_user("validate response id"),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(response.response_id.is_none());
    }
}

#[tokio::test]
async fn deepseek_openai_compatible_transport_keeps_a_valid_raw_response_id() {
    // Mutation caught: removing the verified DeepSeek fallback together with unverified adapters.
    let server = ModelServer::spawn(ServerMode::Complete).await;
    let gateway = gateway_for_provider(&server.endpoint, "sk-transport-secret", "deepseek");

    let response = gateway
        .complete(
            ModelRequest::from_user("keep valid response id"),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(response.provider, "deepseek");
    assert_eq!(response.response_id.as_deref(), Some("chatcmpl-task-seven"));
}

fn settings_with_temporary_key(key: &str) -> ModelSettingsStore {
    let store = ModelSettingsStore::new_with_run_defaults(
        model_config("http://127.0.0.1:1/v1"),
        test_run_defaults(),
    )
    .unwrap();
    store
        .update(ModelSettingsUpdate {
            api_key: Some(key.to_owned()),
            ..ModelSettingsUpdate::default()
        })
        .unwrap();
    store
}

fn gateway_for(endpoint: &str, key: &str) -> GenaiModelGateway {
    gateway_for_provider(endpoint, key, "openai")
}

fn gateway_for_provider(endpoint: &str, key: &str, provider: &str) -> GenaiModelGateway {
    let mut config = model_config(endpoint);
    config.provider = provider.to_owned();
    let store = Arc::new(ModelSettingsStore::new(config).unwrap());
    store
        .update(ModelSettingsUpdate {
            api_key: Some(key.to_owned()),
            ..ModelSettingsUpdate::default()
        })
        .unwrap();
    GenaiModelGateway::new(store)
}

fn model_config(endpoint: &str) -> ModelConfig {
    ModelConfig {
        provider: "openai".to_owned(),
        endpoint: endpoint.to_owned(),
        name: "configured-model".to_owned(),
        api_key_env: "WRITING_COACH_TEST_API_KEY_THAT_IS_NOT_SET".to_owned(),
        context_length: 8_192,
        max_output_tokens: 512,
        reasoning_mode: "medium".to_owned(),
        input_price_microusd_per_million: 10,
        output_price_microusd_per_million: 20,
    }
}

fn test_run_defaults() -> RunDefaults {
    RunDefaults {
        max_input_tokens: 8_000,
        max_output_tokens: 512,
        max_cost_microusd: 5_000_000,
    }
}
