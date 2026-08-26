use axum::{Json, response::IntoResponse};
use serde_json::json;

pub async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok", "service": "writing-coach-rust"}))
}
