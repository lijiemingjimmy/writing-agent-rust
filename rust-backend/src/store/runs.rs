use serde_json::{Map, Value, json};
use sqlx::{Row, Sqlite, SqlitePool, Transaction, sqlite::SqliteRow};
#[cfg(test)]
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[cfg(test)]
use tokio::sync::Notify;

use crate::{
    AppError,
    domain::{
        AgentRun, MessageId, ModelCallRecord, RunEvent, RunId, RunStatus, SessionId, SkillEventId,
    },
};

#[derive(Clone, Debug)]
pub struct WritingTurnSkillEvent {
    pub skill_id: String,
    pub event_type: String,
    pub payload: Value,
}

impl WritingTurnSkillEvent {
    pub fn new(skill_id: impl Into<String>, event_type: impl Into<String>, payload: Value) -> Self {
        Self {
            skill_id: skill_id.into(),
            event_type: event_type.into(),
            payload,
        }
    }
}

#[derive(Clone)]
pub struct RunRepository {
    pool: SqlitePool,
    #[cfg(test)]
    terminal_test_gate: Option<Arc<TerminalCommitTestGate>>,
    #[cfg(test)]
    terminal_timestamp_test_value: Option<String>,
    #[cfg(test)]
    terminal_failures_for_test: Arc<AtomicUsize>,
}

#[cfg(test)]
#[derive(Default)]
struct TerminalCommitTestGate {
    entered: Notify,
    release: Notify,
}

impl RunRepository {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            #[cfg(test)]
            terminal_test_gate: None,
            #[cfg(test)]
            terminal_timestamp_test_value: None,
            #[cfg(test)]
            terminal_failures_for_test: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[cfg(test)]
    fn with_terminal_test_gate(mut self, gate: Arc<TerminalCommitTestGate>) -> Self {
        self.terminal_test_gate = Some(gate);
        self
    }

    #[cfg(test)]
    fn with_terminal_timestamp_test_value(mut self, value: String) -> Self {
        self.terminal_timestamp_test_value = Some(value);
        self
    }

    #[cfg(test)]
    pub(crate) fn inject_terminal_failures_for_test(&self, count: usize) {
        self.terminal_failures_for_test
            .store(count, Ordering::SeqCst);
    }

    pub async fn create(
        &self,
        session_id: SessionId,
        max_steps: u32,
        token_budget: Option<u64>,
        cost_budget_microusd: Option<u64>,
    ) -> Result<AgentRun, AppError> {
        self.create_prepared(
            session_id,
            false,
            None,
            None,
            max_steps,
            token_budget,
            cost_budget_microusd,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_prepared(
        &self,
        session_id: SessionId,
        create_session: bool,
        user_id: Option<&str>,
        student_name: Option<&str>,
        max_steps: u32,
        token_budget: Option<u64>,
        cost_budget_microusd: Option<u64>,
    ) -> Result<AgentRun, AppError> {
        if max_steps == 0 {
            return Err(AppError::InvalidRun(
                "max_steps must be greater than zero".to_owned(),
            ));
        }
        let id = RunId::new();
        let id_hex = id.to_legacy_hex();
        let session_hex = session_id.to_legacy_hex();
        let mut tx = self.pool.begin().await?;

        if create_session {
            sqlx::query(
                "INSERT INTO sessions (id, user_id, stage, created_at, updated_at) \
                 VALUES (?, ?, 'NEW', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            )
            .bind(&session_hex)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO session_states (session_id, state_json, updated_at) \
                 VALUES (?, '{}', CURRENT_TIMESTAMP)",
            )
            .bind(&session_hex)
            .execute(&mut *tx)
            .await?;
        } else {
            let session = sqlx::query(
                "UPDATE sessions SET updated_at = updated_at WHERE id = ? RETURNING id",
            )
            .bind(&session_hex)
            .fetch_optional(&mut *tx)
            .await?;
            if session.is_none() {
                return Err(AppError::NotFound("session".to_owned()));
            }
        }
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agent_runs WHERE session_id = ? AND status IN ('queued', 'running')",
        )
        .bind(&session_hex)
        .fetch_one(&mut *tx)
        .await?;
        if active != 0 {
            return Err(AppError::ActiveRunConflict);
        }

        if user_id.is_some() || student_name.is_some() {
            let state_text: String =
                sqlx::query_scalar("SELECT state_json FROM session_states WHERE session_id = ?")
                    .bind(&session_hex)
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or_else(|| AppError::NotFound("session state".to_owned()))?;
            let mut state: Value = serde_json::from_str(&state_text).map_err(|_| {
                AppError::CorruptData("session state must be valid JSON".to_owned())
            })?;
            let object = state.as_object_mut().ok_or_else(|| {
                AppError::CorruptData("session state must be a JSON object".to_owned())
            })?;
            let profile = object
                .entry("student_profile")
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .ok_or_else(|| {
                    AppError::CorruptData("student profile must be a JSON object".to_owned())
                })?;
            if let Some(user_id) = user_id {
                profile.insert("student_id".to_owned(), Value::String(user_id.to_owned()));
            }
            if let Some(student_name) = student_name {
                profile.insert("name".to_owned(), Value::String(student_name.to_owned()));
            }
            sqlx::query(
                "UPDATE sessions SET user_id = COALESCE(?, user_id), updated_at = CURRENT_TIMESTAMP WHERE id = ?",
            )
            .bind(user_id)
            .bind(&session_hex)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE session_states SET state_json = ?, updated_at = CURRENT_TIMESTAMP WHERE session_id = ?",
            )
            .bind(serialize_payload(&state)?)
            .bind(&session_hex)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "INSERT INTO agent_runs (id, session_id, status, max_steps, token_budget, \
             cost_budget_microusd, created_at) VALUES (?, ?, 'queued', ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(&id_hex)
        .bind(&session_hex)
        .bind(i64::from(max_steps))
        .bind(optional_u64_to_i64(token_budget, "token budget")?)
        .bind(optional_u64_to_i64(cost_budget_microusd, "cost budget")?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.get(id).await
    }

    pub async fn get(&self, run_id: RunId) -> Result<AgentRun, AppError> {
        let row = sqlx::query(
            "SELECT id, session_id, status, current_step, max_steps, token_budget, \
             cost_budget_microusd, input_tokens, output_tokens, cost_microusd, \
             cancel_reason, error_message, created_at, started_at, finished_at \
             FROM agent_runs WHERE id = ?",
        )
        .bind(run_id.to_legacy_hex())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("run".to_owned()))?;
        run_from_row(&row)
    }

    pub async fn list_events(
        &self,
        run_id: RunId,
        after_seq: u64,
    ) -> Result<Vec<RunEvent>, AppError> {
        let rows = sqlx::query(
            "SELECT run_id, seq, kind, payload_json, created_at FROM run_events \
             WHERE run_id = ? AND seq > ? ORDER BY seq ASC",
        )
        .bind(run_id.to_legacy_hex())
        .bind(u64_to_i64(after_seq, "event sequence")?)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(event_from_row).collect()
    }

    pub async fn mark_running(&self, run_id: RunId) -> Result<Option<RunEvent>, AppError> {
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query(
            "UPDATE agent_runs SET status = 'running', started_at = CURRENT_TIMESTAMP \
             WHERE id = ? AND status = 'queued'",
        )
        .bind(run_id.to_legacy_hex())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 0 {
            tx.rollback().await?;
            self.get(run_id).await?;
            return Ok(None);
        }
        let event = insert_event(&mut tx, run_id, "run.started", &json!({})).await?;
        tx.commit().await?;
        Ok(Some(event))
    }

    pub async fn reconcile_orphans(&self, reason: &str) -> Result<Vec<RunEvent>, AppError> {
        let payload_text = serialize_payload(&json!({"message": reason}))?;
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT id FROM agent_runs WHERE status IN ('queued', 'running') ORDER BY created_at, id",
        )
        .fetch_all(&mut *tx)
        .await?;
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let run_id = RunId::parse_legacy(&row.try_get::<String, _>("id")?)?;
            let claimed = sqlx::query(
                "UPDATE agent_runs SET status = 'failed', error_message = ?, \
                 finished_at = strftime('%Y-%m-%d %H:%M:%f', 'now') \
                 WHERE id = ? AND status IN ('queued', 'running') RETURNING finished_at",
            )
            .bind(reason)
            .bind(run_id.to_legacy_hex())
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(claimed) = claimed {
                let terminal_time: String = claimed.try_get("finished_at")?;
                events.push(
                    insert_serialized_event_at(
                        &mut tx,
                        run_id,
                        "run.failed",
                        &payload_text,
                        Some(&terminal_time),
                    )
                    .await?,
                );
            }
        }
        tx.commit().await?;
        Ok(events)
    }

    pub async fn append_event(
        &self,
        run_id: RunId,
        kind: &str,
        payload: Value,
        current_step: Option<&str>,
    ) -> Result<RunEvent, AppError> {
        let payload_text = serialize_payload(&payload)?;
        let mut tx = self.pool.begin().await?;
        let active = sqlx::query(
            "UPDATE agent_runs SET current_step = COALESCE(?, current_step) \
             WHERE id = ? AND status = 'running'",
        )
        .bind(current_step)
        .bind(run_id.to_legacy_hex())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if active == 0 {
            tx.rollback().await?;
            return Err(self.inactive_error(run_id).await);
        }
        let event = insert_serialized_event(&mut tx, run_id, kind, &payload_text).await?;
        tx.commit().await?;
        Ok(event)
    }

    pub async fn record_model_call(&self, record: &ModelCallRecord) -> Result<RunEvent, AppError> {
        let call_id = RunId::new().to_legacy_hex();
        let run_id = record.run_id;
        let input = u64_to_i64(record.usage.input_tokens, "input token count")?;
        let output = u64_to_i64(record.usage.output_tokens, "output token count")?;
        let input_price = u64_to_i64(
            record.price.input_microusd_per_million,
            "input price snapshot",
        )?;
        let output_price = u64_to_i64(
            record.price.output_microusd_per_million,
            "output price snapshot",
        )?;
        let cost = u64_to_i64(record.cost.0, "model call cost")?;
        let duration = optional_u64_to_i64(record.duration_ms, "model call duration")?;
        let mut tx = self.pool.begin().await?;

        let active = sqlx::query(
            "UPDATE agent_runs SET status = status WHERE id = ? AND status = 'running'",
        )
        .bind(run_id.to_legacy_hex())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if active == 0 {
            tx.rollback().await?;
            return Err(self.inactive_error(run_id).await);
        }

        sqlx::query(
            "INSERT INTO model_calls (id, run_id, purpose, provider, model, input_tokens, \
             output_tokens, input_price_microusd_per_million, \
             output_price_microusd_per_million, cost_microusd, duration_ms, finish_reason, \
             response_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(call_id)
        .bind(run_id.to_legacy_hex())
        .bind(&record.purpose)
        .bind(&record.provider)
        .bind(&record.model)
        .bind(input)
        .bind(output)
        .bind(input_price)
        .bind(output_price)
        .bind(cost)
        .bind(duration)
        .bind(&record.finish_reason)
        .bind(&record.response_id)
        .execute(&mut *tx)
        .await?;

        let totals = sqlx::query(
            "UPDATE agent_runs SET input_tokens = input_tokens + ?, \
             output_tokens = output_tokens + ?, cost_microusd = cost_microusd + ? \
             WHERE id = ? AND status = 'running' \
             RETURNING input_tokens, output_tokens, cost_microusd",
        )
        .bind(input)
        .bind(output)
        .bind(cost)
        .bind(run_id.to_legacy_hex())
        .fetch_one(&mut *tx)
        .await?;
        let cumulative_input: i64 = totals.try_get("input_tokens")?;
        let cumulative_output: i64 = totals.try_get("output_tokens")?;
        let cumulative_cost: i64 = totals.try_get("cost_microusd")?;
        let payload = json!({
            "purpose": record.purpose,
            "provider": record.provider,
            "model": record.model,
            "input_tokens": record.usage.input_tokens,
            "output_tokens": record.usage.output_tokens,
            "cost_microusd": record.cost.0,
            "cumulative_input_tokens": cumulative_input,
            "cumulative_output_tokens": cumulative_output,
            "cumulative_cost_microusd": cumulative_cost,
        });
        let event = insert_event(&mut tx, run_id, "model.usage", &payload).await?;
        tx.commit().await?;
        Ok(event)
    }

    pub async fn finish(
        &self,
        run_id: RunId,
        status: RunStatus,
        reason: Option<&str>,
        payload: Value,
    ) -> Result<Option<RunEvent>, AppError> {
        #[cfg(test)]
        if self
            .terminal_failures_for_test
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(sqlx::Error::Protocol(
                "injected transient terminal persistence failure".to_owned(),
            )
            .into());
        }
        let kind = status
            .terminal_event_kind()
            .ok_or_else(|| AppError::InvalidRun("finish requires a terminal status".to_owned()))?;
        let payload_text = serialize_payload(&payload)?;
        let cancel_reason = matches!(status, RunStatus::Cancelled | RunStatus::BudgetExceeded)
            .then_some(reason)
            .flatten();
        let error_message = matches!(status, RunStatus::Failed)
            .then_some(reason)
            .flatten();
        let mut tx = self.pool.begin().await?;
        let terminal = sqlx::query(
            "UPDATE agent_runs SET status = ?, cancel_reason = ?, error_message = ?, \
             finished_at = COALESCE(?, strftime('%Y-%m-%d %H:%M:%f', 'now')) \
             WHERE id = ? AND status IN ('queued', 'running') RETURNING finished_at",
        )
        .bind(status.as_str())
        .bind(cancel_reason)
        .bind(error_message)
        .bind(self.terminal_timestamp_override())
        .bind(run_id.to_legacy_hex())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(terminal) = terminal else {
            tx.rollback().await?;
            self.get(run_id).await?;
            return Ok(None);
        };
        let terminal_time: String = terminal.try_get("finished_at")?;
        let event =
            insert_serialized_event_at(&mut tx, run_id, kind, &payload_text, Some(&terminal_time))
                .await?;
        tx.commit().await?;
        Ok(Some(event))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn persist_terminal_writing_turn(
        &self,
        run_id: RunId,
        session_id: SessionId,
        state: Value,
        assistant_content: &str,
        mut assistant_metadata: Value,
        skill_events: &[WritingTurnSkillEvent],
        completed_phase: &str,
    ) -> Result<Vec<RunEvent>, AppError> {
        let state_text = serialize_payload(&state)?;
        assistant_metadata
            .as_object_mut()
            .ok_or_else(|| {
                AppError::CorruptData("assistant metadata must be a JSON object".to_owned())
            })?
            .insert("run_id".to_owned(), Value::String(run_id.to_legacy_hex()));
        let metadata_text = serialize_payload(&assistant_metadata)?;
        let run_hex = run_id.to_legacy_hex();
        let session_hex = session_id.to_legacy_hex();
        #[cfg(test)]
        if let Some(gate) = &self.terminal_test_gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        let mut tx = self.pool.begin().await?;
        let claimed = sqlx::query(
            "UPDATE agent_runs SET status = 'completed', \
             finished_at = COALESCE(?, strftime('%Y-%m-%d %H:%M:%f', 'now')) \
             WHERE id = ? AND session_id = ? AND status = 'running' AND current_step = ? \
             RETURNING finished_at",
        )
        .bind(self.terminal_timestamp_override())
        .bind(&run_hex)
        .bind(&session_hex)
        .bind(completed_phase)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(claimed) = claimed else {
            tx.rollback().await?;
            let run = self.get(run_id).await?;
            if run.status == RunStatus::Running
                && run.current_step.as_deref() != Some(completed_phase)
            {
                return Err(AppError::InvalidRun(
                    "completion phase does not own the current run step".to_owned(),
                ));
            }
            return Err(self.inactive_error(run_id).await);
        };
        let terminal_time: String = claimed.try_get("finished_at")?;

        let existing_stage: String = sqlx::query_scalar("SELECT stage FROM sessions WHERE id = ?")
            .bind(&session_hex)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| AppError::NotFound("session".to_owned()))?;
        let stage = state
            .pointer("/writing_context/stage")
            .and_then(Value::as_str)
            .or_else(|| state.get("stage").and_then(Value::as_str))
            .unwrap_or(&existing_stage);
        let task_type = state.get("task_type").and_then(Value::as_str);
        sqlx::query(
            "UPDATE sessions SET stage = ?, task_type = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
        )
        .bind(stage)
        .bind(task_type)
        .bind(&session_hex)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO session_states (session_id, state_json, updated_at) VALUES (?, ?, CURRENT_TIMESTAMP) \
             ON CONFLICT(session_id) DO UPDATE SET state_json = excluded.state_json, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(&session_hex)
        .bind(state_text)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, content, metadata_json, created_at) \
             VALUES (?, ?, 'assistant', ?, ?, CURRENT_TIMESTAMP)",
        )
        .bind(MessageId::new().to_legacy_hex())
        .bind(&session_hex)
        .bind(assistant_content)
        .bind(metadata_text)
        .execute(&mut *tx)
        .await?;
        for event in skill_events {
            sqlx::query(
                "INSERT INTO skill_events (id, session_id, skill_id, event_type, metadata_json, created_at) \
                 VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)",
            )
            .bind(SkillEventId::new().to_legacy_hex())
            .bind(&session_hex)
            .bind(&event.skill_id)
            .bind(&event.event_type)
            .bind(serialize_payload(&event.payload)?)
            .execute(&mut *tx)
            .await?;
        }
        let phase_event = insert_event_at(
            &mut tx,
            run_id,
            "step.completed",
            &json!({"step": completed_phase}),
            Some(&terminal_time),
        )
        .await?;
        let terminal_event = insert_event_at(
            &mut tx,
            run_id,
            "run.completed",
            &json!({"answer": assistant_content, "metadata": assistant_metadata}),
            Some(&terminal_time),
        )
        .await?;
        tx.commit().await?;
        Ok(vec![phase_event, terminal_event])
    }

    #[cfg(test)]
    fn terminal_timestamp_override(&self) -> Option<&str> {
        self.terminal_timestamp_test_value.as_deref()
    }

    #[cfg(not(test))]
    fn terminal_timestamp_override(&self) -> Option<&str> {
        None
    }

    async fn inactive_error(&self, run_id: RunId) -> AppError {
        match self.get(run_id).await {
            Ok(run) if run.status.is_terminal() => AppError::RunTerminal,
            Ok(_) => AppError::InvalidRun("run is not executing".to_owned()),
            Err(error) => error,
        }
    }
}

async fn insert_event(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: RunId,
    kind: &str,
    payload: &Value,
) -> Result<RunEvent, AppError> {
    insert_event_at(tx, run_id, kind, payload, None).await
}

async fn insert_event_at(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: RunId,
    kind: &str,
    payload: &Value,
    created_at: Option<&str>,
) -> Result<RunEvent, AppError> {
    let payload_text = serialize_payload(payload)?;
    insert_serialized_event_at(tx, run_id, kind, &payload_text, created_at).await
}

async fn insert_serialized_event(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: RunId,
    kind: &str,
    payload_text: &str,
) -> Result<RunEvent, AppError> {
    insert_serialized_event_at(tx, run_id, kind, payload_text, None).await
}

async fn insert_serialized_event_at(
    tx: &mut Transaction<'_, Sqlite>,
    run_id: RunId,
    kind: &str,
    payload_text: &str,
    created_at: Option<&str>,
) -> Result<RunEvent, AppError> {
    let event_id = RunId::new().to_legacy_hex();
    let run_hex = run_id.to_legacy_hex();
    let next_seq: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) + 1 FROM run_events WHERE run_id = ?")
            .bind(&run_hex)
            .fetch_one(&mut **tx)
            .await?;
    let row = sqlx::query(
        "INSERT INTO run_events (id, run_id, seq, kind, payload_json, created_at) \
         VALUES (?, ?, ?, ?, ?, COALESCE(?, CURRENT_TIMESTAMP)) RETURNING created_at",
    )
    .bind(event_id)
    .bind(&run_hex)
    .bind(next_seq)
    .bind(kind)
    .bind(payload_text)
    .bind(created_at)
    .fetch_one(&mut **tx)
    .await?;
    Ok(RunEvent {
        run_id,
        seq: i64_to_u64(next_seq, "run_events.seq")?,
        kind: kind.to_owned(),
        payload: serde_json::from_str(payload_text).map_err(|error| {
            AppError::CorruptData(format!("invalid serialized run event payload: {error}"))
        })?,
        created_at: row.try_get("created_at")?,
    })
}

fn run_from_row(row: &SqliteRow) -> Result<AgentRun, AppError> {
    let status_text: String = row.try_get("status")?;
    let status = RunStatus::from_database(&status_text).ok_or_else(|| {
        AppError::CorruptData(format!("unknown agent_runs.status {status_text:?}"))
    })?;
    let max_steps = u32::try_from(row.try_get::<i64, _>("max_steps")?)
        .map_err(|_| AppError::CorruptData("invalid agent_runs.max_steps".to_owned()))?;
    Ok(AgentRun {
        id: RunId::parse_legacy(&row.try_get::<String, _>("id")?)?,
        session_id: SessionId::parse_legacy(&row.try_get::<String, _>("session_id")?)?,
        status,
        current_step: row.try_get("current_step")?,
        max_steps,
        token_budget: optional_i64_to_u64(row.try_get("token_budget")?, "agent_runs.token_budget")?,
        cost_budget_microusd: optional_i64_to_u64(
            row.try_get("cost_budget_microusd")?,
            "agent_runs.cost_budget_microusd",
        )?,
        input_tokens: i64_to_u64(row.try_get("input_tokens")?, "agent_runs.input_tokens")?,
        output_tokens: i64_to_u64(row.try_get("output_tokens")?, "agent_runs.output_tokens")?,
        cost_microusd: i64_to_u64(row.try_get("cost_microusd")?, "agent_runs.cost_microusd")?,
        cancel_reason: row.try_get("cancel_reason")?,
        error_message: row.try_get("error_message")?,
        created_at: row.try_get("created_at")?,
        started_at: row.try_get("started_at")?,
        finished_at: row.try_get("finished_at")?,
    })
}

fn event_from_row(row: &SqliteRow) -> Result<RunEvent, AppError> {
    let payload_text: String = row.try_get("payload_json")?;
    Ok(RunEvent {
        run_id: RunId::parse_legacy(&row.try_get::<String, _>("run_id")?)?,
        seq: i64_to_u64(row.try_get("seq")?, "run_events.seq")?,
        kind: row.try_get("kind")?,
        payload: serde_json::from_str(&payload_text).map_err(|error| {
            AppError::CorruptData(format!("invalid JSON in run_events.payload_json: {error}"))
        })?,
        created_at: row.try_get("created_at")?,
    })
}

fn serialize_payload(payload: &Value) -> Result<String, AppError> {
    serde_json::to_string(payload).map_err(|error| {
        AppError::CorruptData(format!("could not serialize run event payload: {error}"))
    })
}

fn u64_to_i64(value: u64, field: &str) -> Result<i64, AppError> {
    i64::try_from(value)
        .map_err(|_| AppError::InvalidRun(format!("{field} exceeds SQLite INTEGER range")))
}

fn optional_u64_to_i64(value: Option<u64>, field: &str) -> Result<Option<i64>, AppError> {
    value.map(|value| u64_to_i64(value, field)).transpose()
}

fn i64_to_u64(value: i64, field: &str) -> Result<u64, AppError> {
    u64::try_from(value).map_err(|_| AppError::CorruptData(format!("negative {field}")))
}

fn optional_i64_to_u64(value: Option<i64>, field: &str) -> Result<Option<u64>, AppError> {
    value.map(|value| i64_to_u64(value, field)).transpose()
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use serde_json::json;
    use sqlx::{SqlitePool, sqlite::SqlitePoolOptions};

    use super::{RunRepository, TerminalCommitTestGate};
    use crate::{
        AppError,
        domain::{RunStatus, SessionId},
        store::{sessions::SessionRepository, sqlite},
    };

    const WAIT: Duration = Duration::from_secs(5);

    async fn fixture() -> (SqlitePool, SessionId, RunRepository, crate::domain::RunId) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlite::migrate(&pool).await.unwrap();
        let session_id = SessionRepository::new(pool.clone())
            .create(None)
            .await
            .unwrap()
            .id;
        let repository = RunRepository::new(pool.clone());
        let run = repository.create(session_id, 12, None, None).await.unwrap();
        repository.mark_running(run.id).await.unwrap();
        repository
            .append_event(
                run.id,
                "step.started",
                json!({"step": "persist_answer"}),
                Some("persist_answer"),
            )
            .await
            .unwrap();
        (pool, session_id, repository, run.id)
    }

    async fn output_count(pool: &SqlitePool, session_id: SessionId) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE session_id = ? AND role = 'assistant'",
        )
        .bind(session_id.to_legacy_hex())
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn cancellation_wins_private_prewrite_gate_without_partial_success() {
        let (pool, session_id, repository, run_id) = fixture().await;
        let gate = Arc::new(TerminalCommitTestGate::default());
        let gated = repository.clone().with_terminal_test_gate(gate.clone());
        let completion = tokio::spawn(async move {
            gated
                .persist_terminal_writing_turn(
                    run_id,
                    session_id,
                    json!({"stage": "topic"}),
                    "answer",
                    json!({}),
                    &[],
                    "persist_answer",
                )
                .await
        });
        tokio::time::timeout(WAIT, gate.entered.notified())
            .await
            .expect("completion reached private prewrite gate");
        repository
            .finish(
                run_id,
                RunStatus::Cancelled,
                Some("test_cancel"),
                json!({"reason": "test_cancel"}),
            )
            .await
            .unwrap();
        gate.release.notify_one();

        let completion = tokio::time::timeout(WAIT, completion)
            .await
            .expect("cancelled completion task finished")
            .unwrap();
        assert!(matches!(completion, Err(AppError::RunTerminal)));
        assert_eq!(output_count(&pool, session_id).await, 0);
        assert_eq!(
            repository.get(run_id).await.unwrap().status,
            RunStatus::Cancelled
        );
        assert_eq!(
            repository
                .list_events(run_id, 0)
                .await
                .unwrap()
                .iter()
                .filter(|event| event.kind.starts_with("run.") && event.kind != "run.started")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn completion_wins_private_prewrite_gate_and_late_cancel_is_noop() {
        let (pool, session_id, repository, run_id) = fixture().await;
        let gate = Arc::new(TerminalCommitTestGate::default());
        let gated = repository.clone().with_terminal_test_gate(gate.clone());
        let completion = tokio::spawn(async move {
            gated
                .persist_terminal_writing_turn(
                    run_id,
                    session_id,
                    json!({"stage": "topic"}),
                    "answer",
                    json!({}),
                    &[],
                    "persist_answer",
                )
                .await
        });
        tokio::time::timeout(WAIT, gate.entered.notified())
            .await
            .expect("completion reached private prewrite gate");
        gate.release.notify_one();
        tokio::time::timeout(WAIT, completion)
            .await
            .expect("successful completion task finished")
            .unwrap()
            .unwrap();

        assert!(
            repository
                .finish(
                    run_id,
                    RunStatus::Cancelled,
                    Some("too_late"),
                    json!({"reason": "too_late"}),
                )
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(output_count(&pool, session_id).await, 1);
        assert_eq!(
            repository.get(run_id).await.unwrap().status,
            RunStatus::Completed
        );
        assert_eq!(
            repository
                .list_events(run_id, 0)
                .await
                .unwrap()
                .iter()
                .filter(|event| event.kind.starts_with("run.") && event.kind != "run.started")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn terminal_transactions_bind_one_timestamp_to_run_and_terminal_event() {
        // Break caught: separate CURRENT_TIMESTAMP evaluations can straddle a second boundary.
        let terminal_time = "2030-05-06 07:08:09.123";
        for status in [
            RunStatus::Cancelled,
            RunStatus::Failed,
            RunStatus::BudgetExceeded,
        ] {
            let (_, _, repository, run_id) = fixture().await;
            let repository =
                repository.with_terminal_timestamp_test_value(terminal_time.to_owned());
            let event = repository
                .finish(
                    run_id,
                    status,
                    Some("terminal"),
                    json!({"reason": "terminal"}),
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(event.created_at.as_deref(), Some(terminal_time));
            assert_eq!(
                repository.get(run_id).await.unwrap().finished_at.as_deref(),
                Some(terminal_time)
            );
        }

        let (_, session_id, repository, run_id) = fixture().await;
        let repository = repository.with_terminal_timestamp_test_value(terminal_time.to_owned());
        let events = repository
            .persist_terminal_writing_turn(
                run_id,
                session_id,
                json!({"stage": "topic"}),
                "answer",
                json!({}),
                &[],
                "persist_answer",
            )
            .await
            .unwrap();
        assert_eq!(events[1].created_at.as_deref(), Some(terminal_time));
        assert!(events[0].created_at <= events[1].created_at);
        assert_eq!(
            repository.get(run_id).await.unwrap().finished_at.as_deref(),
            Some(terminal_time)
        );
    }
}
