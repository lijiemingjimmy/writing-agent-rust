mod support;

use sqlx::{Row, SqlitePool};
use writing_coach_server::{AppError, domain::SessionId};

#[sqlx::test(migrations = false)]
async fn migrations_preserve_existing_rows(pool: SqlitePool) {
    support::create_existing_schema(&pool).await;
    support::insert_existing_session(&pool, "0123456789abcdef0123456789abcdef").await;

    writing_coach_server::store::sqlite::migrate(&pool)
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);

    for table in ["agent_runs", "run_events", "model_calls"] {
        assert!(support::table_exists(&pool, table).await);
    }
}

#[sqlx::test]
async fn fresh_schema_enforces_required_columns(pool: SqlitePool) {
    assert_required_columns(&pool).await;
}

#[sqlx::test(migrations = false)]
async fn existing_schema_enforces_required_columns_after_upgrade(pool: SqlitePool) {
    support::create_existing_schema(&pool).await;
    assert_required_columns(&pool).await;
}

#[test]
fn ids_round_trip_storage_hex() {
    let id = SessionId::parse_legacy("0123456789abcdef0123456789abcdef").unwrap();

    assert_eq!(id.to_legacy_hex(), "0123456789abcdef0123456789abcdef");
}

#[test]
fn ids_reject_invalid_database_values_as_corrupt_data() {
    for invalid in [
        "not-a-uuid",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef-",
        "0123456789ABCDEF0123456789ABCDEF",
    ] {
        assert!(matches!(
            SessionId::parse_legacy(invalid),
            Err(AppError::CorruptData(_))
        ));
    }
}

async fn assert_required_columns(pool: &SqlitePool) {
    for (table, expected) in [
        ("sessions", &["created_at", "updated_at"][..]),
        ("messages", &["metadata_json", "created_at"][..]),
        (
            "skill_events",
            &["skill_id", "metadata_json", "created_at"][..],
        ),
        ("session_states", &["state_json", "updated_at"][..]),
        ("documents", &["metadata_json", "created_at"][..]),
    ] {
        let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(pool)
            .await
            .unwrap();
        for column in expected {
            let row = rows
                .iter()
                .find(|row| row.get::<String, _>("name") == *column)
                .unwrap_or_else(|| panic!("missing {table}.{column}"));
            assert_eq!(
                row.get::<i64, _>("notnull"),
                1,
                "{table}.{column} must be NOT NULL"
            );
        }
    }
}
