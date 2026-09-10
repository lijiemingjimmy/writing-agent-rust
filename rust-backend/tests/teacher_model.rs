use axum::{
    Json, Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
    routing::post,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use writing_coach_server::{
    AppConfig,
    store::{
        sessions::{MessageRepository, SessionRepository},
        sqlite,
    },
};

const TEST_CONFIG: &str = r#"
bind_addr = "127.0.0.1:0"
database_url = "sqlite::memory:"
skill_root = "../skills"
corpus_root = "../corpus"

[security]
teacher_access_token = "teacher-test-token"

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

async fn request(app: &Router, method: &str, path: &str, payload: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .header("x-teacher-token", "teacher-test-token")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn fixture() -> (Router, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("teacher-model-{}.db", uuid::Uuid::new_v4()));
    let mut config = AppConfig::from_toml(TEST_CONFIG).unwrap();
    config.database_url = format!("sqlite://{}", path.display());
    let pool = sqlite::open_database(&config.database_url).await.unwrap();
    sqlite::migrate(&pool).await.unwrap();
    let session = SessionRepository::new(pool.clone())
        .create(Some("student-example"))
        .await
        .unwrap();
    MessageRepository::new(pool.clone())
        .add(
            session.id,
            "user",
            "我的选题太宽了，不知道怎样缩小研究问题",
            None,
        )
        .await
        .unwrap();
    pool.close().await;
    (writing_coach_server::build_app(config).await.unwrap(), path)
}

#[tokio::test]
async fn teacher_uses_student_saved_model_and_real_evidence() {
    // Catches the fixed-template response, disconnected settings, and Chinese whole-sentence matching.
    let provider = Router::new().route("/v1/chat/completions", post(|headers: HeaderMap, Json(body): Json<Value>| async move {
        assert_eq!(headers["authorization"], "Bearer test-shared-key");
        assert_eq!(body["model"], "teacher-test-model");
        let prompt = body["messages"].to_string();
        assert!(prompt.contains("我的选题太宽了"));
        assert!(prompt.contains("我下次上课大概需要讲什么东西"));
        Json(json!({"id":"test","object":"chat.completion","created":1,"model":"teacher-test-model",
            "choices":[{"index":0,"message":{"role":"assistant","content":"下次课先讲如何收窄研究问题，再用学生选题做课堂练习。"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, provider).await.unwrap();
    });
    let (app, path) = fixture().await;
    let (status, settings) = request(
        &app,
        "PUT",
        "/api/settings/model",
        json!({"endpoint":endpoint,"name":"teacher-test-model","api_key":"test-shared-key"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["api_key_configured"], true);
    assert!(!settings.to_string().contains("test-shared-key"));
    let (status, result) = request(
        &app,
        "POST",
        "/api/teacher/ask",
        json!({"question":"我下次上课大概需要讲什么东西？"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        result["answer"],
        "下次课先讲如何收窄研究问题，再用学生选题做课堂练习。"
    );
    assert_eq!(result["evidence"].as_array().unwrap().len(), 1);
    assert!(!result.to_string().contains("test-shared-key"));
    server.abort();
    drop(app);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn teacher_reports_missing_shared_key_instead_of_template() {
    let (app, path) = fixture().await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/teacher/ask",
        json!({"question":"下次课讲什么"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("API Key"));
    drop(app);
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn teacher_rejects_empty_question() {
    let (app, path) = fixture().await;
    let (status, _) = request(&app, "POST", "/api/teacher/ask", json!({"question":"  "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    drop(app);
    let _ = std::fs::remove_file(path);
}
