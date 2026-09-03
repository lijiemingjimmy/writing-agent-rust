use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{AppError, domain::SessionId};

#[derive(Clone, Debug)]
pub struct StudentPrincipal {
    pub id: String,
    pub student_name: String,
    pub student_id: String,
}

#[derive(Clone)]
pub struct StudentAccessRepository {
    pool: SqlitePool,
}

impl StudentAccessRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn bootstrap(
        &self,
        student_name: &str,
        student_id: &str,
    ) -> Result<(String, StudentPrincipal), AppError> {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let principal = StudentPrincipal {
            id: Uuid::new_v4().simple().to_string(),
            student_name: student_name.trim().to_owned(),
            student_id: student_id.trim().to_owned(),
        };
        sqlx::query(
            "INSERT INTO student_principals \
             (id, student_name, student_id, token_hash, created_at) \
             VALUES (?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&principal.id)
        .bind(&principal.student_name)
        .bind(&principal.student_id)
        .bind(token_digest(&token))
        .execute(&self.pool)
        .await?;
        Ok((token, principal))
    }

    pub async fn authenticate(&self, token: &str) -> Result<Option<StudentPrincipal>, AppError> {
        let row = sqlx::query(
            "SELECT id, student_name, student_id FROM student_principals \
             WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(token_digest(token))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| StudentPrincipal {
            id: row.get("id"),
            student_name: row.get("student_name"),
            student_id: row.get("student_id"),
        }))
    }

    pub async fn bind_session(
        &self,
        session_id: SessionId,
        principal_id: &str,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO session_ownerships (session_id, principal_id, created_at) \
             VALUES (?, ?, CURRENT_TIMESTAMP) \
             ON CONFLICT(session_id) DO UPDATE SET principal_id = excluded.principal_id",
        )
        .bind(session_id.to_legacy_hex())
        .bind(principal_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn owns_session(
        &self,
        session_id: SessionId,
        principal_id: &str,
    ) -> Result<bool, AppError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_ownerships WHERE session_id = ? AND principal_id = ?",
        )
        .bind(session_id.to_legacy_hex())
        .bind(principal_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(count > 0)
    }

    pub async fn owned_session_ids(&self, principal_id: &str) -> Result<Vec<String>, AppError> {
        Ok(
            sqlx::query_scalar("SELECT session_id FROM session_ownerships WHERE principal_id = ?")
                .bind(principal_id)
                .fetch_all(&self.pool)
                .await?,
        )
    }
}

fn token_digest(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
