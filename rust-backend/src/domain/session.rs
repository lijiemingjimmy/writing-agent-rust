use super::{DocumentChunkId, DocumentId, MessageId, SessionId, SkillEventId};

#[derive(Clone, Debug)]
pub struct Session {
    pub id: SessionId,
    pub user_id: Option<String>,
    pub task_type: Option<String>,
    pub stage: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub id: MessageId,
    pub session_id: SessionId,
    pub role: String,
    pub content: String,
    pub metadata_json: serde_json::Value,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SessionState {
    pub session_id: SessionId,
    pub state_json: serde_json::Value,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub id: DocumentId,
    pub session_id: SessionId,
    pub filename: String,
    pub content_type: String,
    pub raw_path: Option<String>,
    pub parsed_text: Option<String>,
    pub metadata_json: serde_json::Value,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DocumentChunk {
    pub id: DocumentChunkId,
    pub document_id: DocumentId,
    pub session_id: SessionId,
    pub chunk_index: usize,
    pub heading: String,
    pub start_char: usize,
    pub end_char: usize,
    pub text: String,
    pub search_text: String,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SkillEvent {
    pub id: SkillEventId,
    pub session_id: SessionId,
    pub skill_id: String,
    pub event_type: String,
    pub metadata_json: serde_json::Value,
    pub created_at: Option<String>,
}
