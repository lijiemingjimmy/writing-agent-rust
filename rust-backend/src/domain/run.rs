use serde::{Deserialize, Serialize};

use super::{RunId, SessionId};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct CostMicrousd(pub u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PriceSnapshot {
    pub input_microusd_per_million: u64,
    pub output_microusd_per_million: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Queued,
    Running,
    Completed,
    Cancelled,
    BudgetExceeded,
    Failed,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Failed => "failed",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Cancelled | Self::BudgetExceeded | Self::Failed
        )
    }

    pub fn terminal_event_kind(self) -> Option<&'static str> {
        match self {
            Self::Completed => Some("run.completed"),
            Self::Cancelled => Some("run.cancelled"),
            Self::BudgetExceeded => Some("run.budget_exceeded"),
            Self::Failed => Some("run.failed"),
            Self::Queued | Self::Running => None,
        }
    }

    pub(crate) fn from_database(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "cancelled" => Some(Self::Cancelled),
            "budget_exceeded" => Some(Self::BudgetExceeded),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AgentRun {
    pub id: RunId,
    pub session_id: SessionId,
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

impl AgentRun {
    pub fn usage(&self) -> Usage {
        Usage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunEvent {
    pub run_id: RunId,
    pub seq: u64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at: Option<String>,
}

impl RunEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "run.completed" | "run.cancelled" | "run.budget_exceeded" | "run.failed"
        )
    }
}

#[derive(Clone, Debug)]
pub struct ModelCallRecord {
    pub run_id: RunId,
    pub purpose: String,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    pub price: PriceSnapshot,
    pub cost: CostMicrousd,
    pub duration_ms: Option<u64>,
    pub finish_reason: Option<String>,
    pub response_id: Option<String>,
    pub created_at: Option<String>,
}
