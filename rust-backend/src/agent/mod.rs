mod conversation_memory;
mod input_safety;
mod run_context;
mod run_engine;
mod writing_coach;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AppError, domain::SessionId};

pub use run_context::RunContext;
pub use run_engine::{RunEngine, RunHandle, RunSubscription, SessionPreparation};
pub use writing_coach::WritingCoachProgram;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnAction {
    Synthesize,
}

impl TurnAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Synthesize => "synthesize",
        }
    }
}

#[derive(Clone, Debug)]
pub struct UserTurn {
    pub session_id: SessionId,
    pub content: String,
    pub max_steps: u32,
    pub token_budget: Option<u64>,
    pub cost_budget_microusd: Option<u64>,
    pub enable_web_search: bool,
    pub action: Option<TurnAction>,
}

impl UserTurn {
    pub fn new(session_id: SessionId, content: impl Into<String>) -> Self {
        Self {
            session_id,
            content: content.into(),
            max_steps: 32,
            token_budget: None,
            cost_budget_microusd: None,
            enable_web_search: false,
            action: None,
        }
    }

    pub fn with_limits(
        mut self,
        max_steps: u32,
        token_budget: Option<u64>,
        cost_budget_microusd: Option<u64>,
    ) -> Self {
        self.max_steps = max_steps;
        self.token_budget = token_budget;
        self.cost_budget_microusd = cost_budget_microusd;
        self
    }

    pub fn with_web_search(mut self, enabled: bool) -> Self {
        self.enable_web_search = enabled;
        self
    }

    pub fn with_action(mut self, action: impl AsRef<str>) -> Self {
        self.action = match action.as_ref() {
            "synthesize" => Some(TurnAction::Synthesize),
            _ => None,
        };
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentAnswer {
    pub content: String,
    pub metadata: Value,
}

impl AgentAnswer {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            metadata: serde_json::json!({}),
        }
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }
}

#[async_trait]
pub trait AgentProgram: Send + Sync {
    async fn execute(&self, context: RunContext, turn: UserTurn) -> Result<AgentAnswer, AppError>;
}
