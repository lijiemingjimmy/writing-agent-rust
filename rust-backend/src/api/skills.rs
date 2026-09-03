use std::path::PathBuf;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, Uri},
    routing::{get, post},
};
use serde_json::{Value, json};

use crate::{
    AppState,
    api::{ApiError, auth::require_teacher},
    skills::SkillRegistry,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_skills))
        .route("/reload", post(reload_skills))
        .route("/{skill_id}", get(get_skill))
}

fn load_registry() -> Result<SkillRegistry, ApiError> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| ApiError::bad_request("skill root unavailable"))?
        .join("skills");
    SkillRegistry::load(&root).map_err(ApiError::from)
}

fn public_skill(skill: &crate::skills::SkillDefinition) -> Value {
    json!({"id": skill.id, "name": skill.name, "description": skill.description})
}

async fn list_skills() -> Result<Json<Value>, ApiError> {
    let registry = load_registry()?;
    Ok(Json(
        json!({"skills": registry.all().map(public_skill).collect::<Vec<_>>() }),
    ))
}

async fn get_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
    Path(skill_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_teacher(&state, &headers, &uri)?;
    let registry = load_registry()?;
    let skill = registry
        .get(&skill_id)
        .ok_or_else(|| ApiError::not_found("skill not found"))?;
    Ok(Json(json!({"skill": public_skill(skill)})))
}

async fn reload_skills(
    State(state): State<AppState>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Json<Value>, ApiError> {
    require_teacher(&state, &headers, &uri)?;
    let registry = load_registry()?;
    Ok(Json(json!({"count": registry.all().len()})))
}
