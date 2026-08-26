use std::env;

use tokio::net::TcpListener;
use tracing::info;
use tracing_subscriber::EnvFilter;
use writing_coach_server::{AppConfig, build_app};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config_path =
        env::var("WRITING_COACH_CONFIG").unwrap_or_else(|_| "rust-backend/config.toml".to_owned());
    let config = AppConfig::from_file(config_path)?;
    let bind_addr = config.bind_addr;
    let app = build_app(config).await?;
    let listener = TcpListener::bind(bind_addr).await?;

    info!(%bind_addr, "writing coach Rust service listening");
    axum::serve(listener, app).await?;
    Ok(())
}
