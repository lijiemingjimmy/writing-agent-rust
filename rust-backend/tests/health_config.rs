use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use writing_coach_server::{
    AppConfig,
    tools::{KnowledgePlan, PlannedSearch, SearchRequest},
};

const VALID_TEST_CONFIG: &str = r#"
bind_addr = "127.0.0.1:0"
database_url = "sqlite::memory:"
skill_root = "../skills"
corpus_root = "../corpus"

[model]
provider = "openai-compatible"
endpoint = "http://127.0.0.1:1234/v1"
name = "local-writing-model"
api_key_env = "WRITING_COACH_TEST_API_KEY"
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

#[tokio::test]
async fn health_returns_rust_service_identity() {
    let config = AppConfig::from_toml(VALID_TEST_CONFIG).unwrap();
    let app = writing_coach_server::build_app(config).await.unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"status":"ok","service":"writing-coach-rust"})
    );
}

#[test]
fn config_rejects_zero_context_length() {
    let invalid = VALID_TEST_CONFIG.replace("context_length = 32768", "context_length = 0");
    let error = AppConfig::from_toml(&invalid).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("context_length must be greater than zero")
    );
}

#[test]
fn config_rejects_unknown_knowledge_provider_names() {
    for section in [
        "[knowledge.web]\nproviders = [\"searxng\", \"search_typo\"]",
        "[knowledge.scholarly]\nproviders = [\"crossref\", \"papers_typo\"]",
    ] {
        let source = format!("{VALID_TEST_CONFIG}\n{section}\n");
        let error = AppConfig::from_toml(&source).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid configuration: unknown knowledge provider"
        );
    }
}

#[test]
fn config_rejects_cors_values_that_are_not_http_origins() {
    for origin in [
        "https://coach.example/",
        "https://coach.example/student",
        "https://user:password@coach.example",
        "file:///tmp/student.html",
    ] {
        let invalid = VALID_TEST_CONFIG.replace(
            "corpus_root = \"../corpus\"",
            &format!("corpus_root = \"../corpus\"\ncors_allowed_origins = [\"{origin}\"]"),
        );
        assert!(AppConfig::from_toml(&invalid).is_err(), "accepted {origin}");
    }
}

#[tokio::test]
async fn configured_rust_cors_allows_only_the_pages_origin_and_student_methods() {
    let source = VALID_TEST_CONFIG.replace(
        "corpus_root = \"../corpus\"",
        "corpus_root = \"../corpus\"\ncors_allowed_origins = [\"https://coach.example\"]",
    );
    let app = writing_coach_server::build_app(AppConfig::from_toml(&source).unwrap())
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/runs")
                .header(header::ORIGIN, "https://coach.example")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://coach.example"
    );
    assert!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_METHODS]
            .to_str()
            .unwrap()
            .contains("POST")
    );

    let untrusted = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(header::ORIGIN, "https://untrusted.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        untrusted
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none()
    );
}

#[tokio::test]
async fn knowledge_defaults_disabled_and_configured_web_provider_executes_through_builder() {
    // Break caught: build_app always installs no external tools or silently enables defaults.
    let disabled = AppConfig::from_toml(VALID_TEST_CONFIG).unwrap();
    let (coordinator, web_enabled) = disabled.build_knowledge_coordinator().unwrap();
    assert!(!web_enabled);
    assert!(!coordinator.has_tool("web"));
    assert!(!coordinator.has_tool("scholarly"));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let count = stream.read(&mut request).await.unwrap();
        assert!(String::from_utf8_lossy(&request[..count]).starts_with("GET /search?"));
        let body = r#"{"results":[{"title":"Configured result","url":"https://example.test/source","content":"grounded","engine":"fake"}]}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(), body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let source = format!(
        "{VALID_TEST_CONFIG}\n[knowledge.web]\nproviders = [\"searxng\"]\nsearxng_base_url = \"http://{address}\"\nmax_results = 3\n"
    );
    let configured = AppConfig::from_toml(&source).unwrap();
    let rendered = format!("{configured:?}");
    assert!(!rendered.contains("api_key ="));
    let (coordinator, web_enabled) = configured.build_knowledge_coordinator().unwrap();
    assert!(web_enabled);
    assert!(coordinator.has_tool("web"));
    let result = coordinator
        .execute(
            KnowledgePlan::new([PlannedSearch::new("web", SearchRequest::new("writing"))]),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].title, "Configured result");
    server.await.unwrap();
}
