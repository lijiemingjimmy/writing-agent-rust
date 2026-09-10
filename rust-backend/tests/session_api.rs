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
    assert!(
        !documents
            .list_chunks_by_session(imported.id)
            .await
            .unwrap()
            .is_empty()
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
    assert_eq!(uploaded["index_status"], "ready");
    assert!(uploaded["chunk_count"].as_u64().unwrap() >= 1);
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

    let listed = get_json(
        app.clone(),
        &format!("/api/sessions/{session_id}/documents"),
        &token,
    )
    .await;
    assert_eq!(listed["documents"].as_array().unwrap().len(), 1);
    assert_eq!(listed["documents"][0]["filename"], "field-notes.md");
    assert_eq!(listed["documents"][0]["index_status"], "ready");
    assert!(listed["documents"][0]["chunk_count"].as_u64().unwrap() >= 1);

    let delete_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/api/sessions/{session_id}/documents/{}",
                    uploaded["document_id"].as_str().unwrap()
                ))
                .header(AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

    let listed_after_delete = get_json(
        app.clone(),
        &format!("/api/sessions/{session_id}/documents"),
        &token,
    )
    .await;
    assert!(
        listed_after_delete["documents"]
            .as_array()
            .unwrap()
            .is_empty()
    );

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

#[tokio::test]
async fn a_new_login_cannot_read_or_delete_another_principals_session_documents() {
    let (app, database_path, owner_token) = app_with_session("2025010468", "我的选题").await;
    let sessions = get_json(
        app.clone(),
        "/api/sessions?user_id=2025010468",
        &owner_token,
    )
    .await;
    let session_id = sessions["sessions"][0]["session_id"].as_str().unwrap();
    let upload = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/sessions/{session_id}/documents?filename=private.md"
                ))
                .header(AUTHORIZATION, bearer(&owner_token))
                .header(CONTENT_TYPE, "text/markdown")
                .body(Body::from("# 私有资料\n不能被同名登录继承"))
                .unwrap(),
        )
        .await
        .unwrap();
    let uploaded: Value =
        serde_json::from_slice(&upload.into_body().collect().await.unwrap().to_bytes()).unwrap();

    let bootstrap = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/student/access/bootstrap")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"student_name":"2025010468","student_id":"2025010468"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bootstrap.status(), StatusCode::CREATED);
    let bootstrap_body = bootstrap.into_body().collect().await.unwrap().to_bytes();
    let other_access: Value = serde_json::from_slice(&bootstrap_body).unwrap();
    let other_token = other_access["access_token"].as_str().unwrap();

    for request in [
        Request::builder()
            .uri(format!("/api/sessions/{session_id}/documents"))
            .header(AUTHORIZATION, bearer(other_token))
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .method("DELETE")
            .uri(format!(
                "/api/sessions/{session_id}/documents/{}",
                uploaded["document_id"].as_str().unwrap()
            ))
            .header(AUTHORIZATION, bearer(other_token))
            .body(Body::empty())
            .unwrap(),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

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

#[tokio::test]
async fn revise_latest_prompt_forks_an_owned_prefix_without_changing_the_original() {
    let (app, database, token) = app_with_session("revision-student", "旧的第一轮问题").await;
    let sessions = get_json(app.clone(), "/api/sessions", &token).await;
    let id = sessions["sessions"][0]["session_id"].as_str().unwrap();
    let messages = get_json(app.clone(), &format!("/api/sessions/{id}/messages"), &token).await;
    let message_id = messages["messages"][0]["id"].as_str().unwrap();
    let uri = format!("/api/sessions/{id}/messages/{message_id}/fork");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header(AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let fork: Value = serde_json::from_slice(&body).unwrap();
    let new_id = fork["session_id"].as_str().unwrap();
    assert_ne!(new_id, id);
    let prefix = get_json(
        app.clone(),
        &format!("/api/sessions/{new_id}/messages"),
        &token,
    )
    .await;
    assert_eq!(prefix["messages"].as_array().unwrap().len(), 0);
    let original = get_json(app.clone(), &format!("/api/sessions/{id}/messages"), &token).await;
    assert_eq!(original["messages"][0]["content"], "旧的第一轮问题");
    let unauth = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauth.status(), StatusCode::UNAUTHORIZED);
    remove_database(&database);
}

#[tokio::test]
async fn revision_restores_checkpoint_and_documents_and_rejects_stale_or_foreign_turns() {
    let (app, database, token) = app_with_session("revision-owner", "保留的前文").await;
    let listed = get_json(app.clone(), "/api/sessions", &token).await;
    let id = listed["sessions"][0]["session_id"].as_str().unwrap();
    let session_id = SessionId::parse_legacy(id).unwrap();
    let pool = sqlite::open_database(&format!("sqlite://{}", database.display()))
        .await
        .unwrap();
    let repo = SessionRepository::new(pool.clone());
    let messages = MessageRepository::new(pool.clone());
    let initial = messages.list_by_session(session_id).await.unwrap();
    messages
        .add(session_id, "assistant", "保留的回答", None)
        .await
        .unwrap();
    let before = json!({"student_profile":{"name":"revision-owner","student_id":"revision-owner"},
        "writing_context":{"stage":"evidence","topic":"原先确认的主题"},
        "conversation_memory_summary":"只包含前文"});
    let target = messages
        .add(
            session_id,
            "user",
            "需要修改的问题",
            Some(json!({"state_before_turn":before})),
        )
        .await
        .unwrap();
    messages
        .add(session_id, "assistant", "不应再使用的旧答案", None)
        .await
        .unwrap();
    repo.save_state(session_id, json!({"writing_context":{"topic":"错误的新主题"},"conversation_memory_summary":"不应再使用的旧摘要"})).await.unwrap();
    let documents = DocumentRepository::new(pool.clone());
    documents
        .add(
            session_id,
            "材料.md",
            "text/markdown",
            None,
            Some("# 证据\n合作分工的观察资料"),
            None,
        )
        .await
        .unwrap();
    let uri = format!(
        "/api/sessions/{id}/messages/{}/fork",
        target.id.to_legacy_hex()
    );
    let post = |uri: String, token: &str| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(AUTHORIZATION, bearer(token))
            .body(Body::empty())
            .unwrap()
    };
    let stale = app
        .clone()
        .oneshot(post(
            format!(
                "/api/sessions/{id}/messages/{}/fork",
                initial[0].id.to_legacy_hex()
            ),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::BAD_REQUEST);
    let (other, _) = StudentAccessRepository::new(pool.clone())
        .bootstrap("另一个学生", "another-student")
        .await
        .unwrap();
    let foreign = app
        .clone()
        .oneshot(post(uri.clone(), &other))
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
    let response = app
        .clone()
        .oneshot(post(uri.clone(), &token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let fork: Value = serde_json::from_slice(&body).unwrap();
    let new_id = SessionId::parse_legacy(fork["session_id"].as_str().unwrap()).unwrap();
    let prefix = messages.list_by_session(new_id).await.unwrap();
    assert_eq!(
        prefix
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["保留的前文", "保留的回答"]
    );
    assert_eq!(
        repo.load_state(new_id).await.unwrap().state_json["writing_context"]["topic"],
        "原先确认的主题"
    );
    assert_eq!(
        repo.load_state(new_id).await.unwrap().state_json["conversation_memory_summary"],
        "只包含前文"
    );
    assert_eq!(repo.get(new_id).await.unwrap().stage, "evidence");
    assert_eq!(
        documents.list_by_session(new_id).await.unwrap()[0].filename,
        "材料.md"
    );
    assert!(
        !documents
            .list_chunks_by_session(new_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(messages.list_by_session(session_id).await.unwrap().len(), 4);
    let exported = repo.export_v1(new_id).await.unwrap();
    repo.import_v1(exported).await.unwrap();
    writing_coach_server::store::runs::RunRepository::new(pool.clone())
        .create(session_id, 3, None, None)
        .await
        .unwrap();
    let active = app.oneshot(post(uri, &token)).await.unwrap();
    assert_eq!(active.status(), StatusCode::CONFLICT);
    pool.close().await;
    remove_database(&database);
}

#[sqlx::test]
async fn user_message_metadata_updates_preserve_the_pre_turn_checkpoint(pool: SqlitePool) {
    let session = SessionRepository::new(pool.clone())
        .create(None)
        .await
        .unwrap();
    let messages = MessageRepository::new(pool);
    let message = messages
        .add(
            session.id,
            "user",
            "问题",
            Some(json!({"state_before_turn":{"stage":"NEW"}})),
        )
        .await
        .unwrap();
    let updated = messages
        .update_metadata(message.id, json!({"intent":"course_qa"}))
        .await
        .unwrap();
    assert_eq!(updated.metadata_json["state_before_turn"]["stage"], "NEW");
    assert_eq!(updated.metadata_json["intent"], "course_qa");
}

#[tokio::test]
async fn legacy_revision_recovers_skill_and_slots_from_only_the_retained_prefix() {
    let (app, database, token) = app_with_session(
        "legacy-revision",
        &"这是我的初稿：小组合作需要明确责任边界。".repeat(8),
    )
    .await;
    let listed = get_json(app.clone(), "/api/sessions", &token).await;
    let id = listed["sessions"][0]["session_id"].as_str().unwrap();
    let sid = SessionId::parse_legacy(id).unwrap();
    let pool = sqlite::open_database(&format!("sqlite://{}", database.display()))
        .await
        .unwrap();
    let messages = MessageRepository::new(pool.clone());
    let first = messages.list_by_session(sid).await.unwrap().remove(0);
    messages
        .update_metadata(first.id, json!({"skill_id":"writing_feedback"}))
        .await
        .unwrap();
    messages.add(sid,"assistant","这篇文章的目标读者是谁？",Some(json!({
        "skill_id":"writing_feedback","awaiting_slots":["audience"],
        "student_progress":{"topic":"小组合作","current_skill":"writing_feedback","stage":"DRAFTING"},
        "branch":{"mode":"locked","active_skill":"writing_feedback"}
    }))).await.unwrap();
    let target = messages.add(sid, "user", "错误的受众", None).await.unwrap();
    SessionRepository::new(pool.clone()).save_state(sid,json!({"current_skill":"material_search","collected_slots":{"audience":"错误的受众"},"conversation_memory_summary":"错误的旧摘要"})).await.unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/sessions/{id}/messages/{}/fork",
                    target.id.to_legacy_hex()
                ))
                .header(AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let fork: Value = serde_json::from_slice(&body).unwrap();
    let state = SessionRepository::new(pool.clone())
        .load_state(SessionId::parse_legacy(fork["session_id"].as_str().unwrap()).unwrap())
        .await
        .unwrap()
        .state_json;
    assert_eq!(state["current_skill"], "writing_feedback");
    assert_eq!(state["awaiting_slots"], json!(["audience"]));
    assert!(
        state["collected_slots"]["draft_text"]
            .as_str()
            .unwrap()
            .contains("小组合作")
    );
    assert!(state["collected_slots"].get("audience").is_none());
    assert!(state.get("conversation_memory_summary").is_none());
    assert_eq!(state["writing_context"]["topic"], "小组合作");
    pool.close().await;
    remove_database(&database);
}

#[tokio::test]
async fn revision_does_not_load_or_duplicate_large_execution_logs() {
    use writing_coach_server::{domain::RunStatus, store::runs::RunRepository};
    let (app, database, token) = app_with_session("large-run-log", "你好").await;
    let listed = get_json(app.clone(), "/api/sessions", &token).await;
    let id = listed["sessions"][0]["session_id"].as_str().unwrap();
    let sid = SessionId::parse_legacy(id).unwrap();
    let pool = sqlite::open_database(&format!("sqlite://{}", database.display()))
        .await
        .unwrap();
    let repo = RunRepository::new(pool.clone());
    let run = repo.create(sid, 3, None, None).await.unwrap();
    repo.mark_running(run.id).await.unwrap();
    repo.append_event(
        run.id,
        "step.completed",
        json!({"trace":"x".repeat(1_500_000)}),
        None,
    )
    .await
    .unwrap();
    repo.finish(run.id, RunStatus::Completed, None, json!({"answer":"你好"}))
        .await
        .unwrap();
    assert!(
        SessionRepository::new(pool.clone())
            .export_v1(sid)
            .await
            .is_err()
    );
    let target = MessageRepository::new(pool.clone())
        .list_by_session(sid)
        .await
        .unwrap()
        .remove(0);
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/sessions/{id}/messages/{}/fork",
                    target.id.to_legacy_hex()
                ))
                .header(AUTHORIZATION, bearer(&token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    pool.close().await;
    remove_database(&database);
}
