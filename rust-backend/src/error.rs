#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("failed to read configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse configuration: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid database URL: {0}")]
    InvalidDatabaseUrl(String),
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("database migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("corrupt database data: {0}")]
    CorruptData(String),
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("invalid session export: {0}")]
    InvalidExport(String),
    #[error("invalid run request: {0}")]
    InvalidRun(String),
    #[error("session already has an active run")]
    ActiveRunConflict,
    #[error("run was cancelled")]
    RunCancelled,
    #[error("run budget was exceeded: {0}")]
    RunBudgetExceeded(String),
    #[error("run reached its maximum step count")]
    RunMaxSteps,
    #[error("run is already terminal")]
    RunTerminal,
    #[error(transparent)]
    Model(#[from] crate::llm::ModelError),
    #[error(transparent)]
    Pricing(#[from] crate::llm::PricingError),
}
