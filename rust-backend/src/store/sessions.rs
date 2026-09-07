use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Row, Sqlite, SqlitePool, Transaction, sqlite::SqliteRow};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio::sync::Notify;

use crate::{
    AppError,
    corpus::chunking::{NewDocumentChunk, chunk_document},
    domain::{
        Document, DocumentChunk, DocumentChunkId, DocumentId, Message, MessageId, PriceSnapshot,
        RunStatus, Session, SessionId, SessionState, SkillEvent, SkillEventId, Usage,
    },
    llm::calculate_cost,
};

pub const MAX_SESSION_EXPORT_BYTES: usize = 8 * 1024 * 1024;
const MAX_SESSION_EXPORT_ROWS: usize = 100_000;
const SESSION_EXPORT_SCHEMA: &str = "writing-coach.session";
const JSON_WORST_CASE_EXPANSION: usize = 6;
const JSON_ROW_OVERHEAD: usize = 1024;
const JSON_ENVELOPE_OVERHEAD: usize = 4096;

#[derive(Clone)]
pub struct SessionRepository {
    pool: SqlitePool,
    #[cfg(test)]
    export_snapshot_test_gate: Option<(Arc<Notify>, Arc<Notify>)>,
}

#[derive(Clone)]
pub struct MessageRepository {
    pool: SqlitePool,
}

#[derive(Clone)]
pub struct DocumentRepository {
    pool: SqlitePool,
}

#[derive(Clone)]
pub struct SkillEventRepository {
    pool: SqlitePool,
}

#[derive(Clone, Debug)]
pub struct SessionSummary {
    pub session: Session,
    pub student_name: Option<String>,
    pub student_id: Option<String>,
    pub preview: Option<String>,
    pub message_count: i64,
}

#[derive(Clone, Debug)]
pub struct ImportedSession {
    pub session: Session,
    pub run_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionExportV1 {
    pub schema: String,
    pub version: u8,
    pub source_session_id: String,
    pub session: TransferSession,
    pub state: TransferState,
    pub messages: Vec<TransferMessage>,
    pub documents: Vec<TransferDocument>,
    pub skill_events: Vec<TransferSkillEvent>,
    pub runs: Vec<TransferRun>,
    pub run_events: Vec<TransferRunEvent>,
    pub model_calls: Vec<TransferModelCall>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferSession {
    pub id: String,
    pub user_id: Option<String>,
    pub task_type: Option<String>,
    pub stage: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferState {
    pub session_id: String,
    pub state_json: Value,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub metadata_json: Value,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferDocument {
    pub id: String,
    pub session_id: String,
    pub filename: String,
    pub content_type: String,
    pub raw_path: Option<String>,
    pub parsed_text: Option<String>,
    pub metadata_json: Value,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferSkillEvent {
    pub id: String,
    pub session_id: String,
    pub skill_id: String,
    pub event_type: String,
    pub metadata_json: Value,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferRun {
    pub id: String,
    pub session_id: String,
    pub status: RunStatus,
    pub current_step: Option<String>,
    pub max_steps: u32,
    pub token_budget: Option<u64>,
    pub cost_budget_microusd: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_microusd: u64,
    pub cancel_reason: Option<String>,
    pub error_message: Option<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferRunEvent {
    pub id: String,
    pub run_id: String,
    pub seq: u64,
    pub kind: String,
    pub payload: Value,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransferModelCall {
    pub id: String,
    pub run_id: String,
    pub purpose: String,
    pub provider: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub input_price_microusd_per_million: u64,
    pub output_price_microusd_per_million: u64,
    pub cost_microusd: u64,
    pub duration_ms: Option<u64>,
    pub finish_reason: Option<String>,
    pub response_id: Option<String>,
    pub created_at: Option<String>,
}

impl SessionRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            #[cfg(test)]
            export_snapshot_test_gate: None,
        }
    }

    #[cfg(test)]
    fn with_export_snapshot_test_gate(
        mut self,
        entered: Arc<Notify>,
        release: Arc<Notify>,
    ) -> Self {
        self.export_snapshot_test_gate = Some((entered, release));
        self
    }

    pub async fn create(&self, user_id: Option<&str>) -> Result<Session, AppError> {
        let id = SessionId::new();
        let id_hex = id.to_legacy_hex();
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO sessions (id, user_id, stage, created_at, updated_at) \
             VALUES (?, ?, 'NEW', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .bind(&id_hex)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO session_states (session_id, state_json, updated_at) \
             VALUES (?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&id_hex)
        .bind("{}")
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        self.get(id).await
    }

    pub async fn get(&self, id: SessionId) -> Result<Session, AppError> {
        let row = sqlx::query(
            "SELECT id, user_id, task_type, stage, created_at, updated_at FROM sessions WHERE id = ?",
        )
        .bind(id.to_legacy_hex())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("session".to_owned()))?;
        session_from_row(&row)
    }

    pub async fn list_recent(&self, limit: i64) -> Result<Vec<Session>, AppError> {
        let rows = sqlx::query(
            "SELECT id, user_id, task_type, stage, created_at, updated_at \
             FROM sessions ORDER BY updated_at DESC, rowid DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(session_from_row).collect()
    }

    pub async fn list_recent_with_preview(
        &self,
        limit: i64,
        user_id: Option<&str>,
    ) -> Result<Vec<SessionSummary>, AppError> {
        let rows = sqlx::query(
            "SELECT s.id, s.user_id, s.task_type, s.stage, s.created_at, s.updated_at, ss.state_json, \
                    (SELECT m.content FROM messages m \
                     WHERE m.session_id = s.id AND m.role = 'user' \
                     ORDER BY m.created_at DESC, m.rowid DESC LIMIT 1) AS preview, \
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS message_count \
             FROM sessions s LEFT JOIN session_states ss ON ss.session_id = s.id \
             WHERE (? IS NULL OR s.user_id = ?) \
             ORDER BY s.updated_at DESC, s.rowid DESC LIMIT ?",
        )
        .bind(user_id)
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                let session = session_from_row(row)?;
                let state = parse_json(row.try_get("state_json")?, "session_states.state_json")?;
                let profile = state.get("student_profile").and_then(Value::as_object);
                Ok(SessionSummary {
                    student_name: profile
                        .and_then(|profile| profile.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    student_id: profile
                        .and_then(|profile| profile.get("student_id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or_else(|| session.user_id.clone()),
                    session,
                    preview: row.try_get("preview")?,
                    message_count: row.try_get("message_count")?,
                })
            })
            .collect()
    }

    pub async fn load_state(&self, session_id: SessionId) -> Result<SessionState, AppError> {
        let row = sqlx::query(
            "SELECT session_id, state_json, updated_at FROM session_states WHERE session_id = ?",
        )
        .bind(session_id.to_legacy_hex())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("session state".to_owned()))?;
        state_from_row(&row)
    }

    pub async fn save_state(
        &self,
        session_id: SessionId,
        state_json: Value,
    ) -> Result<(), AppError> {
        self.save_state_with_user_id(session_id, state_json, None)
            .await
    }

    pub async fn save_state_with_user_id(
        &self,
        session_id: SessionId,
        state_json: Value,
        user_id: Option<&str>,
    ) -> Result<(), AppError> {
        let session_id_hex = session_id.to_legacy_hex();
        let state_text = serde_json::to_string(&state_json).map_err(|error| {
            AppError::CorruptData(format!("could not serialize session state: {error}"))
        })?;
        let mut tx = self.pool.begin().await?;
        let existing_stage: String = sqlx::query_scalar("SELECT stage FROM sessions WHERE id = ?")
            .bind(&session_id_hex)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::NotFound("session".to_owned()))?;
        let stage = stage_from_state(&state_json).unwrap_or(existing_stage.as_str());
        let task_type = state_json.get("task_type").and_then(Value::as_str);

        sqlx::query(
            "UPDATE sessions SET stage = ?, task_type = ?, user_id = COALESCE(?, user_id), \
             updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(stage)
        .bind(task_type)
        .bind(user_id)
        .bind(&session_id_hex)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO session_states (session_id, state_json, updated_at) VALUES (?, ?, CURRENT_TIMESTAMP) \
             ON CONFLICT(session_id) DO UPDATE SET state_json = excluded.state_json, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(&session_id_hex)
        .bind(state_text)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn export_v1(&self, session_id: SessionId) -> Result<SessionExportV1, AppError> {
        let mut tx = self.pool.begin().await?;
        preflight_export(&mut tx, session_id).await?;
        #[cfg(test)]
        if let Some((entered, release)) = &self.export_snapshot_test_gate {
            entered.notify_one();
            release.notified().await;
        }
        let session = export_session(&mut tx, session_id).await?;
        let state = export_state(&mut tx, session_id).await?;
        let messages = export_messages(&mut tx, session_id).await?;
        let documents = export_documents(&mut tx, session_id).await?;
        let skill_events = export_skill_events(&mut tx, session_id).await?;
        let runs = export_runs(&mut tx, session_id).await?;
        let run_events = export_run_events(&mut tx, session_id).await?;
        let model_calls = export_model_calls(&mut tx, session_id).await?;
        let export = SessionExportV1 {
            schema: SESSION_EXPORT_SCHEMA.to_owned(),
            version: 1,
            source_session_id: session_id.to_legacy_hex(),
            session: TransferSession {
                id: session.id.to_legacy_hex(),
                user_id: session.user_id,
                task_type: session.task_type,
                stage: session.stage,
                created_at: session.created_at,
                updated_at: session.updated_at,
            },
            state: TransferState {
                session_id: state.session_id.to_legacy_hex(),
                state_json: state.state_json,
                updated_at: state.updated_at,
            },
            messages: messages.into_iter().map(TransferMessage::from).collect(),
            documents: documents.into_iter().map(TransferDocument::from).collect(),
            skill_events: skill_events
                .into_iter()
                .map(TransferSkillEvent::from)
                .collect(),
            runs,
            run_events,
            model_calls,
        };
        validate_export(&export)?;
        ensure_export_size(&export)?;
        tx.commit().await?;
        Ok(export)
    }

    pub async fn import_v1(&self, export: SessionExportV1) -> Result<Session, AppError> {
        Ok(self.import_v1_with_runs(export).await?.session)
    }

    pub async fn import_v1_with_runs(
        &self,
        export: SessionExportV1,
    ) -> Result<ImportedSession, AppError> {
        validate_export(&export)?;
        ensure_export_size(&export)?;
        let new_session_id = SessionId::new();
        let new_session_id_hex = new_session_id.to_legacy_hex();
        let run_ids = export
            .runs
            .iter()
            .map(|run| (run.id.clone(), crate::domain::RunId::new().to_legacy_hex()))
            .collect::<HashMap<_, _>>();
        let imported_run_ids = export
            .runs
            .iter()
            .map(|run| run_ids.get(&run.id).expect("validated run map").clone())
            .collect();
        let mut state_json = export.state.state_json.clone();
        rewrite_run_references(&mut state_json, &run_ids);
        state_json
            .as_object_mut()
            .ok_or_else(|| invalid_export("state must be an object"))?
            .insert(
                "imported_from_session_id".to_owned(),
                Value::String(export.source_session_id.clone()),
            );
        let state_text = serde_json::to_string(&state_json)
            .map_err(|_| invalid_export("could not serialize session state"))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO sessions (id, user_id, task_type, stage, created_at, updated_at) \
             VALUES (?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP), COALESCE(?, CURRENT_TIMESTAMP))",
        )
        .bind(&new_session_id_hex)
        .bind(&export.session.user_id)
        .bind(&export.session.task_type)
        .bind(&export.session.stage)
        .bind(&export.session.created_at)
        .bind(&export.session.updated_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO session_states (session_id, state_json, updated_at) VALUES (?, ?, COALESCE(?, CURRENT_TIMESTAMP))",
        )
        .bind(&new_session_id_hex)
        .bind(state_text)
        .bind(&export.state.updated_at)
        .execute(&mut *tx)
        .await?;

        for mut message in export.messages {
            rewrite_run_references(&mut message.metadata_json, &run_ids);
            let message = Message {
                id: MessageId::new(),
                session_id: new_session_id,
                role: message.role,
                content: message.content,
                metadata_json: message.metadata_json,
                created_at: message.created_at,
            };
            insert_message_with_time(&mut tx, new_session_id, &message).await?;
        }
        for mut document in export.documents {
            rewrite_run_references(&mut document.metadata_json, &run_ids);
            let document = Document {
                id: DocumentId::new(),
                session_id: new_session_id,
                filename: document.filename,
                content_type: document.content_type,
                raw_path: document.raw_path,
                parsed_text: document.parsed_text,
                metadata_json: document.metadata_json,
                created_at: document.created_at,
            };
            insert_document_with_time(&mut tx, new_session_id, &document).await?;
            if let Some(parsed_text) = document.parsed_text.as_deref() {
                let chunks = chunk_document(&document.filename, parsed_text);
                insert_document_chunks(&mut tx, &document, &chunks).await?;
            }
        }
        for mut event in export.skill_events {
            rewrite_run_references(&mut event.metadata_json, &run_ids);
            let event = SkillEvent {
                id: SkillEventId::new(),
                session_id: new_session_id,
                skill_id: event.skill_id,
                event_type: event.event_type,
                metadata_json: event.metadata_json,
                created_at: event.created_at,
            };
            insert_skill_event_with_time(&mut tx, new_session_id, &event).await?;
        }
        for run in export.runs {
            let new_run_id = run_ids.get(&run.id).expect("validated run map");
            sqlx::query(
                "INSERT INTO agent_runs (id, session_id, status, current_step, max_steps, token_budget, \
                 cost_budget_microusd, input_tokens, output_tokens, cost_microusd, cancel_reason, \
                 error_message, created_at, started_at, finished_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP), ?, ?)",
            )
            .bind(new_run_id)
            .bind(&new_session_id_hex)
            .bind(run.status.as_str())
            .bind(run.current_step)
            .bind(i64::from(run.max_steps))
            .bind(optional_export_integer(run.token_budget)?)
            .bind(optional_export_integer(run.cost_budget_microusd)?)
            .bind(export_integer(run.input_tokens)?)
            .bind(export_integer(run.output_tokens)?)
            .bind(export_integer(run.cost_microusd)?)
            .bind(run.cancel_reason)
            .bind(run.error_message)
            .bind(run.created_at)
            .bind(run.started_at)
            .bind(run.finished_at)
            .execute(&mut *tx)
            .await?;
        }
        for mut event in export.run_events {
            rewrite_run_references(&mut event.payload, &run_ids);
            sqlx::query(
                "INSERT INTO run_events (id, run_id, seq, kind, payload_json, created_at) \
                 VALUES (?, ?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP))",
            )
            .bind(crate::domain::RunId::new().to_legacy_hex())
            .bind(run_ids.get(&event.run_id).expect("validated event run"))
            .bind(export_integer(event.seq)?)
            .bind(event.kind)
            .bind(
                serde_json::to_string(&event.payload)
                    .map_err(|_| invalid_export("invalid event payload"))?,
            )
            .bind(event.created_at)
            .execute(&mut *tx)
            .await?;
        }
        for call in export.model_calls {
            sqlx::query(
                "INSERT INTO model_calls (id, run_id, purpose, provider, model, input_tokens, \
                 output_tokens, input_price_microusd_per_million, output_price_microusd_per_million, \
                 cost_microusd, duration_ms, finish_reason, response_id, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP))",
            )
            .bind(crate::domain::RunId::new().to_legacy_hex())
            .bind(run_ids.get(&call.run_id).expect("validated model-call run"))
            .bind(call.purpose)
            .bind(call.provider)
            .bind(call.model)
            .bind(export_integer(call.input_tokens)?)
            .bind(export_integer(call.output_tokens)?)
            .bind(export_integer(call.input_price_microusd_per_million)?)
            .bind(export_integer(call.output_price_microusd_per_million)?)
            .bind(export_integer(call.cost_microusd)?)
            .bind(optional_export_integer(call.duration_ms)?)
            .bind(call.finish_reason)
            .bind(call.response_id)
            .bind(call.created_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        Ok(ImportedSession {
            session: self.get(new_session_id).await?,
            run_ids: imported_run_ids,
        })
    }
}

impl MessageRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn add(
        &self,
        session_id: SessionId,
        role: &str,
        content: &str,
        metadata_json: Option<Value>,
    ) -> Result<Message, AppError> {
        let message = Message {
            id: MessageId::new(),
            session_id,
            role: role.to_owned(),
            content: content.to_owned(),
            metadata_json: metadata_json.unwrap_or_else(|| serde_json::json!({})),
            created_at: None,
        };
        let mut tx = self.pool.begin().await?;
        insert_message(&mut tx, session_id, &message).await?;
        tx.commit().await?;
        self.get(message.id).await
    }

    pub async fn list_by_session(&self, session_id: SessionId) -> Result<Vec<Message>, AppError> {
        let rows = sqlx::query(
            "SELECT id, session_id, role, content, metadata_json, created_at \
             FROM messages WHERE session_id = ? ORDER BY created_at ASC, rowid ASC",
        )
        .bind(session_id.to_legacy_hex())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(message_from_row).collect()
    }

    pub async fn update_metadata(
        &self,
        id: MessageId,
        metadata_json: Value,
    ) -> Result<Message, AppError> {
        let metadata = serde_json::to_string(&metadata_json).map_err(|error| {
            AppError::CorruptData(format!("could not serialize message metadata: {error}"))
        })?;
        let result = sqlx::query("UPDATE messages SET metadata_json = ? WHERE id = ?")
            .bind(metadata)
            .bind(id.to_legacy_hex())
            .execute(&self.pool)
            .await?;
        if result.rows_affected() != 1 {
            return Err(AppError::NotFound("message".to_owned()));
        }
        self.get(id).await
    }

    async fn get(&self, id: MessageId) -> Result<Message, AppError> {
        let row = sqlx::query(
            "SELECT id, session_id, role, content, metadata_json, created_at FROM messages WHERE id = ?",
        )
        .bind(id.to_legacy_hex())
        .fetch_one(&self.pool)
        .await?;
        message_from_row(&row)
    }
}

impl DocumentRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn add(
        &self,
        session_id: SessionId,
        filename: &str,
        content_type: &str,
        raw_path: Option<&str>,
        parsed_text: Option<&str>,
        metadata_json: Option<Value>,
    ) -> Result<Document, AppError> {
        let document = Document {
            id: DocumentId::new(),
            session_id,
            filename: filename.to_owned(),
            content_type: content_type.to_owned(),
            raw_path: raw_path.map(str::to_owned),
            parsed_text: parsed_text.map(str::to_owned),
            metadata_json: metadata_json.unwrap_or_else(|| serde_json::json!({})),
            created_at: None,
        };
        let mut tx = self.pool.begin().await?;
        insert_document(&mut tx, session_id, &document).await?;
        tx.commit().await?;
        self.get(document.id).await
    }

    pub async fn add_with_chunks(
        &self,
        session_id: SessionId,
        filename: &str,
        content_type: &str,
        parsed_text: &str,
        metadata_json: Option<Value>,
        chunks: &[NewDocumentChunk],
    ) -> Result<Document, AppError> {
        let document = Document {
            id: DocumentId::new(),
            session_id,
            filename: filename.to_owned(),
            content_type: content_type.to_owned(),
            raw_path: None,
            parsed_text: Some(parsed_text.to_owned()),
            metadata_json: metadata_json.unwrap_or_else(|| serde_json::json!({})),
            created_at: None,
        };
        let mut tx = self.pool.begin().await?;
        insert_document(&mut tx, session_id, &document).await?;
        insert_document_chunks(&mut tx, &document, chunks).await?;
        tx.commit().await?;
        self.get(document.id).await
    }

    pub async fn list_by_session(&self, session_id: SessionId) -> Result<Vec<Document>, AppError> {
        let rows = sqlx::query(
            "SELECT id, session_id, filename, content_type, raw_path, parsed_text, metadata_json, created_at \
             FROM documents WHERE session_id = ? ORDER BY created_at ASC, rowid ASC",
        )
        .bind(session_id.to_legacy_hex())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(document_from_row).collect()
    }

    pub async fn list_chunks_by_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<DocumentChunk>, AppError> {
        let rows = sqlx::query(
            "SELECT id, document_id, session_id, chunk_index, heading, start_char, end_char, text, search_text, created_at \
             FROM document_chunks WHERE session_id = ? ORDER BY document_id, chunk_index",
        )
        .bind(session_id.to_legacy_hex())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(document_chunk_from_row).collect()
    }

    pub async fn count_chunks(&self, document_id: DocumentId) -> Result<i64, AppError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM document_chunks WHERE document_id = ?")
                .bind(document_id.to_legacy_hex())
                .fetch_one(&self.pool)
                .await?,
        )
    }

    pub async fn delete_for_session(
        &self,
        session_id: SessionId,
        document_id: DocumentId,
    ) -> Result<bool, AppError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM document_chunks WHERE document_id = ? AND session_id = ?")
            .bind(document_id.to_legacy_hex())
            .bind(session_id.to_legacy_hex())
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM documents WHERE id = ? AND session_id = ?")
            .bind(document_id.to_legacy_hex())
            .bind(session_id.to_legacy_hex())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() == 1)
    }

    async fn get(&self, id: DocumentId) -> Result<Document, AppError> {
        let row = sqlx::query(
            "SELECT id, session_id, filename, content_type, raw_path, parsed_text, metadata_json, created_at \
             FROM documents WHERE id = ?",
        )
        .bind(id.to_legacy_hex())
        .fetch_one(&self.pool)
        .await?;
        document_from_row(&row)
    }
}

impl SkillEventRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn add(
        &self,
        session_id: SessionId,
        skill_id: &str,
        event_type: &str,
        metadata_json: Option<Value>,
    ) -> Result<SkillEvent, AppError> {
        let event = SkillEvent {
            id: SkillEventId::new(),
            session_id,
            skill_id: skill_id.to_owned(),
            event_type: event_type.to_owned(),
            metadata_json: metadata_json.unwrap_or_else(|| serde_json::json!({})),
            created_at: None,
        };
        let mut tx = self.pool.begin().await?;
        insert_skill_event(&mut tx, session_id, &event).await?;
        tx.commit().await?;
        self.get(event.id).await
    }

    pub async fn list_by_session(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<SkillEvent>, AppError> {
        let rows = sqlx::query(
            "SELECT id, session_id, skill_id, event_type, metadata_json, created_at \
             FROM skill_events WHERE session_id = ? ORDER BY created_at ASC, rowid ASC",
        )
        .bind(session_id.to_legacy_hex())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(skill_event_from_row).collect()
    }

    async fn get(&self, id: SkillEventId) -> Result<SkillEvent, AppError> {
        let row = sqlx::query(
            "SELECT id, session_id, skill_id, event_type, metadata_json, created_at \
             FROM skill_events WHERE id = ?",
        )
        .bind(id.to_legacy_hex())
        .fetch_one(&self.pool)
        .await?;
        skill_event_from_row(&row)
    }
}

impl From<Message> for TransferMessage {
    fn from(message: Message) -> Self {
        Self {
            id: message.id.to_legacy_hex(),
            session_id: message.session_id.to_legacy_hex(),
            role: message.role,
            content: message.content,
            metadata_json: message.metadata_json,
            created_at: message.created_at,
        }
    }
}

impl From<Document> for TransferDocument {
    fn from(document: Document) -> Self {
        Self {
            id: document.id.to_legacy_hex(),
            session_id: document.session_id.to_legacy_hex(),
            filename: document.filename,
            content_type: document.content_type,
            raw_path: document.raw_path,
            parsed_text: document.parsed_text,
            metadata_json: document.metadata_json,
            created_at: document.created_at,
        }
    }
}

impl From<SkillEvent> for TransferSkillEvent {
    fn from(event: SkillEvent) -> Self {
        Self {
            id: event.id.to_legacy_hex(),
            session_id: event.session_id.to_legacy_hex(),
            skill_id: event.skill_id,
            event_type: event.event_type,
            metadata_json: event.metadata_json,
            created_at: event.created_at,
        }
    }
}

async fn preflight_export(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<(), AppError> {
    let id = session_id.to_legacy_hex();
    let row = sqlx::query(
        "SELECT COALESCE(SUM(row_count), 0) AS row_count, COALESCE(SUM(byte_count), 0) AS byte_count FROM (\
         SELECT COUNT(*) AS row_count, COALESCE(SUM(\
           LENGTH(CAST(COALESCE(id,'') AS BLOB)) + LENGTH(CAST(COALESCE(user_id,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(task_type,'') AS BLOB)) + LENGTH(CAST(COALESCE(stage,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(created_at,'') AS BLOB)) + LENGTH(CAST(COALESCE(updated_at,'') AS BLOB))),0) AS byte_count \
           FROM sessions WHERE id = ? \
         UNION ALL SELECT COUNT(*), COALESCE(SUM(\
           LENGTH(CAST(COALESCE(session_id,'') AS BLOB)) + LENGTH(CAST(COALESCE(state_json,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(updated_at,'') AS BLOB))),0) FROM session_states WHERE session_id = ? \
         UNION ALL SELECT COUNT(*), COALESCE(SUM(\
           LENGTH(CAST(COALESCE(id,'') AS BLOB)) + LENGTH(CAST(COALESCE(session_id,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(role,'') AS BLOB)) + LENGTH(CAST(COALESCE(content,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(metadata_json,'') AS BLOB)) + LENGTH(CAST(COALESCE(created_at,'') AS BLOB))),0) \
           FROM messages WHERE session_id = ? \
         UNION ALL SELECT COUNT(*), COALESCE(SUM(\
           LENGTH(CAST(COALESCE(id,'') AS BLOB)) + LENGTH(CAST(COALESCE(session_id,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(filename,'') AS BLOB)) + LENGTH(CAST(COALESCE(content_type,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(raw_path,'') AS BLOB)) + LENGTH(CAST(COALESCE(parsed_text,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(metadata_json,'') AS BLOB)) + LENGTH(CAST(COALESCE(created_at,'') AS BLOB))),0) \
           FROM documents WHERE session_id = ? \
         UNION ALL SELECT COUNT(*), COALESCE(SUM(\
           LENGTH(CAST(COALESCE(id,'') AS BLOB)) + LENGTH(CAST(COALESCE(session_id,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(skill_id,'') AS BLOB)) + LENGTH(CAST(COALESCE(event_type,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(metadata_json,'') AS BLOB)) + LENGTH(CAST(COALESCE(created_at,'') AS BLOB))),0) \
           FROM skill_events WHERE session_id = ? \
         UNION ALL SELECT COUNT(*), COALESCE(SUM(\
           LENGTH(CAST(COALESCE(id,'') AS BLOB)) + LENGTH(CAST(COALESCE(session_id,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(status,'') AS BLOB)) + LENGTH(CAST(COALESCE(current_step,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(cancel_reason,'') AS BLOB)) + LENGTH(CAST(COALESCE(error_message,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(created_at,'') AS BLOB)) + LENGTH(CAST(COALESCE(started_at,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(finished_at,'') AS BLOB))),0) FROM agent_runs WHERE session_id = ? \
         UNION ALL SELECT COUNT(*), COALESCE(SUM(\
           LENGTH(CAST(COALESCE(e.id,'') AS BLOB)) + LENGTH(CAST(COALESCE(e.run_id,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(e.kind,'') AS BLOB)) + LENGTH(CAST(COALESCE(e.payload_json,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(e.created_at,'') AS BLOB))),0) \
           FROM run_events e JOIN agent_runs r ON r.id = e.run_id WHERE r.session_id = ? \
         UNION ALL SELECT COUNT(*), COALESCE(SUM(\
           LENGTH(CAST(COALESCE(c.id,'') AS BLOB)) + LENGTH(CAST(COALESCE(c.run_id,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(c.purpose,'') AS BLOB)) + LENGTH(CAST(COALESCE(c.provider,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(c.model,'') AS BLOB)) + LENGTH(CAST(COALESCE(c.finish_reason,'') AS BLOB)) + \
           LENGTH(CAST(COALESCE(c.response_id,'') AS BLOB)) + LENGTH(CAST(COALESCE(c.created_at,'') AS BLOB))),0) \
           FROM model_calls c JOIN agent_runs r ON r.id = c.run_id WHERE r.session_id = ?\
         )",
    )
    .bind(&id)
    .bind(&id)
    .bind(&id)
    .bind(&id)
    .bind(&id)
    .bind(&id)
    .bind(&id)
    .bind(&id)
    .fetch_one(&mut **tx)
    .await?;
    let rows: i64 = row.try_get("row_count")?;
    let bytes: i64 = row.try_get("byte_count")?;
    let rows = usize::try_from(rows).map_err(|_| invalid_export("invalid record count"))?;
    let bytes = usize::try_from(bytes).map_err(|_| invalid_export("invalid byte count"))?;
    // Six bytes covers the largest JSON escape for one UTF-8 input byte. The fixed
    // per-row allowance dominates every field name, delimiter, integer, null and bool
    // in the version-1 row variants; the envelope covers schema and collection syntax.
    let conservative_bytes = bytes
        .checked_mul(JSON_WORST_CASE_EXPANSION)
        .and_then(|value| {
            rows.checked_mul(JSON_ROW_OVERHEAD)
                .and_then(|overhead| value.checked_add(overhead))
        })
        .and_then(|value| value.checked_add(JSON_ENVELOPE_OVERHEAD))
        .ok_or_else(|| invalid_export("session export exceeds bounds"))?;
    if rows > MAX_SESSION_EXPORT_ROWS || conservative_bytes > MAX_SESSION_EXPORT_BYTES {
        return Err(invalid_export("session export exceeds bounds"));
    }
    Ok(())
}

async fn export_session(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<Session, AppError> {
    let row = sqlx::query(
        "SELECT id, user_id, task_type, stage, created_at, updated_at FROM sessions WHERE id = ? LIMIT 1",
    )
    .bind(session_id.to_legacy_hex())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::NotFound("session".to_owned()))?;
    session_from_row(&row)
}

async fn export_state(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<SessionState, AppError> {
    let row = sqlx::query(
        "SELECT session_id, state_json, updated_at FROM session_states WHERE session_id = ? LIMIT 1",
    )
    .bind(session_id.to_legacy_hex())
    .fetch_optional(&mut **tx)
    .await?
    .ok_or_else(|| AppError::NotFound("session state".to_owned()))?;
    state_from_row(&row)
}

async fn export_messages(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<Vec<Message>, AppError> {
    let rows = sqlx::query(
        "SELECT id, session_id, role, content, metadata_json, created_at FROM messages \
         WHERE session_id = ? ORDER BY created_at ASC, rowid ASC LIMIT ?",
    )
    .bind(session_id.to_legacy_hex())
    .bind(i64::try_from(MAX_SESSION_EXPORT_ROWS + 1).unwrap())
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(message_from_row).collect()
}

async fn export_documents(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<Vec<Document>, AppError> {
    let rows = sqlx::query(
        "SELECT id, session_id, filename, content_type, raw_path, parsed_text, metadata_json, created_at \
         FROM documents WHERE session_id = ? ORDER BY created_at ASC, rowid ASC LIMIT ?",
    )
    .bind(session_id.to_legacy_hex())
    .bind(i64::try_from(MAX_SESSION_EXPORT_ROWS + 1).unwrap())
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(document_from_row).collect()
}

async fn export_skill_events(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<Vec<SkillEvent>, AppError> {
    let rows = sqlx::query(
        "SELECT id, session_id, skill_id, event_type, metadata_json, created_at FROM skill_events \
         WHERE session_id = ? ORDER BY created_at ASC, rowid ASC LIMIT ?",
    )
    .bind(session_id.to_legacy_hex())
    .bind(i64::try_from(MAX_SESSION_EXPORT_ROWS + 1).unwrap())
    .fetch_all(&mut **tx)
    .await?;
    rows.iter().map(skill_event_from_row).collect()
}

async fn export_runs(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<Vec<TransferRun>, AppError> {
    let rows = sqlx::query(
        "SELECT id, session_id, status, current_step, max_steps, token_budget, \
         cost_budget_microusd, input_tokens, output_tokens, cost_microusd, cancel_reason, \
         error_message, created_at, started_at, finished_at FROM agent_runs \
         WHERE session_id = ? ORDER BY created_at ASC, rowid ASC LIMIT ?",
    )
    .bind(session_id.to_legacy_hex())
    .bind(i64::try_from(MAX_SESSION_EXPORT_ROWS + 1).unwrap())
    .fetch_all(&mut **tx)
    .await?;
    rows.iter()
        .map(|row| {
            let status: String = row.try_get("status")?;
            Ok(TransferRun {
                id: validated_database_id(row.try_get("id")?)?,
                session_id: validated_database_id(row.try_get("session_id")?)?,
                status: RunStatus::from_database(&status)
                    .ok_or_else(|| AppError::CorruptData("invalid run status".to_owned()))?,
                current_step: row.try_get("current_step")?,
                max_steps: u32::try_from(row.try_get::<i64, _>("max_steps")?)
                    .map_err(|_| AppError::CorruptData("invalid run max_steps".to_owned()))?,
                token_budget: optional_database_u64(row.try_get("token_budget")?)?,
                cost_budget_microusd: optional_database_u64(row.try_get("cost_budget_microusd")?)?,
                input_tokens: database_u64(row.try_get("input_tokens")?)?,
                output_tokens: database_u64(row.try_get("output_tokens")?)?,
                cost_microusd: database_u64(row.try_get("cost_microusd")?)?,
                cancel_reason: row.try_get("cancel_reason")?,
                error_message: row.try_get("error_message")?,
                created_at: row.try_get("created_at")?,
                started_at: row.try_get("started_at")?,
                finished_at: row.try_get("finished_at")?,
            })
        })
        .collect()
}

async fn export_run_events(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<Vec<TransferRunEvent>, AppError> {
    let rows = sqlx::query(
        "SELECT e.id, e.run_id, e.seq, e.kind, e.payload_json, e.created_at \
         FROM run_events e JOIN agent_runs r ON r.id = e.run_id \
         WHERE r.session_id = ? ORDER BY r.created_at ASC, r.rowid ASC, e.seq ASC LIMIT ?",
    )
    .bind(session_id.to_legacy_hex())
    .bind(i64::try_from(MAX_SESSION_EXPORT_ROWS + 1).unwrap())
    .fetch_all(&mut **tx)
    .await?;
    rows.iter()
        .map(|row| {
            let payload: String = row.try_get("payload_json")?;
            Ok(TransferRunEvent {
                id: validated_database_id(row.try_get("id")?)?,
                run_id: validated_database_id(row.try_get("run_id")?)?,
                seq: database_u64(row.try_get("seq")?)?,
                kind: row.try_get("kind")?,
                payload: serde_json::from_str(&payload).map_err(|_| {
                    AppError::CorruptData("invalid persisted run event payload".to_owned())
                })?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

async fn export_model_calls(
    tx: &mut Transaction<'_, Sqlite>,
    session_id: SessionId,
) -> Result<Vec<TransferModelCall>, AppError> {
    let rows = sqlx::query(
        "SELECT c.id, c.run_id, c.purpose, c.provider, c.model, c.input_tokens, \
         c.output_tokens, c.input_price_microusd_per_million, \
         c.output_price_microusd_per_million, c.cost_microusd, c.duration_ms, \
         c.finish_reason, c.response_id, c.created_at \
         FROM model_calls c JOIN agent_runs r ON r.id = c.run_id \
         WHERE r.session_id = ? ORDER BY r.created_at ASC, r.rowid ASC, c.created_at ASC, c.rowid ASC LIMIT ?",
    )
    .bind(session_id.to_legacy_hex())
    .bind(i64::try_from(MAX_SESSION_EXPORT_ROWS + 1).unwrap())
    .fetch_all(&mut **tx)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(TransferModelCall {
                id: validated_database_id(row.try_get("id")?)?,
                run_id: validated_database_id(row.try_get("run_id")?)?,
                purpose: row.try_get("purpose")?,
                provider: row.try_get("provider")?,
                model: row.try_get("model")?,
                input_tokens: database_u64(row.try_get("input_tokens")?)?,
                output_tokens: database_u64(row.try_get("output_tokens")?)?,
                input_price_microusd_per_million: database_u64(
                    row.try_get("input_price_microusd_per_million")?,
                )?,
                output_price_microusd_per_million: database_u64(
                    row.try_get("output_price_microusd_per_million")?,
                )?,
                cost_microusd: database_u64(row.try_get("cost_microusd")?)?,
                duration_ms: optional_database_u64(row.try_get("duration_ms")?)?,
                finish_reason: row.try_get("finish_reason")?,
                response_id: row.try_get("response_id")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

fn validate_export(export: &SessionExportV1) -> Result<(), AppError> {
    if export.schema != SESSION_EXPORT_SCHEMA || export.version != 1 {
        return Err(invalid_export("unsupported schema or version"));
    }
    let source = validate_id::<SessionId>(&export.source_session_id)?;
    if validate_id::<SessionId>(&export.session.id)? != source
        || validate_id::<SessionId>(&export.state.session_id)? != source
        || export.session.stage.trim().is_empty()
        || !export.state.state_json.is_object()
    {
        return Err(invalid_export("invalid source session"));
    }
    reject_strings([
        Some(export.schema.as_str()),
        export.session.user_id.as_deref(),
        export.session.task_type.as_deref(),
        Some(export.session.stage.as_str()),
    ])?;
    validate_timestamp_order(
        export.session.created_at.as_deref(),
        export.session.updated_at.as_deref(),
    )?;
    validate_optional_timestamp(export.state.updated_at.as_deref())?;
    let row_count = export
        .messages
        .len()
        .saturating_add(export.documents.len())
        .saturating_add(export.skill_events.len())
        .saturating_add(export.runs.len())
        .saturating_add(export.run_events.len())
        .saturating_add(export.model_calls.len());
    if row_count > MAX_SESSION_EXPORT_ROWS {
        return Err(invalid_export("too many records"));
    }

    let mut ids = HashSet::new();
    for message in &export.messages {
        validate_unique_id::<MessageId>(&message.id, &mut ids)?;
        validate_child_session(&message.session_id, source)?;
        reject_secret_material(&message.metadata_json)?;
        reject_strings([Some(message.role.as_str()), Some(message.content.as_str())])?;
        validate_optional_timestamp(message.created_at.as_deref())?;
        if message.role.trim().is_empty() {
            return Err(invalid_export("invalid message"));
        }
    }
    ids.clear();
    for document in &export.documents {
        validate_unique_id::<DocumentId>(&document.id, &mut ids)?;
        validate_child_session(&document.session_id, source)?;
        reject_secret_material(&document.metadata_json)?;
        reject_strings([
            Some(document.filename.as_str()),
            Some(document.content_type.as_str()),
            document.raw_path.as_deref(),
            document.parsed_text.as_deref(),
        ])?;
        validate_optional_timestamp(document.created_at.as_deref())?;
        if document.filename.trim().is_empty() || document.content_type.trim().is_empty() {
            return Err(invalid_export("invalid document"));
        }
    }
    ids.clear();
    for event in &export.skill_events {
        validate_unique_id::<SkillEventId>(&event.id, &mut ids)?;
        validate_child_session(&event.session_id, source)?;
        reject_secret_material(&event.metadata_json)?;
        reject_strings([
            Some(event.skill_id.as_str()),
            Some(event.event_type.as_str()),
        ])?;
        validate_optional_timestamp(event.created_at.as_deref())?;
        if event.skill_id.trim().is_empty() || event.event_type.trim().is_empty() {
            return Err(invalid_export("invalid skill event"));
        }
    }
    reject_secret_material(&export.state.state_json)?;

    ids.clear();
    let mut run_ids = HashSet::new();
    let mut run_times = HashMap::new();
    for run in &export.runs {
        validate_unique_id::<crate::domain::RunId>(&run.id, &mut ids)?;
        validate_child_session(&run.session_id, source)?;
        if run.max_steps == 0 || !run.status.is_terminal() {
            return Err(invalid_export("invalid run"));
        }
        run_ids.insert(run.id.as_str());
        reject_strings([
            run.current_step.as_deref(),
            run.cancel_reason.as_deref(),
            run.error_message.as_deref(),
        ])?;
        let created = parse_required_timestamp(run.created_at.as_deref())?;
        let started = validate_optional_timestamp(run.started_at.as_deref())?;
        let finished = parse_required_timestamp(run.finished_at.as_deref())?;
        if created > finished
            || started.is_some_and(|started| started < created || started > finished)
        {
            return Err(invalid_export("invalid run timestamps"));
        }
        run_times.insert(run.id.as_str(), (run.status, created, finished));
        export_integer(run.input_tokens)?;
        export_integer(run.output_tokens)?;
        export_integer(run.cost_microusd)?;
        optional_export_integer(run.token_budget)?;
        optional_export_integer(run.cost_budget_microusd)?;
    }
    validate_embedded_run_references(&export.state.state_json, &run_ids)?;
    for value in export
        .messages
        .iter()
        .map(|message| &message.metadata_json)
        .chain(
            export
                .documents
                .iter()
                .map(|document| &document.metadata_json),
        )
        .chain(export.skill_events.iter().map(|event| &event.metadata_json))
        .chain(export.run_events.iter().map(|event| &event.payload))
    {
        validate_embedded_run_references(value, &run_ids)?;
    }
    ids.clear();
    let mut sequences = HashSet::new();
    let mut events_by_run: HashMap<&str, Vec<&TransferRunEvent>> = HashMap::new();
    for event in &export.run_events {
        validate_unique_id::<crate::domain::RunId>(&event.id, &mut ids)?;
        validate_id::<crate::domain::RunId>(&event.run_id)?;
        if !run_ids.contains(event.run_id.as_str())
            || event.seq == 0
            || !sequences.insert((event.run_id.as_str(), event.seq))
            || event.kind.trim().is_empty()
        {
            return Err(invalid_export("invalid run event"));
        }
        export_integer(event.seq)?;
        reject_secret_material(&event.payload)?;
        reject_strings([Some(event.kind.as_str())])?;
        let created = parse_required_timestamp(event.created_at.as_deref())?;
        let (_, run_created, run_finished) = run_times
            .get(event.run_id.as_str())
            .ok_or_else(|| invalid_export("invalid event run"))?;
        if created < *run_created || created > *run_finished {
            return Err(invalid_export("invalid event timestamp"));
        }
        events_by_run
            .entry(event.run_id.as_str())
            .or_default()
            .push(event);
    }
    for run in &export.runs {
        let events = events_by_run
            .get_mut(run.id.as_str())
            .ok_or_else(|| invalid_export("run has no events"))?;
        events.sort_unstable_by_key(|event| event.seq);
        for (index, event) in events.iter().enumerate() {
            if event.seq != u64::try_from(index + 1).unwrap() {
                return Err(invalid_export("run event sequence is not contiguous"));
            }
        }
        let terminal = events
            .iter()
            .filter(|event| event_is_terminal(&event.kind))
            .count();
        let final_event = events.last().expect("non-empty validated events");
        let expected_kind = run
            .status
            .terminal_event_kind()
            .ok_or_else(|| invalid_export("run is not terminal"))?;
        if terminal != 1 || final_event.kind != expected_kind {
            return Err(invalid_export("terminal event does not match run"));
        }
        let final_time = parse_required_timestamp(final_event.created_at.as_deref())?;
        let (_, _, finished) = run_times[run.id.as_str()];
        if final_time != finished {
            return Err(invalid_export("terminal timestamp does not match run"));
        }
    }
    ids.clear();
    let mut model_call_totals: HashMap<&str, (u64, u64, u64)> = export
        .runs
        .iter()
        .map(|run| (run.id.as_str(), (0, 0, 0)))
        .collect();
    for call in &export.model_calls {
        validate_unique_id::<crate::domain::RunId>(&call.id, &mut ids)?;
        validate_id::<crate::domain::RunId>(&call.run_id)?;
        if !run_ids.contains(call.run_id.as_str())
            || call.purpose.trim().is_empty()
            || call.provider.trim().is_empty()
            || call.model.trim().is_empty()
        {
            return Err(invalid_export("invalid model call"));
        }
        reject_strings([
            Some(call.purpose.as_str()),
            Some(call.provider.as_str()),
            Some(call.model.as_str()),
            call.finish_reason.as_deref(),
            call.response_id.as_deref(),
        ])?;
        let created = parse_required_timestamp(call.created_at.as_deref())?;
        let (_, run_created, run_finished) = run_times[call.run_id.as_str()];
        if created < run_created || created > run_finished {
            return Err(invalid_export("invalid model-call timestamp"));
        }
        for value in [
            call.input_tokens,
            call.output_tokens,
            call.input_price_microusd_per_million,
            call.output_price_microusd_per_million,
            call.cost_microusd,
        ] {
            export_integer(value)?;
        }
        optional_export_integer(call.duration_ms)?;
        let recalculated = calculate_cost(
            Usage {
                input_tokens: call.input_tokens,
                output_tokens: call.output_tokens,
            },
            PriceSnapshot {
                input_microusd_per_million: call.input_price_microusd_per_million,
                output_microusd_per_million: call.output_price_microusd_per_million,
            },
        )
        .map_err(|_| invalid_export("invalid model-call cost"))?;
        if recalculated.0 != call.cost_microusd {
            return Err(invalid_export("invalid model-call cost"));
        }
        let totals = model_call_totals
            .get_mut(call.run_id.as_str())
            .ok_or_else(|| invalid_export("invalid model-call run"))?;
        totals.0 = totals
            .0
            .checked_add(call.input_tokens)
            .ok_or_else(|| invalid_export("model-call usage overflow"))?;
        totals.1 = totals
            .1
            .checked_add(call.output_tokens)
            .ok_or_else(|| invalid_export("model-call usage overflow"))?;
        totals.2 = totals
            .2
            .checked_add(call.cost_microusd)
            .ok_or_else(|| invalid_export("model-call cost overflow"))?;
    }
    for run in &export.runs {
        if model_call_totals[run.id.as_str()]
            != (run.input_tokens, run.output_tokens, run.cost_microusd)
        {
            return Err(invalid_export("model-call totals do not match run"));
        }
    }
    Ok(())
}

trait TransferId: Sized + Copy + Eq + std::hash::Hash {
    fn parse(value: &str) -> Result<Self, AppError>;
}

macro_rules! transfer_id {
    ($type:ty) => {
        impl TransferId for $type {
            fn parse(value: &str) -> Result<Self, AppError> {
                <$type>::parse_legacy(value)
            }
        }
    };
}

transfer_id!(SessionId);
transfer_id!(MessageId);
transfer_id!(DocumentId);
transfer_id!(SkillEventId);
transfer_id!(crate::domain::RunId);

fn validate_id<T: TransferId>(value: &str) -> Result<T, AppError> {
    T::parse(value).map_err(|_| invalid_export("invalid identifier"))
}

fn validate_unique_id<T: TransferId>(
    value: &str,
    ids: &mut HashSet<String>,
) -> Result<T, AppError> {
    let parsed = validate_id(value)?;
    if !ids.insert(value.to_owned()) {
        return Err(invalid_export("duplicate identifier"));
    }
    Ok(parsed)
}

fn validate_child_session(value: &str, source: SessionId) -> Result<(), AppError> {
    if validate_id::<SessionId>(value)? != source {
        return Err(invalid_export("foreign key mismatch"));
    }
    Ok(())
}

fn ensure_export_size(export: &SessionExportV1) -> Result<(), AppError> {
    let size = serde_json::to_vec(export)
        .map_err(|_| invalid_export("could not serialize export"))?
        .len();
    if size > MAX_SESSION_EXPORT_BYTES {
        return Err(invalid_export("export exceeds size limit"));
    }
    Ok(())
}

fn reject_secret_material(value: &Value) -> Result<(), AppError> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if sensitive_key(key) {
                    return Err(invalid_export("secret material is not transferable"));
                }
                reject_secret_material(value)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                reject_secret_material(value)?;
            }
        }
        Value::String(value) => reject_secret_string(value)?,
        _ => {}
    }
    Ok(())
}

fn reject_strings<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Result<(), AppError> {
    for value in values.into_iter().flatten() {
        reject_secret_string(value)?;
    }
    Ok(())
}

fn sensitive_key(key: &str) -> bool {
    let tokens = identifier_tokens(key);
    let compact = tokens.concat();
    tokens
        .iter()
        .any(|token| token == "authorization" || token == "secret")
        || tokens.windows(2).any(|pair| {
            matches!(
                (pair[0].as_str(), pair[1].as_str()),
                ("api", "key") | ("access", "token") | ("client", "secret")
            )
        })
        || compact.ends_with("apikey")
        || compact.ends_with("accesstoken")
        || compact.ends_with("clientsecret")
}

fn identifier_tokens(value: &str) -> Vec<String> {
    let characters = value.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut current = String::new();
    for (index, character) in characters.iter().copied().enumerate() {
        if !character.is_ascii_alphanumeric() {
            if !current.is_empty() {
                tokens.push(current.to_ascii_lowercase());
                current.clear();
            }
            continue;
        }
        let previous = index.checked_sub(1).and_then(|index| characters.get(index));
        let next = characters.get(index + 1);
        let camel_boundary = character.is_ascii_uppercase()
            && !current.is_empty()
            && (previous.is_some_and(|value| value.is_ascii_lowercase())
                || (previous.is_some_and(|value| value.is_ascii_uppercase())
                    && next.is_some_and(|value| value.is_ascii_lowercase())));
        if camel_boundary {
            tokens.push(current.to_ascii_lowercase());
            current.clear();
        }
        current.push(character);
    }
    if !current.is_empty() {
        tokens.push(current.to_ascii_lowercase());
    }
    tokens
}

fn reject_secret_string(value: &str) -> Result<(), AppError> {
    let lower = value.to_ascii_lowercase();
    if lower.match_indices("authorization").any(|(index, _)| {
        let tail = &lower[index..lower.len().min(index.saturating_add(96))];
        tail.contains("bearer ") || tail.contains("basic ")
    }) || contains_credential_prefix(&lower)
        || contains_sensitive_assignment(value)
    {
        return Err(invalid_export("secret material is not transferable"));
    }
    Ok(())
}

fn contains_credential_prefix(value: &str) -> bool {
    [
        ("sk-", 20),
        ("sk_live_", 16),
        ("sk_test_", 16),
        ("pk_live_", 16),
        ("pk_test_", 16),
        ("ghp_", 20),
        ("gho_", 20),
        ("ghu_", 20),
        ("ghs_", 20),
        ("ghr_", 20),
        ("xoxb-", 20),
        ("xoxp-", 20),
    ]
    .into_iter()
    .any(|(prefix, minimum)| {
        value.match_indices(prefix).any(|(index, _)| {
            value[index + prefix.len()..]
                .chars()
                .take_while(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
                .take(128)
                .count()
                >= minimum
        })
    })
}

fn contains_sensitive_assignment(value: &str) -> bool {
    value.char_indices().any(|(separator, character)| {
        if !matches!(character, ':' | '=') {
            return false;
        }
        let lower_bound = separator.saturating_sub(96);
        let mut start = lower_bound;
        while start < separator && !value.is_char_boundary(start) {
            start += 1;
        }
        let key_window = &value[start..separator];
        let key_start = key_window
            .char_indices()
            .rev()
            .find_map(|(index, character)| {
                matches!(character, '\n' | '\r' | ';' | '&' | '?' | '{' | '[' | ',')
                    .then_some(index + character.len_utf8())
            })
            .unwrap_or(0);
        sensitive_key(&key_window[key_start..]) && assigned_value_is_opaque(&value[separator + 1..])
    })
}

fn assigned_value_is_opaque(value: &str) -> bool {
    let value = value.trim_start();
    let (value, closing_quote) = match value.chars().next() {
        Some(character @ ('\'' | '"')) => (&value[character.len_utf8()..], Some(character)),
        _ => (value, None),
    };
    value
        .chars()
        .take(256)
        .take_while(|character| {
            if closing_quote.is_some_and(|quote| quote == *character) {
                return false;
            }
            !character.is_whitespace() && !matches!(character, '&' | ';' | ',')
        })
        .count()
        >= 12
}

fn validate_optional_timestamp(value: Option<&str>) -> Result<Option<DateTime<Utc>>, AppError> {
    value.map(parse_timestamp).transpose()
}

fn parse_required_timestamp(value: Option<&str>) -> Result<DateTime<Utc>, AppError> {
    value
        .ok_or_else(|| invalid_export("required timestamp is missing"))
        .and_then(parse_timestamp)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, AppError> {
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(timestamp) = NaiveDateTime::parse_from_str(value, format) {
            return Ok(timestamp.and_utc());
        }
    }
    Err(invalid_export("invalid timestamp"))
}

fn validate_timestamp_order(created: Option<&str>, updated: Option<&str>) -> Result<(), AppError> {
    let created = validate_optional_timestamp(created)?;
    let updated = validate_optional_timestamp(updated)?;
    if created
        .zip(updated)
        .is_some_and(|(created, updated)| created > updated)
    {
        return Err(invalid_export("invalid timestamp order"));
    }
    Ok(())
}

fn event_is_terminal(kind: &str) -> bool {
    matches!(
        kind,
        "run.completed" | "run.failed" | "run.cancelled" | "run.budget_exceeded"
    )
}

fn rewrite_run_references(value: &mut Value, run_ids: &HashMap<String, String>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_run_reference_key(key)
                    && let Some(old_id) = value.as_str()
                    && let Some(new_id) = run_ids.get(old_id)
                {
                    *value = Value::String(new_id.clone());
                } else {
                    rewrite_run_references(value, run_ids);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                rewrite_run_references(value, run_ids);
            }
        }
        _ => {}
    }
}

fn validate_embedded_run_references(
    value: &Value,
    run_ids: &HashSet<&str>,
) -> Result<(), AppError> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_run_reference_key(key) {
                    let run_id = value
                        .as_str()
                        .ok_or_else(|| invalid_export("embedded run reference is invalid"))?;
                    validate_id::<crate::domain::RunId>(run_id)?;
                    if !run_ids.contains(run_id) {
                        return Err(invalid_export("embedded run reference is orphaned"));
                    }
                } else {
                    validate_embedded_run_references(value, run_ids)?;
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_embedded_run_references(value, run_ids)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_run_reference_key(key: &str) -> bool {
    let tokens = identifier_tokens(key);
    tokens
        .windows(2)
        .any(|pair| pair[0] == "run" && pair[1] == "id")
        || tokens.concat().ends_with("runid")
}

fn invalid_export(message: &str) -> AppError {
    AppError::InvalidExport(message.to_owned())
}

fn validated_database_id(value: String) -> Result<String, AppError> {
    crate::domain::RunId::parse_legacy(&value)?;
    Ok(value)
}

fn database_u64(value: i64) -> Result<u64, AppError> {
    u64::try_from(value).map_err(|_| AppError::CorruptData("negative trajectory value".to_owned()))
}

fn optional_database_u64(value: Option<i64>) -> Result<Option<u64>, AppError> {
    value.map(database_u64).transpose()
}

fn export_integer(value: u64) -> Result<i64, AppError> {
    i64::try_from(value).map_err(|_| invalid_export("integer exceeds persistence range"))
}

fn optional_export_integer(value: Option<u64>) -> Result<Option<i64>, AppError> {
    value.map(export_integer).transpose()
}

fn stage_from_state(state: &Value) -> Option<&str> {
    state
        .get("writing_context")
        .and_then(|context| context.get("stage"))
        .and_then(Value::as_str)
        .or_else(|| state.get("stage").and_then(Value::as_str))
}

fn parse_json(value: Option<String>, column: &str) -> Result<Value, AppError> {
    value.map_or(Ok(Value::Null), |json| {
        serde_json::from_str(&json)
            .map_err(|error| AppError::CorruptData(format!("invalid JSON in {column}: {error}")))
    })
}

fn session_from_row(row: &SqliteRow) -> Result<Session, AppError> {
    Ok(Session {
        id: SessionId::parse_legacy(&row.try_get::<String, _>("id")?)?,
        user_id: row.try_get("user_id")?,
        task_type: row.try_get("task_type")?,
        stage: row.try_get("stage")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn state_from_row(row: &SqliteRow) -> Result<SessionState, AppError> {
    Ok(SessionState {
        session_id: SessionId::parse_legacy(&row.try_get::<String, _>("session_id")?)?,
        state_json: parse_json(row.try_get("state_json")?, "session_states.state_json")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn message_from_row(row: &SqliteRow) -> Result<Message, AppError> {
    Ok(Message {
        id: MessageId::parse_legacy(&row.try_get::<String, _>("id")?)?,
        session_id: SessionId::parse_legacy(&row.try_get::<String, _>("session_id")?)?,
        role: row.try_get("role")?,
        content: row.try_get("content")?,
        metadata_json: parse_json(row.try_get("metadata_json")?, "messages.metadata_json")?,
        created_at: row.try_get("created_at")?,
    })
}

fn document_from_row(row: &SqliteRow) -> Result<Document, AppError> {
    Ok(Document {
        id: DocumentId::parse_legacy(&row.try_get::<String, _>("id")?)?,
        session_id: SessionId::parse_legacy(&row.try_get::<String, _>("session_id")?)?,
        filename: row.try_get("filename")?,
        content_type: row.try_get("content_type")?,
        raw_path: row.try_get("raw_path")?,
        parsed_text: row.try_get("parsed_text")?,
        metadata_json: parse_json(row.try_get("metadata_json")?, "documents.metadata_json")?,
        created_at: row.try_get("created_at")?,
    })
}

fn document_chunk_from_row(row: &SqliteRow) -> Result<DocumentChunk, AppError> {
    let chunk_index = row.try_get::<i64, _>("chunk_index")?;
    let start_char = row.try_get::<i64, _>("start_char")?;
    let end_char = row.try_get::<i64, _>("end_char")?;
    Ok(DocumentChunk {
        id: DocumentChunkId::parse_legacy(&row.try_get::<String, _>("id")?)?,
        document_id: DocumentId::parse_legacy(&row.try_get::<String, _>("document_id")?)?,
        session_id: SessionId::parse_legacy(&row.try_get::<String, _>("session_id")?)?,
        chunk_index: usize::try_from(chunk_index)
            .map_err(|_| AppError::CorruptData("invalid document chunk index".to_owned()))?,
        heading: row.try_get("heading")?,
        start_char: usize::try_from(start_char)
            .map_err(|_| AppError::CorruptData("invalid document chunk start".to_owned()))?,
        end_char: usize::try_from(end_char)
            .map_err(|_| AppError::CorruptData("invalid document chunk end".to_owned()))?,
        text: row.try_get("text")?,
        search_text: row.try_get("search_text")?,
        created_at: row.try_get("created_at")?,
    })
}

fn skill_event_from_row(row: &SqliteRow) -> Result<SkillEvent, AppError> {
    Ok(SkillEvent {
        id: SkillEventId::parse_legacy(&row.try_get::<String, _>("id")?)?,
        session_id: SessionId::parse_legacy(&row.try_get::<String, _>("session_id")?)?,
        skill_id: row.try_get("skill_id")?,
        event_type: row.try_get("event_type")?,
        metadata_json: parse_json(row.try_get("metadata_json")?, "skill_events.metadata_json")?,
        created_at: row.try_get("created_at")?,
    })
}

async fn insert_message(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: SessionId,
    message: &Message,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, metadata_json, created_at) \
         VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
    )
    .bind(message.id.to_legacy_hex())
    .bind(session_id.to_legacy_hex())
    .bind(&message.role)
    .bind(&message.content)
    .bind(
        serde_json::to_string(&message.metadata_json).map_err(|error| {
            AppError::CorruptData(format!("could not serialize message metadata: {error}"))
        })?,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_message_with_time(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: SessionId,
    message: &Message,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO messages (id, session_id, role, content, metadata_json, created_at) \
         VALUES (?, ?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP))",
    )
    .bind(message.id.to_legacy_hex())
    .bind(session_id.to_legacy_hex())
    .bind(&message.role)
    .bind(&message.content)
    .bind(
        serde_json::to_string(&message.metadata_json)
            .map_err(|_| invalid_export("invalid message metadata"))?,
    )
    .bind(&message.created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_document(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: SessionId,
    document: &Document,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO documents (id, session_id, filename, content_type, raw_path, parsed_text, metadata_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
    )
    .bind(document.id.to_legacy_hex())
    .bind(session_id.to_legacy_hex())
    .bind(&document.filename)
    .bind(&document.content_type)
    .bind(&document.raw_path)
    .bind(&document.parsed_text)
    .bind(serde_json::to_string(&document.metadata_json).map_err(|error| {
        AppError::CorruptData(format!("could not serialize document metadata: {error}"))
    })?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_document_with_time(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: SessionId,
    document: &Document,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO documents (id, session_id, filename, content_type, raw_path, parsed_text, metadata_json, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP))",
    )
    .bind(document.id.to_legacy_hex())
    .bind(session_id.to_legacy_hex())
    .bind(&document.filename)
    .bind(&document.content_type)
    .bind(&document.raw_path)
    .bind(&document.parsed_text)
    .bind(
        serde_json::to_string(&document.metadata_json)
            .map_err(|_| invalid_export("invalid document metadata"))?,
    )
    .bind(&document.created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_document_chunks(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    document: &Document,
    chunks: &[NewDocumentChunk],
) -> Result<(), AppError> {
    for chunk in chunks {
        sqlx::query(
            "INSERT INTO document_chunks (id, document_id, session_id, chunk_index, heading, start_char, end_char, text, search_text, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(DocumentChunkId::new().to_legacy_hex())
        .bind(document.id.to_legacy_hex())
        .bind(document.session_id.to_legacy_hex())
        .bind(i64::try_from(chunk.chunk_index).map_err(|_| {
            AppError::CorruptData("chunk index exceeds SQLite integer range".to_owned())
        })?)
        .bind(&chunk.heading)
        .bind(i64::try_from(chunk.start_char).map_err(|_| {
            AppError::CorruptData("chunk start exceeds SQLite integer range".to_owned())
        })?)
        .bind(i64::try_from(chunk.end_char).map_err(|_| {
            AppError::CorruptData("chunk end exceeds SQLite integer range".to_owned())
        })?)
        .bind(&chunk.text)
        .bind(&chunk.search_text)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn insert_skill_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: SessionId,
    event: &SkillEvent,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO skill_events (id, session_id, skill_id, event_type, metadata_json, created_at) \
         VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
    )
    .bind(event.id.to_legacy_hex())
    .bind(session_id.to_legacy_hex())
    .bind(&event.skill_id)
    .bind(&event.event_type)
    .bind(serde_json::to_string(&event.metadata_json).map_err(|error| {
        AppError::CorruptData(format!("could not serialize skill-event metadata: {error}"))
    })?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_skill_event_with_time(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    session_id: SessionId,
    event: &SkillEvent,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO skill_events (id, session_id, skill_id, event_type, metadata_json, created_at) \
         VALUES (?, ?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP))",
    )
    .bind(event.id.to_legacy_hex())
    .bind(session_id.to_legacy_hex())
    .bind(&event.skill_id)
    .bind(&event.event_type)
    .bind(
        serde_json::to_string(&event.metadata_json)
            .map_err(|_| invalid_export("invalid skill-event metadata"))?,
    )
    .bind(&event.created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use serde_json::json;
    use tokio::{
        sync::Notify,
        time::{Duration, timeout},
    };
    use uuid::Uuid;

    use super::{MessageRepository, SessionRepository, preflight_export};
    use crate::store::sqlite;

    const WAIT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn export_uses_one_snapshot_when_a_writer_commits_after_preflight() {
        // Break caught: export combines parent/state rows from one SQLite moment with later children.
        let path = std::env::temp_dir().join(format!(
            "writing-coach-export-snapshot-{}.db",
            Uuid::new_v4().simple()
        ));
        let url = format!("sqlite://{}", path.display());
        let pool = sqlite::open_database(&url).await.unwrap();
        sqlite::migrate(&pool).await.unwrap();
        let session = SessionRepository::new(pool.clone())
            .create(Some("snapshot-user"))
            .await
            .unwrap();
        MessageRepository::new(pool.clone())
            .add(session.id, "user", "before-snapshot", Some(json!({})))
            .await
            .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let repository = SessionRepository::new(pool.clone())
            .with_export_snapshot_test_gate(entered.clone(), release.clone());
        let export = tokio::spawn(async move { repository.export_v1(session.id).await.unwrap() });
        timeout(WAIT, entered.notified()).await.unwrap();
        sqlx::query("UPDATE messages SET content = 'after-snapshot' WHERE session_id = ?")
            .bind(session.id.to_legacy_hex())
            .execute(&pool)
            .await
            .unwrap();
        release.notify_one();
        let export = timeout(WAIT, export).await.unwrap().unwrap();
        assert_eq!(export.messages[0].content, "before-snapshot");
        pool.close().await;
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("db-shm"));
        let _ = fs::remove_file(path.with_extension("db-wal"));
    }

    #[tokio::test]
    async fn preflight_counts_utf8_json_escaping_and_embedded_nul_before_materialization() {
        // Break caught: TEXT length counts characters/stops at NUL and raw length ignores JSON escaping.
        let pool = sqlite::open_database("sqlite::memory:").await.unwrap();
        sqlite::migrate(&pool).await.unwrap();
        let repository = SessionRepository::new(pool.clone());

        let cases = [
            "界".repeat(3_000_000),
            "\u{0001}".repeat(super::MAX_SESSION_EXPORT_BYTES / 6 + 1024),
            format!("\0{}", "x".repeat(super::MAX_SESSION_EXPORT_BYTES + 1024)),
        ];
        for content in cases {
            let session = repository.create(None).await.unwrap();
            MessageRepository::new(pool.clone())
                .add(session.id, "user", &content, Some(json!({})))
                .await
                .unwrap();
            let mut tx = pool.begin().await.unwrap();
            let error = preflight_export(&mut tx, session.id).await.unwrap_err();
            assert_eq!(
                error.to_string(),
                "invalid session export: session export exceeds bounds"
            );
            tx.rollback().await.unwrap();
        }
    }
}
