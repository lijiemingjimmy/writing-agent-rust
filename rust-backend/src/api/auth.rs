use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, Uri},
    middleware::Next,
    response::Response,
};

use crate::{
    AppState,
    api::ApiError,
    store::access::{StudentAccessRepository, StudentPrincipal},
};

pub(crate) async fn optional_student(
    state: &AppState,
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
    StudentAccessRepository::with_pepper(
        state.pool.clone(),
        state.security.student_token_pepper.clone(),
    )
    .authenticate(token.trim())
    .await?
    .map(Some)
    .ok_or_else(|| ApiError::unauthorized("invalid student access credential"))
}

pub(crate) async fn require_student(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<StudentPrincipal, ApiError> {
    optional_student(state, headers)
        .await?
        .ok_or_else(|| ApiError::unauthorized("student access credential required"))
}

pub(crate) fn require_teacher(
    state: &AppState,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<(), ApiError> {
    let expected = state.security.teacher_access_token.as_deref();
    let Some(expected) = expected.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let header_token = headers
        .get("x-teacher-token")
        .and_then(|value| value.to_str().ok());
    let query_token = uri.query().and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == "teacher_token")
            .map(|(_, value)| value.into_owned())
    });
    if header_token == Some(expected) || query_token.as_deref() == Some(expected) {
        Ok(())
    } else {
        Err(ApiError::forbidden("teacher access token is incorrect"))
    }
}

pub(crate) async fn teacher_guard(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response, ApiError> {
    require_teacher(&state, request.headers(), request.uri())?;
    Ok(next.run(request).await)
}
