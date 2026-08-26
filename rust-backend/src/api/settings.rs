use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    routing::get,
};

use crate::{
    AppState,
    api::{ApiError, parse_json},
    llm::{ModelSettingsError, ModelSettingsUpdate, PublicModelSettings},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/model", get(get_model).put(put_model))
}

async fn get_model(State(state): State<AppState>) -> Json<PublicModelSettings> {
    Json(state.model_settings.public())
}

async fn put_model(
    State(state): State<AppState>,
    payload: Result<Json<ModelSettingsUpdate>, JsonRejection>,
) -> Result<Json<PublicModelSettings>, ApiError> {
    let update = parse_json(payload)?;
    state
        .model_settings
        .update(update)
        .map_err(settings_error)?;
    Ok(Json(state.model_settings.public()))
}

fn settings_error(_: ModelSettingsError) -> ApiError {
    ApiError::bad_request("invalid model settings")
}
