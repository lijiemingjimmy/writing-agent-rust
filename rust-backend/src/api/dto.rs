use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{agent::TurnAction, domain::Message, store::sessions::SessionSummary};

#[derive(Clone, Debug, Deserialize)]
pub struct RunRequest {
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    pub student_name: Option<String>,
    pub student_id: Option<String>,
    pub message: String,
    pub action: Option<TurnAction>,
    #[serde(default)]
    pub enable_web_search: bool,
}

#[derive(Serialize)]
pub struct CreateRunResponse {
    pub run_id: String,
    pub session_id: String,
}

#[derive(Serialize)]
pub struct RunResponse {
    pub run_id: String,
    pub session_id: String,
    pub status: crate::domain::RunStatus,
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

impl From<crate::domain::AgentRun> for RunResponse {
    fn from(run: crate::domain::AgentRun) -> Self {
        Self {
            run_id: run.id.to_legacy_hex(),
            session_id: run.session_id.to_legacy_hex(),
            status: run.status,
            current_step: run.current_step,
            max_steps: run.max_steps,
            token_budget: run.token_budget,
            cost_budget_microusd: run.cost_budget_microusd,
            input_tokens: run.input_tokens,
            output_tokens: run.output_tokens,
            cost_microusd: run.cost_microusd,
            cancel_reason: run.cancel_reason,
            error_message: run.error_message,
            created_at: run.created_at,
            started_at: run.started_at,
            finished_at: run.finished_at,
        }
    }
}

#[derive(Serialize)]
pub struct ChatResponse {
    pub session_id: String,
    pub reply: String,
    pub current_skill: Option<String>,
    pub awaiting_slots: Vec<String>,
    pub metadata: Value,
}

#[derive(Serialize)]
pub struct ImportSessionResponse {
    pub session_id: String,
    pub run_ids: Vec<String>,
}

#[derive(Serialize)]
pub struct DocumentUploadResponse {
    pub document_id: String,
    pub session_id: String,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: usize,
}

#[derive(Serialize)]
pub struct SessionListResponse {
    pub sessions: Vec<SessionListItem>,
}

#[derive(Serialize)]
pub struct SessionListItem {
    pub session_id: String,
    pub user_id: Option<String>,
    pub student_name: Option<String>,
    pub student_id: Option<String>,
    pub task_type: Option<String>,
    pub stage: String,
    pub preview: Option<String>,
    pub message_count: i64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Serialize)]
pub struct MessageListResponse {
    pub messages: Vec<HistoryMessage>,
}

#[derive(Serialize)]
pub struct HistoryMessage {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub metadata_json: Value,
}

impl From<SessionSummary> for SessionListItem {
    fn from(summary: SessionSummary) -> Self {
        let session = summary.session;
        Self {
            session_id: session.id.to_legacy_hex(),
            user_id: session.user_id,
            student_name: summary.student_name,
            student_id: summary.student_id,
            task_type: session.task_type,
            stage: session.stage,
            preview: summary.preview,
            message_count: summary.message_count,
            created_at: session.created_at,
            updated_at: session.updated_at,
        }
    }
}

impl From<Message> for HistoryMessage {
    fn from(message: Message) -> Self {
        Self {
            id: message.id.to_legacy_hex(),
            session_id: message.session_id.to_legacy_hex(),
            role: message.role,
            content: message.content,
            metadata_json: message.metadata_json,
        }
    }
}
