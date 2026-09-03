use axum::http::HeaderMap;

use crate::{
    api::ApiError,
    store::access::{StudentAccessRepository, StudentPrincipal},
};

pub(crate) async fn optional_student(
    pool: sqlx::SqlitePool,
    headers: &HeaderMap,
) -> Result<Option<StudentPrincipal>, ApiError> {
    let Some(value) = headers.get("authorization") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| ApiError::unauthorized("invalid student access credential"))?;
    let (scheme, token) = value.split_once(' ').unwrap_or(("", ""));
    if !scheme.eq_ignore_ascii_case("bearer") || token.trim().is_empty() {
        return Err(ApiError::unauthorized("student access credential required"));
    }
    StudentAccessRepository::new(pool)
        .authenticate(token.trim())
        .await?
        .map(Some)
        .ok_or_else(|| ApiError::unauthorized("invalid student access credential"))
}
