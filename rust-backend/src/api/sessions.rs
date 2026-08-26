use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    routing::{get, post},
};
use serde::Deserialize;

use crate::{
    AppState,
    api::{
        ApiError,
        dto::{
            DocumentUploadResponse, HistoryMessage, ImportSessionResponse, MessageListResponse,
            SessionListItem, SessionListResponse,
        },
        parse_json,
    },
    domain::SessionId,
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
    filename: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_sessions))
        .route(
            "/import",
            post(import_session).layer(DefaultBodyLimit::max(MAX_SESSION_EXPORT_BYTES)),
        )
        .route("/{id}/messages", get(list_messages))
        .route(
            "/{id}/documents",
            post(upload_document).layer(DefaultBodyLimit::max(MAX_DOCUMENT_UPLOAD_BYTES)),
        )
        .route("/{id}/export", get(export_session))
}

async fn list_sessions(
    State(state): State<AppState>,
    Query(query): Query<SessionListQuery>,
) -> Result<Json<SessionListResponse>, ApiError> {
    let sessions = SessionRepository::new(state.pool)
        .list_recent_with_preview(50, query.user_id.as_deref())
        .await?
        .into_iter()
        .map(SessionListItem::from)
        .collect();
    Ok(Json(SessionListResponse { sessions }))
}

async fn list_messages(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<MessageListResponse>, ApiError> {
    let session_id = parse_session_id(&id)?;
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

async fn export_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionExportV1>, ApiError> {
    let session_id = parse_session_id(&id)?;
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
) -> Result<(StatusCode, Json<DocumentUploadResponse>), ApiError> {
    let session_id = parse_session_id(&id)?;
    let (filename, expected_content_type) = validate_document_filename(&query.filename)?;
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| *value == expected_content_type)
        .ok_or_else(|| ApiError::bad_request("invalid document content type"))?;
    if body.is_empty() {
        return Err(ApiError::bad_request("document is empty"));
    }
    let text = std::str::from_utf8(&body)
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
            filename,
            content_type,
            None,
            Some(&parsed_text),
            Some(serde_json::json!({
                "source": "student_upload",
                "size_bytes": body.len(),
            })),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(DocumentUploadResponse {
            document_id: document.id.to_legacy_hex(),
            session_id: document.session_id.to_legacy_hex(),
            filename: document.filename,
            content_type: document.content_type,
            size_bytes: body.len(),
        }),
    ))
}

async fn import_session(
    State(state): State<AppState>,
    payload: Result<Json<SessionExportV1>, JsonRejection>,
) -> Result<(StatusCode, Json<ImportSessionResponse>), ApiError> {
    let export = parse_json(payload)?;
    let imported = SessionRepository::new(state.pool)
        .import_v1_with_runs(export)
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
