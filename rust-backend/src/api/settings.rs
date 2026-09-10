use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, Uri},
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
    headers: HeaderMap,
    payload: Result<Json<ModelSettingsUpdate>, JsonRejection>,
) -> Result<Json<PublicModelSettings>, ApiError> {
    if state
        .security
        .teacher_access_token
        .as_deref()
        .is_none_or(|token| token.is_empty())
    {
        return Err(ApiError::forbidden(
            "请先在服务端配置 security.teacher_access_token，才能修改共享模型设置",
        ));
    }
    crate::api::auth::require_teacher(&state, &headers, &Uri::from_static("/api/settings/model"))?;
    let update = parse_json(payload)?;
    state
        .model_settings
        .update(update)
        .map_err(settings_error)?;
    Ok(Json(state.model_settings.public()))
}

fn settings_error(error: ModelSettingsError) -> ApiError {
    use ModelSettingsError::*;
    ApiError::bad_request(match error {
        UnsupportedProvider => "不支持的模型服务商，请选择 DeepSeek、OpenAI 或 openai-compatible",
        InvalidEndpoint => "API Endpoint 必须是完整的 HTTP(S) 地址",
        EmptyModelName => "模型名称不能为空",
        OutputExceedsContext => "最大输出 Token 不能超过上下文长度",
        UnsupportedReasoningMode => "不支持的思考模式，请检查 reasoning_mode",
        EndpointKeyRequired => "更换模型地址或服务商时，请重新填写该服务的 API Key",
        ZeroContextLength | ZeroMaxOutputTokens => "上下文和最大输出 Token 必须大于零",
        ZeroDefaultTokenBudget | ZeroDefaultCostBudget | BudgetOutOfRange => {
            "默认预算必须为有效的正整数"
        }
        PriceOutOfRange => "模型价格超出支持范围",
        _ => "模型配置不可用，请检查服务端配置及 API Key 环境变量",
    })
}
