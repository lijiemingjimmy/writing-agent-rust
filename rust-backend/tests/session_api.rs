use std::{fs, path::PathBuf};

use axum::{
    Router,
    body::Body,
    http::{
        Request, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    },
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;
use uuid::Uuid;
use writing_coach_server::{
    AppConfig,
    domain::SessionId,
    store::{
        access::StudentAccessRepository,
        sessions::{
            DocumentRepository, MessageRepository, SessionRepository, SkillEventRepository,
        },
        sqlite,
    },
};

const TEST_CONFIG: &str = r#"
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

#[test]
fn default_session_id_uses_legacy_storage_format() {
    assert_eq!(SessionId::default().to_legacy_hex().len(), 32);
}

#[sqlx::test]
async fn save_state_prefers_writing_context_stage_then_top_level_then_existing(pool: SqlitePool) {
    let repo = SessionRepository::new(pool);
    let session = repo.create(Some("student-1")).await.unwrap();

    repo.save_state(
        session.id,
        json!({"stage": "top-level", "writing_context": {"stage": "evidence"}}),
    )
    .await
    .unwrap();
    assert_eq!(repo.get(session.id).await.unwrap().stage, "evidence");

    repo.save_state(session.id, json!({"stage": "revision"}))
        .await
        .unwrap();
    assert_eq!(repo.get(session.id).await.unwrap().stage, "revision");

    repo.save_state(session.id, json!({"unknown": {"preserved": true}}))
        .await
        .unwrap();
    assert_eq!(repo.get(session.id).await.unwrap().stage, "revision");
    assert_eq!(
        repo.load_state(session.id).await.unwrap().state_json,
        json!({"unknown": {"preserved": true}})
    );
}

#[sqlx::test]
async fn repositories_preserve_legacy_json_and_import_under_new_ids(pool: SqlitePool) {
    let sessions = SessionRepository::new(pool.clone());
    let messages = MessageRepository::new(pool.clone());
    let documents = DocumentRepository::new(pool.clone());
    let events = SkillEventRepository::new(pool);
    let session = sessions.create(Some("student-1")).await.unwrap();

    messages
        .add(
            session.id,
            "user",
            "我的选题",
            Some(json!({"source": "student", "unknown": [1, 2]})),
        )
        .await
        .unwrap();
    documents
        .add(
            session.id,
            "notes.md",
            "text/markdown",
            Some("/tmp/notes.md"),
            Some("材料原文"),
            Some(json!({"tag": "evidence"})),
        )
        .await
        .unwrap();
    events
        .add(
            session.id,
            "topic_selection",
            "completed",
            Some(json!({"reason": "student choice"})),
        )
        .await
        .unwrap();
    sessions
        .save_state(
            session.id,
            json!({"writing_context": {"stage": "evidence"}}),
        )
        .await
        .unwrap();

    let exported = sessions.export_v1(session.id).await.unwrap();
    let imported = sessions.import_v1(exported).await.unwrap();

    assert_ne!(imported.id, session.id);
    assert_eq!(imported.user_id.as_deref(), Some("student-1"));
    assert_eq!(imported.stage, "evidence");
    assert_eq!(
        messages.list_by_session(imported.id).await.unwrap()[0].metadata_json,
        json!({"source": "student", "unknown": [1, 2]})
    );
    assert_eq!(
        documents.list_by_session(imported.id).await.unwrap()[0].metadata_json,
        json!({"tag": "evidence"})
    );
    assert_eq!(
        events.list_by_session(imported.id).await.unwrap()[0].metadata_json,
        json!({"reason": "student choice"})
    );
}

#[tokio::test]
async fn session_history_matches_existing_frontend_contract() {
    let (app, database_path, token) = app_with_session("student-1", "我的选题").await;

    let sessions = get_json(app.clone(), "/api/sessions?user_id=student-1", &token).await;
    let item = &sessions["sessions"][0];
    assert_eq!(item["session_id"].as_str().unwrap().len(), 32);
    assert_eq!(item["user_id"], "student-1");
    assert!(item["task_type"].is_null());
    assert_eq!(item["stage"], "NEW");
    assert_eq!(item["preview"], "我的选题");
    assert_eq!(item["message_count"], 1);
    assert!(item["created_at"].is_string());
    assert!(item["updated_at"].is_string());

    let session_id = item["session_id"].as_str().unwrap();
    let messages = get_json(
        app.clone(),
        &format!("/api/sessions/{session_id}/messages"),
        &token,
    )
    .await;
    assert_eq!(messages["messages"][0]["id"].as_str().unwrap().len(), 32);
    assert_eq!(messages["messages"][0]["session_id"], session_id);
    assert_eq!(messages["messages"][0]["role"], "user");
    assert_eq!(messages["messages"][0]["content"], "我的选题");
    assert_eq!(
        messages["messages"][0]["metadata_json"],
        json!({"kind": "prompt"})
    );

    drop(app);
    remove_database(&database_path);
}

#[tokio::test]
async fn session_list_exposes_student_profile_from_preserved_state() {
    let (app, database_path, token) = app_with_session_and_state(
        "20260001",
        "我的选题",
        json!({"student_profile": {"name": "张三", "student_id": "20260001"}}),
    )
    .await;

    let sessions = get_json(app, "/api/sessions?user_id=20260001", &token).await;
    let item = &sessions["sessions"][0];
    assert_eq!(item["student_name"], "张三");
    assert_eq!(item["student_id"], "20260001");

    remove_database(&database_path);
}

#[tokio::test]
async fn missing_session_history_is_hidden_from_authenticated_student() {
    let (app, database_path, token) = app_with_session("student-1", "我的选题").await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/sessions/0123456789abcdef0123456789abcdef/messages")
                .header(AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    remove_database(&database_path);
}

#[tokio::test]
async fn upload_text_document_stores_bounded_utf8_without_a_server_path() {
    let (app, database_path, token) = app_with_session("student-1", "我的选题").await;
    let sessions = get_json(app.clone(), "/api/sessions?user_id=student-1", &token).await;
    let session_id = sessions["sessions"][0]["session_id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/sessions/{session_id}/documents?filename=field-notes.md"
                ))
                .header(AUTHORIZATION, bearer(&token))
                .header(CONTENT_TYPE, "text/markdown")
                .body(Body::from("# 访谈\r\n\r\n学生观察记录"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let uploaded: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(uploaded["document_id"].as_str().unwrap().len(), 32);
    assert!(
        uploaded["document_id"]
            .as_str()
            .unwrap()
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character))
    );
    assert_eq!(uploaded["session_id"], session_id);
    assert_eq!(uploaded["filename"], "field-notes.md");
    assert_eq!(uploaded["content_type"], "text/markdown");
    assert_eq!(uploaded["size_bytes"], 30);

    let export = get_json(
        app.clone(),
        &format!("/api/sessions/{session_id}/export"),
        &token,
    )
    .await;
    let document = &export["documents"][0];
    assert_eq!(document["raw_path"], Value::Null);
    assert_eq!(document["parsed_text"], "# 访谈\n\n学生观察记录");
    assert_eq!(document["metadata_json"]["source"], "student_upload");
    assert_eq!(document["metadata_json"]["size_bytes"], 30);

    drop(app);
    remove_database(&database_path);
}

#[tokio::test]
async fn upload_text_document_rejects_paths_types_invalid_utf8_and_oversize_without_rows() {
    let (app, database_path, token) = app_with_session("student-1", "我的选题").await;
    let sessions = get_json(app.clone(), "/api/sessions?user_id=student-1", &token).await;
    let session_id = sessions["sessions"][0]["session_id"].as_str().unwrap();
    let cases = [
        (
            "..%2Fsecret.md",
            "text/markdown",
            vec![b'x'],
            StatusCode::BAD_REQUEST,
        ),
        (
            "notes.pdf",
            "text/plain",
            vec![b'x'],
            StatusCode::BAD_REQUEST,
        ),
        (
            "notes.md",
            "text/plain",
            vec![b'x'],
            StatusCode::BAD_REQUEST,
        ),
        (
            "notes.txt",
            "text/plain",
            vec![0xff, 0xfe],
            StatusCode::BAD_REQUEST,
        ),
    ];
    for (filename, content_type, bytes, expected) in cases {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/sessions/{session_id}/documents?filename={filename}"
                    ))
                    .header(AUTHORIZATION, bearer(&token))
                    .header(CONTENT_TYPE, content_type)
                    .body(Body::from(bytes))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "filename={filename}");
    }

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/sessions/{session_id}/documents?filename=oversize.txt"
                ))
                .header(AUTHORIZATION, bearer(&token))
                .header(CONTENT_TYPE, "text/plain")
                .body(Body::from(vec![b'x'; 256 * 1024 + 1]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let export = get_json(
        app.clone(),
        &format!("/api/sessions/{session_id}/export"),
        &token,
    )
    .await;
    assert!(export["documents"].as_array().unwrap().is_empty());

    drop(app);
    remove_database(&database_path);
}

async fn app_with_session(user_id: &str, content: &str) -> (Router, PathBuf, String) {
    app_with_session_and_state(user_id, content, json!({})).await
}

async fn app_with_session_and_state(
    user_id: &str,
    content: &str,
    state: Value,
) -> (Router, PathBuf, String) {
    let database_path =
        std::env::temp_dir().join(format!("writing-coach-session-api-{}.db", Uuid::new_v4()));
    let database_url = format!("sqlite://{}", database_path.display());
    let pool = sqlite::open_database(&database_url).await.unwrap();
    sqlite::migrate(&pool).await.unwrap();

    let sessions = SessionRepository::new(pool.clone());
    let session = sessions.create(Some(user_id)).await.unwrap();
    let (token, principal) = StudentAccessRepository::new(pool.clone())
        .bootstrap(user_id, user_id)
        .await
        .unwrap();
    StudentAccessRepository::new(pool.clone())
        .bind_session(session.id, &principal.id)
        .await
        .unwrap();
    sessions.save_state(session.id, state).await.unwrap();
    MessageRepository::new(pool.clone())
        .add(session.id, "user", content, Some(json!({"kind": "prompt"})))
        .await
        .unwrap();
    pool.close().await;

    let mut config = AppConfig::from_toml(TEST_CONFIG).unwrap();
    config.database_url = database_url;
    (
        writing_coach_server::build_app(config).await.unwrap(),
        database_path,
        token,
    )
}

async fn get_json(app: Router, uri: &str, token: &str) -> Value {
    let response = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(AUTHORIZATION, bearer(token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn remove_database(database_path: &PathBuf) {
    let _ = fs::remove_file(database_path);
    let _ = fs::remove_file(database_path.with_extension("db-shm"));
    let _ = fs::remove_file(database_path.with_extension("db-wal"));
}
