use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    routing::{get, post},
};
use futures_util::stream;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    AppState,
    api::{
        ApiError,
        auth::require_student,
        dto::{
            HistoryMessage, ImportSessionResponse, MessageListResponse, RunRequest,
            SessionListItem, SessionListResponse,
        },
        parse_json,
    },
    domain::SessionId,
    store::access::StudentAccessRepository,
    store::sessions::{
        DocumentRepository, MAX_SESSION_EXPORT_BYTES, MessageRepository, SessionExportV1,
        SessionRepository,
    },
};

pub const MAX_DOCUMENT_UPLOAD_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
struct SessionListQuery {
    user_id: Option<String>,
}

#[derive(Deserialize)]
struct DocumentUploadQuery {
    filename: Option<String>,
}

#[derive(Deserialize)]
struct MessageCreateRequest {
    content: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_sessions).post(create_session))
        .route(
            "/import",
            post(import_session).layer(DefaultBodyLimit::max(MAX_SESSION_EXPORT_BYTES)),
        )
        .route("/{id}", get(get_session))
        .route("/{id}/messages", get(list_messages).post(send_message))
        .route("/{id}/report", get(get_report))
        .route(
            "/{id}/documents",
            post(upload_document).layer(DefaultBodyLimit::max(MAX_DOCUMENT_UPLOAD_BYTES)),
        )
        .route("/{id}/export", get(export_session))
}

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SessionListQuery>,
) -> Result<Json<SessionListResponse>, ApiError> {
    let principal = require_student(&state, &headers).await?;
    let mut sessions = SessionRepository::new(state.pool.clone())
        .list_recent_with_preview(50, query.user_id.as_deref())
        .await?;
    let owned = StudentAccessRepository::with_pepper(
        state.pool,
        state.security.student_token_pepper.clone(),
    )
    .owned_session_ids(&principal.id)
    .await?
    .into_iter()
    .collect::<std::collections::HashSet<_>>();
    sessions.retain(|item| owned.contains(&item.session.id.to_legacy_hex()));
    let sessions = sessions.into_iter().map(SessionListItem::from).collect();
    Ok(Json(SessionListResponse { sessions }))
}

async fn create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    _payload: Option<Json<Value>>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let principal = require_student(&state, &headers).await?;
    let user_id = Some(principal.student_id.as_str());
    let session = SessionRepository::new(state.pool.clone())
        .create(user_id)
        .await?;
    StudentAccessRepository::with_pepper(
        state.pool.clone(),
        state.security.student_token_pepper.clone(),
    )
    .bind_session(session.id, &principal.id)
    .await?;
    let mut state_json = json!({});
    state_json["student_profile"] = json!({
        "name": principal.student_name,
        "student_id": principal.student_id,
    });
    SessionRepository::new(state.pool)
        .save_state(session.id, state_json)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "session_id": session.id.to_legacy_hex(),
            "stage": session.stage,
        })),
    ))
}

async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let session_id = parse_session_id(&id)?;
    ensure_owner(&state, &headers, session_id).await?;
    let session = SessionRepository::new(state.pool.clone())
        .get(session_id)
        .await?;
    let session_state = SessionRepository::new(state.pool)
        .load_state(session_id)
        .await?;
    Ok(Json(json!({
        "session_id": session.id.to_legacy_hex(),
        "task_type": session.task_type,
        "stage": session.stage,
        "state": session_state.state_json,
    })))
}

async fn list_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<MessageListResponse>, ApiError> {
    let session_id = parse_session_id(&id)?;
    ensure_owner(&state, &headers, session_id).await?;
    SessionRepository::new(state.pool.clone())
        .get(session_id)
        .await?;
    let messages = MessageRepository::new(state.pool)
        .list_by_session(session_id)
        .await?
        .into_iter()
        .map(HistoryMessage::from)
        .collect();
    Ok(Json(MessageListResponse { messages }))
}

async fn send_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<MessageCreateRequest>,
) -> Result<Json<Value>, ApiError> {
    let session_id = parse_session_id(&id)?;
    ensure_owner(&state, &headers, session_id).await?;
    let handle = crate::api::runs::start_run(
        &state,
        RunRequest {
            session_id: Some(id),
            user_id: None,
            student_name: None,
            student_id: None,
            message: payload.content,
            action: None,
            response_mode: None,
            enable_web_search: false,
        },
    )
    .await?;
    let response = crate::api::runs::finish_chat(&state, handle).await?;
    let session = SessionRepository::new(state.pool.clone())
        .get(session_id)
        .await?;
    Ok(Json(json!({
        "session_id": response.session_id,
        "stage": session.stage,
        "reply": {
            "type": response.metadata.get("answer_type").and_then(Value::as_str).unwrap_or("answer"),
            "content": response.reply,
            "missing_slots": response.awaiting_slots,
            "next_actions": [],
        },
        "state_summary": {
            "task_type": session.task_type,
            "collected_slots": response.metadata.get("student_progress").and_then(|v| v.get("collected_slots")).cloned().unwrap_or_else(|| json!([])),
            "missing_slots": response.awaiting_slots,
        }
    })))
}

async fn get_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let session_id = parse_session_id(&id)?;
    ensure_owner(&state, &headers, session_id).await?;
    let state_json = SessionRepository::new(state.pool)
        .load_state(session_id)
        .await?
        .state_json;
    let writing = state_json
        .get("writing_context")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let report = state_json.get("report").cloned().unwrap_or_else(|| {
        let initial_problems = state_json
            .get("diagnosis")
            .and_then(|value| value.get("problems"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|problem| problem.get("problem").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        json!({
            "initial_problems": initial_problems,
            "improvements": [],
            "remaining_issues": [],
            "next_practice_tasks": [writing.get("next_task").and_then(Value::as_str).unwrap_or("完成一轮修改后再生成成长报告。")],
        })
    });
    Ok(Json(json!({"session_id": id, "report": report})))
}

async fn export_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SessionExportV1>, ApiError> {
    let session_id = parse_session_id(&id)?;
    ensure_owner(&state, &headers, session_id).await?;
    Ok(Json(
        SessionRepository::new(state.pool)
            .export_v1(session_id)
            .await?,
    ))
}

async fn upload_document(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DocumentUploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let session_id = parse_session_id(&id)?;
    ensure_owner(&state, &headers, session_id).await?;
    let supplied_content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let (filename, content_type, bytes) =
        if supplied_content_type.starts_with("multipart/form-data") {
            parse_multipart_document(supplied_content_type, body).await?
        } else {
            let filename = query
                .filename
                .as_deref()
                .ok_or_else(|| ApiError::bad_request("document filename is required"))?;
            let (filename, expected_content_type) = validate_document_filename(filename)?;
            let actual = supplied_content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim();
            if actual != expected_content_type {
                return Err(ApiError::bad_request("invalid document content type"));
            }
            (filename.to_owned(), expected_content_type.to_owned(), body)
        };
    if bytes.is_empty() {
        return Err(ApiError::bad_request("document is empty"));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| ApiError::bad_request("document must be valid UTF-8"))?;
    let parsed_text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .replace("\r\n", "\n")
        .replace('\r', "\n");

    SessionRepository::new(state.pool.clone())
        .get(session_id)
        .await?;
    let document = DocumentRepository::new(state.pool)
        .add(
            session_id,
            &filename,
            &content_type,
            None,
            Some(&parsed_text),
            Some(serde_json::json!({
                "source": "student_upload",
                "size_bytes": bytes.len(),
            })),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": document.id.to_legacy_hex(),
            "document_id": document.id.to_legacy_hex(),
            "session_id": document.session_id.to_legacy_hex(),
            "filename": document.filename,
            "content_type": document.content_type,
            "parsed_text": document.parsed_text,
            "size_bytes": bytes.len(),
        })),
    ))
}

async fn parse_multipart_document(
    content_type: &str,
    body: Bytes,
) -> Result<(String, String, Bytes), ApiError> {
    let boundary = multer::parse_boundary(content_type)
        .map_err(|_| ApiError::bad_request("invalid multipart boundary"))?;
    let body_stream = stream::once(async move { Ok::<Bytes, std::io::Error>(body) });
    let mut multipart = multer::Multipart::new(body_stream, boundary);
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::bad_request("invalid multipart upload"))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field
            .file_name()
            .ok_or_else(|| ApiError::bad_request("document filename is required"))?
            .to_owned();
        let (_, expected) = validate_document_filename(&filename)?;
        let declared = field
            .content_type()
            .map(ToString::to_string)
            .unwrap_or_else(|| expected.to_owned());
        if declared != expected && declared != "application/octet-stream" {
            return Err(ApiError::bad_request("invalid document content type"));
        }
        let bytes = field
            .bytes()
            .await
            .map_err(|_| ApiError::bad_request("invalid multipart upload"))?;
        return Ok((filename, expected.to_owned(), bytes));
    }
    Err(ApiError::bad_request("multipart file field is required"))
}

async fn ensure_owner(
    state: &AppState,
    headers: &HeaderMap,
    session_id: SessionId,
) -> Result<(), ApiError> {
    let principal = require_student(state, headers).await?;
    if !StudentAccessRepository::with_pepper(
        state.pool.clone(),
        state.security.student_token_pepper.clone(),
    )
    .owns_session(session_id, &principal.id)
    .await?
    {
        return Err(ApiError::forbidden("session is not owned by this student"));
    }
    Ok(())
}

async fn import_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Result<Json<SessionExportV1>, JsonRejection>,
) -> Result<(StatusCode, Json<ImportSessionResponse>), ApiError> {
    let principal = require_student(&state, &headers).await?;
    let export = parse_json(payload)?;
    let imported = SessionRepository::new(state.pool.clone())
        .import_v1_with_runs(export)
        .await?;
    StudentAccessRepository::with_pepper(state.pool, state.security.student_token_pepper.clone())
        .bind_session(imported.session.id, &principal.id)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(ImportSessionResponse {
            session_id: imported.session.id.to_legacy_hex(),
            run_ids: imported.run_ids,
        }),
    ))
}

fn parse_session_id(value: &str) -> Result<SessionId, ApiError> {
    SessionId::parse_legacy(value).map_err(|_| ApiError::invalid_identifier())
}

fn validate_document_filename(value: &str) -> Result<(&str, &'static str), ApiError> {
    let filename = value.trim();
    if filename.is_empty()
        || filename != value
        || filename.len() > 255
        || filename == "."
        || filename == ".."
        || filename.contains(['/', '\\'])
        || filename.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request("invalid document filename"));
    }
    let (stem, extension) = filename
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .ok_or_else(|| ApiError::bad_request("unsupported document type"))?;
    if stem == "." || stem == ".." {
        return Err(ApiError::bad_request("invalid document filename"));
    }
    let content_type = if extension.eq_ignore_ascii_case("txt") {
        "text/plain"
    } else if extension.eq_ignore_ascii_case("md") {
        "text/markdown"
    } else {
        return Err(ApiError::bad_request("unsupported document type"));
    };
    Ok((filename, content_type))
}
