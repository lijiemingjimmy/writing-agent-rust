use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AppState, api::ApiError, store::access::StudentAccessRepository};

#[derive(Deserialize)]
struct BootstrapRequest {
    student_name: String,
    student_id: String,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/bootstrap", post(bootstrap))
}

async fn bootstrap(
    State(state): State<AppState>,
    Json(payload): Json<BootstrapRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let name = payload.student_name.trim();
    let student_id = payload.student_id.trim();
    if name.is_empty() || student_id.is_empty() || name.len() > 255 || student_id.len() > 255 {
        return Err(ApiError::bad_request("student name and ID are required"));
    }
    let (token, principal) = StudentAccessRepository::new(state.pool)
        .bootstrap(name, student_id)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "access_token": token,
            "principal_id": principal.id,
            "student_name": principal.student_name,
            "student_id": principal.student_id,
        })),
    ))
}
