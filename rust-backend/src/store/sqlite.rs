use std::{str::FromStr, time::Duration};

use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

use crate::AppError;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn open_database(url: &str) -> Result<SqlitePool, AppError> {
    let options = SqliteConnectOptions::from_str(url)
        .map_err(|error| AppError::InvalidDatabaseUrl(error.to_string()))?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));

    SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .map_err(AppError::from)
}

pub async fn migrate(pool: &SqlitePool) -> Result<(), AppError> {
    MIGRATOR.run(pool).await?;
    Ok(())
}
