use std::{path::Path, sync::Arc};

use axum::{
    Router,
    http::{HeaderValue, Method, header::CONTENT_TYPE},
    routing::get,
};
use sqlx::SqlitePool;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub mod agent;
pub mod api;
pub mod config;
pub mod corpus;
pub mod domain;
pub mod error;
pub mod llm;
pub mod skills;
pub mod store;
pub mod tools;

pub use config::AppConfig;
pub use error::AppError;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub run_engine: agent::RunEngine,
    pub model_settings: Arc<llm::ModelSettingsStore>,
}

pub async fn build_app(config: AppConfig) -> Result<Router, AppError> {
    let pool = store::sqlite::open_database(&config.database_url).await?;
    store::sqlite::migrate(&pool).await?;

    let model_settings = Arc::new(
        llm::ModelSettingsStore::new_with_run_defaults(
            config.model.clone(),
            config.run_defaults.clone(),
        )
        .map_err(|error| AppError::InvalidConfig(error.to_string()))?,
    );
    let registry = skills::SkillRegistry::load(Path::new(&config.skill_root))?;
    let (knowledge, web_enabled) = config.build_knowledge_coordinator()?;
    let knowledge = Arc::new(knowledge);
    let program = Arc::new(agent::WritingCoachProgram::new(
        pool.clone(),
        registry,
        knowledge,
        web_enabled,
    ));
    let gateway = Arc::new(llm::GenaiModelGateway::new(model_settings.clone()));
    let run_engine = agent::RunEngine::new(pool.clone(), program, gateway, model_settings.clone());
    run_engine.reconcile_orphans().await?;
    let state = AppState {
        pool,
        run_engine,
        model_settings,
    };

    let app = api::router(state).route("/health", get(api::health::health));
    if config.cors_allowed_origins.is_empty() {
        return Ok(app);
    }
    let allowed_origins = config
        .cors_allowed_origins
        .iter()
        .map(|origin| {
            HeaderValue::from_str(origin)
                .map_err(|_| AppError::InvalidConfig("invalid CORS allowed origin".to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(app.layer(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(allowed_origins))
            .allow_methods([Method::GET, Method::POST, Method::PUT])
            .allow_headers([CONTENT_TYPE]),
    ))
}
