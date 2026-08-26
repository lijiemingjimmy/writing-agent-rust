use sqlx::{Executor, SqlitePool};

pub async fn create_existing_schema(pool: &SqlitePool) {
    pool.execute(
        r#"
        CREATE TABLE sessions (
            id CHAR(32) NOT NULL PRIMARY KEY,
            user_id VARCHAR(255),
            task_type VARCHAR(80),
            stage VARCHAR(80) NOT NULL,
            created_at DATETIME NOT NULL,
            updated_at DATETIME NOT NULL
        );
        CREATE TABLE messages (
            id CHAR(32) NOT NULL PRIMARY KEY,
            session_id CHAR(32) NOT NULL,
            role VARCHAR(20) NOT NULL,
            content TEXT NOT NULL,
            metadata_json JSON NOT NULL,
            created_at DATETIME NOT NULL,
            FOREIGN KEY(session_id) REFERENCES sessions (id)
        );
        CREATE INDEX ix_messages_session_id ON messages (session_id);
        CREATE TABLE skill_events (
            id CHAR(32) NOT NULL PRIMARY KEY,
            session_id CHAR(32) NOT NULL,
            skill_id VARCHAR(120) NOT NULL,
            event_type VARCHAR(80) NOT NULL,
            metadata_json JSON NOT NULL,
            created_at DATETIME NOT NULL,
            FOREIGN KEY(session_id) REFERENCES sessions (id)
        );
        CREATE INDEX ix_skill_events_session_id ON skill_events (session_id);
        CREATE INDEX ix_skill_events_skill_id ON skill_events (skill_id);
        CREATE TABLE session_states (
            session_id CHAR(32) NOT NULL PRIMARY KEY,
            state_json JSON NOT NULL,
            updated_at DATETIME NOT NULL,
            FOREIGN KEY(session_id) REFERENCES sessions (id)
        );
        CREATE TABLE documents (
            id CHAR(32) NOT NULL PRIMARY KEY,
            session_id CHAR(32) NOT NULL,
            filename VARCHAR(255) NOT NULL,
            content_type VARCHAR(120) NOT NULL,
            raw_path TEXT,
            parsed_text TEXT,
            metadata_json JSON NOT NULL,
            created_at DATETIME NOT NULL,
            FOREIGN KEY(session_id) REFERENCES sessions (id)
        );
        CREATE INDEX ix_documents_session_id ON documents (session_id);
        "#,
    )
    .await
    .unwrap();
}

pub async fn insert_existing_session(pool: &SqlitePool, id: &str) {
    sqlx::query(
        "INSERT INTO sessions (id, stage, created_at, updated_at) VALUES (?, 'NEW', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

pub async fn table_exists(pool: &SqlitePool, table: &str) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .unwrap()
        == 1
}
