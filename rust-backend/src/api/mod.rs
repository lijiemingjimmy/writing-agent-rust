pub mod auth;
pub mod dto;
pub mod health;
pub mod runs;
pub mod sessions;
pub mod settings;
pub mod skills;
pub mod student_access;
pub mod teacher;

use axum::{
    Json, Router,
    extract::rejection::JsonRejection,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::json;

use crate::{AppError, AppState};

pub fn router(state: AppState) -> Router {
    let teacher_router = teacher::router().route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::teacher_guard,
    ));
    Router::new()
        .route("/api/chat", post(runs::chat))
        .nest("/api/student/access", student_access::router())
        .nest("/api/runs", runs::router())
        .nest("/api/sessions", sessions::router())
        .nest("/api/settings", settings::router())
        .nest("/api/skills", skills::router())
        .nest("/api/teacher", teacher_router)
        .with_state(state)
}

pub(crate) struct ApiError {
    status: StatusCode,
    message: &'static str,
}

impl ApiError {
    pub(crate) fn bad_request(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }

    pub(crate) fn invalid_identifier() -> Self {
        Self::bad_request("invalid identifier")
    }

    pub(crate) fn not_found(message: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message,
        }
    }

    pub(crate) fn payload_too_large(message: &'static str) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message,
        }
    }

    pub(crate) fn conflict(message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message,
        }
    }

    pub(crate) fn bad_gateway(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message,
        }
    }

    pub(crate) fn unauthorized(message: &'static str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message,
        }
    }

    pub(crate) fn forbidden(message: &'static str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message,
        }
    }
}

impl From<AppError> for ApiError {
    fn from(error: AppError) -> Self {
        let (status, message) = match error {
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, "resource not found"),
            AppError::InvalidExport(_) => (StatusCode::BAD_REQUEST, "invalid session export"),
            AppError::InvalidRun(_) => (StatusCode::BAD_REQUEST, "invalid run request"),
            AppError::ContextCapacityExceeded { .. } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "input exceeds model context capacity",
            ),
            AppError::ActiveRunConflict => {
                (StatusCode::CONFLICT, "session already has an active run")
            }
            AppError::RunTerminal => (StatusCode::CONFLICT, "run is already terminal"),
            AppError::RunCancelled => (StatusCode::CONFLICT, "run was cancelled"),
            AppError::RunBudgetExceeded(_) => (StatusCode::CONFLICT, "run budget was exceeded"),
            AppError::RunMaxSteps => (StatusCode::CONFLICT, "run reached its step limit"),
            AppError::Model(_) | AppError::Pricing(_) => {
                (StatusCode::BAD_GATEWAY, "agent execution failed")
            }
            AppError::CorruptData(_)
            | AppError::Sqlx(_)
            | AppError::Migration(_)
            | AppError::Io(_)
            | AppError::Toml(_)
            | AppError::InvalidConfig(_)
            | AppError::InvalidDatabaseUrl(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
            }
        };
        Self { status, message }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}

pub(crate) fn parse_json<T>(payload: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    payload.map(|Json(value)| value).map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::payload_too_large("request payload exceeds size limit")
        } else {
            ApiError::bad_request("invalid JSON request")
        }
    })
}

pub(crate) fn parse_optional_json<T>(
    payload: Result<Option<Json<T>>, JsonRejection>,
) -> Result<Option<T>, ApiError> {
    payload
        .map(|value| value.map(|Json(value)| value))
        .map_err(|rejection| {
            if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::payload_too_large("request payload exceeds size limit")
            } else {
                ApiError::bad_request("invalid JSON request")
            }
        })
}
