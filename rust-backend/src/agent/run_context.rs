use std::sync::Arc;

use serde_json::{Value, json};
#[cfg(test)]
use tokio::sync::Notify;
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;

use crate::{
    AppError,
    domain::{ModelCallRecord, RunEvent, RunId, RunStatus},
    llm::{
        ModelCallSettings, ModelError, ModelGateway, ModelRequest, ModelResponse, calculate_cost,
    },
    store::runs::{RunRepository, WritingTurnSkillEvent},
    tools::{KnowledgeBundle, KnowledgeCoordinator, KnowledgePlan},
};

#[derive(Clone)]
pub struct RunContext {
    repository: RunRepository,
    gateway: Arc<dyn ModelGateway>,
    cancellation: CancellationToken,
    events: broadcast::Sender<RunEvent>,
    run_id: RunId,
    settings: ModelCallSettings,
    max_steps: u32,
    admission: Arc<Mutex<AdmissionState>>,
    lifecycle: Arc<Mutex<()>>,
    #[cfg(test)]
    response_accounting_test_gate: Option<Arc<ResponseAccountingTestGate>>,
}

pub(crate) struct RunContextControl {
    cancellation: CancellationToken,
    events: broadcast::Sender<RunEvent>,
    lifecycle: Arc<Mutex<()>>,
}

impl RunContextControl {
    pub(crate) fn new(
        cancellation: CancellationToken,
        events: broadcast::Sender<RunEvent>,
        lifecycle: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            cancellation,
            events,
            lifecycle,
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct ResponseAccountingTestGate {
    pub(crate) response_returned: Notify,
    pub(crate) release_accounting: Notify,
}

#[derive(Default)]
struct AdmissionState {
    steps_started: u32,
}

impl RunContext {
    pub(crate) fn new(
        repository: RunRepository,
        gateway: Arc<dyn ModelGateway>,
        control: RunContextControl,
        run_id: RunId,
        settings: ModelCallSettings,
        max_steps: u32,
    ) -> Self {
        Self {
            repository,
            gateway,
            cancellation: control.cancellation,
            events: control.events,
            run_id,
            settings,
            max_steps,
            admission: Arc::new(Mutex::new(AdmissionState::default())),
            lifecycle: control.lifecycle,
            #[cfg(test)]
            response_accounting_test_gate: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_response_accounting_test_gate(
        mut self,
        gate: Arc<ResponseAccountingTestGate>,
    ) -> Self {
        self.response_accounting_test_gate = Some(gate);
        self
    }

    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub async fn check_limits(&self) -> Result<(), AppError> {
        let admission = self.admission.lock().await;
        self.check_limits_admitted(&admission).await
    }

    async fn check_limits_admitted(&self, admission: &AdmissionState) -> Result<(), AppError> {
        self.ensure_active().await?;
        if admission.steps_started > self.max_steps {
            return Err(AppError::RunMaxSteps);
        }
        let run = self.repository.get(self.run_id).await?;
        let total_tokens = run
            .input_tokens
            .checked_add(run.output_tokens)
            .ok_or_else(|| AppError::InvalidRun("cumulative token count overflowed".to_owned()))?;
        if run
            .token_budget
            .is_some_and(|budget| total_tokens >= budget)
        {
            return Err(AppError::RunBudgetExceeded(
                "token budget exhausted".to_owned(),
            ));
        }
        if run
            .cost_budget_microusd
            .is_some_and(|budget| run.cost_microusd >= budget)
        {
            return Err(AppError::RunBudgetExceeded(
                "cost budget exhausted".to_owned(),
            ));
        }
        Ok(())
    }

    pub async fn emit(&self, kind: &str, payload: Value) -> Result<RunEvent, AppError> {
        let mut admission = self.admission.lock().await;
        self.check_limits_admitted(&admission).await?;
        let current_step = if kind == "step.started" {
            if admission.steps_started >= self.max_steps {
                return Err(AppError::RunMaxSteps);
            }
            payload
                .get("step")
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            None
        };
        self.ensure_active().await?;
        let event = self
            .repository
            .append_event(self.run_id, kind, payload, current_step.as_deref())
            .await?;
        if kind == "step.started" {
            admission.steps_started += 1;
        }
        self.publish(event.clone());
        Ok(event)
    }

    pub async fn call_model(
        &self,
        purpose: &str,
        request: ModelRequest,
    ) -> Result<ModelResponse, AppError> {
        let _lifecycle = self.lifecycle.lock().await;
        let admission = self.admission.lock().await;
        self.check_limits_admitted(&admission).await?;
        self.check_model_preflight(&request).await?;
        let started_event = self
            .repository
            .append_event(
                self.run_id,
                "model.started",
                json!({"purpose": purpose}),
                None,
            )
            .await?;
        self.publish(started_event);
        self.check_limits_admitted(&admission).await?;
        let response = self
            .gateway
            .complete(request, self.settings.clone(), self.cancellation.clone())
            .await
            .map_err(map_model_error)?;
        #[cfg(test)]
        if let Some(gate) = &self.response_accounting_test_gate {
            gate.response_returned.notify_one();
            gate.release_accounting.notified().await;
        }
        let cost = calculate_cost(response.usage, self.settings.price)?;
        let record = ModelCallRecord {
            run_id: self.run_id,
            purpose: purpose.to_owned(),
            provider: response.provider.clone(),
            model: response.model.clone(),
            usage: response.usage,
            price: self.settings.price,
            cost,
            duration_ms: Some(response.latency_ms),
            finish_reason: response.stop_reason.clone(),
            response_id: response.response_id.clone(),
            created_at: None,
        };
        let usage_event = self.repository.record_model_call(&record).await?;
        self.publish(usage_event);
        self.check_limits_admitted(&admission).await?;
        Ok(response)
    }

    async fn check_model_preflight(&self, request: &ModelRequest) -> Result<(), AppError> {
        let estimated_input_tokens = estimate_prompt_tokens(request)?;
        let reserved_output_tokens = u64::from(self.settings.max_output_tokens);
        let estimated_tokens = estimated_input_tokens
            .checked_add(reserved_output_tokens)
            .ok_or_else(|| {
                AppError::InvalidRun("model request token estimate overflowed".to_owned())
            })?;
        if estimated_tokens > u64::from(self.settings.context_length) {
            return Err(AppError::ContextCapacityExceeded {
                estimated_tokens,
                context_limit: u64::from(self.settings.context_length),
            });
        }

        let run = self.repository.get(self.run_id).await?;
        let spent_tokens = run
            .input_tokens
            .checked_add(run.output_tokens)
            .ok_or_else(|| AppError::InvalidRun("cumulative token count overflowed".to_owned()))?;
        let projected_tokens = spent_tokens
            .checked_add(estimated_tokens)
            .ok_or_else(|| AppError::InvalidRun("projected token count overflowed".to_owned()))?;
        if run
            .token_budget
            .is_some_and(|budget| projected_tokens > budget)
        {
            return Err(AppError::RunBudgetExceeded(
                "next model call exceeds remaining token budget".to_owned(),
            ));
        }

        let estimated_cost = calculate_cost(
            crate::domain::Usage {
                input_tokens: estimated_input_tokens,
                output_tokens: reserved_output_tokens,
            },
            self.settings.price,
        )?;
        let projected_cost = run
            .cost_microusd
            .checked_add(estimated_cost.0)
            .ok_or_else(|| AppError::InvalidRun("projected model cost overflowed".to_owned()))?;
        if run
            .cost_budget_microusd
            .is_some_and(|budget| projected_cost > budget)
        {
            return Err(AppError::RunBudgetExceeded(
                "next model call exceeds remaining cost budget".to_owned(),
            ));
        }
        Ok(())
    }

    pub async fn record_tool_result(
        &self,
        tool: &str,
        result: Value,
    ) -> Result<RunEvent, AppError> {
        let admission = self.admission.lock().await;
        self.check_limits_admitted(&admission).await?;
        let event = self
            .repository
            .append_event(
                self.run_id,
                "search.completed",
                json!({"tool": tool, "result": result}),
                None,
            )
            .await?;
        self.publish(event.clone());
        Ok(event)
    }

    pub async fn execute_knowledge(
        &self,
        coordinator: &KnowledgeCoordinator,
        plan: KnowledgePlan,
    ) -> Result<KnowledgeBundle, AppError> {
        let admission = self.admission.lock().await;
        self.check_limits_admitted(&admission).await?;
        let tools = plan
            .searches
            .iter()
            .map(|search| search.tool.clone())
            .collect::<Vec<_>>();
        let started = self
            .repository
            .append_event(self.run_id, "tool.started", json!({"tools": tools}), None)
            .await?;
        self.publish(started);
        self.check_limits_admitted(&admission).await?;
        let bundle = coordinator.execute(plan, self.cancellation.clone()).await;
        self.ensure_active().await?;
        if bundle.metadata.cancelled {
            return Err(AppError::RunCancelled);
        }
        let completed = self
            .repository
            .append_event(
                self.run_id,
                "search.completed",
                json!({
                    "tools": tools,
                    "hit_count": bundle.hits.len(),
                    "sources": bundle.hits.iter().map(|hit| &hit.source).collect::<Vec<_>>(),
                    "provider_failures": bundle.metadata.provider_failures,
                }),
                None,
            )
            .await?;
        self.publish(completed);
        Ok(bundle)
    }

    pub async fn persist_terminal_writing_turn(
        &self,
        session_id: crate::domain::SessionId,
        state: Value,
        assistant_content: &str,
        assistant_metadata: Value,
        skill_events: &[WritingTurnSkillEvent],
        completed_phase: &str,
    ) -> Result<(), AppError> {
        let admission = self.admission.lock().await;
        self.check_limits_admitted(&admission).await?;
        self.ensure_active().await?;
        let events = self
            .repository
            .persist_terminal_writing_turn(
                self.run_id,
                session_id,
                state,
                assistant_content,
                assistant_metadata,
                skill_events,
                completed_phase,
            )
            .await?;
        for event in events {
            self.publish(event);
        }
        Ok(())
    }

    async fn ensure_active(&self) -> Result<(), AppError> {
        if self.cancellation.is_cancelled() {
            return Err(AppError::RunCancelled);
        }
        let run = self.repository.get(self.run_id).await?;
        match run.status {
            RunStatus::Queued | RunStatus::Running => Ok(()),
            RunStatus::Cancelled => Err(AppError::RunCancelled),
            RunStatus::BudgetExceeded => Err(AppError::RunBudgetExceeded(
                run.cancel_reason
                    .unwrap_or_else(|| "budget exhausted".to_owned()),
            )),
            RunStatus::Completed | RunStatus::Failed => Err(AppError::RunTerminal),
        }
    }

    fn publish(&self, event: RunEvent) {
        let _ = self.events.send(event);
    }
}

fn map_model_error(error: ModelError) -> AppError {
    match error {
        ModelError::Cancelled => AppError::RunCancelled,
        other => AppError::Model(other),
    }
}

fn estimate_prompt_tokens(request: &ModelRequest) -> Result<u64, AppError> {
    let message_count = u64::try_from(request.messages.len())
        .map_err(|_| AppError::InvalidRun("model request has too many messages".to_owned()))?;
    request.messages.iter().try_fold(
        message_count
            .checked_mul(4)
            .and_then(|value| value.checked_add(2))
            .ok_or_else(|| {
                AppError::InvalidRun("model request token estimate overflowed".to_owned())
            })?,
        |total, message| {
            let mut ascii = 0u64;
            let mut non_ascii = 0u64;
            for character in message.content.chars() {
                if character.is_ascii() {
                    ascii = ascii.checked_add(1).ok_or_else(|| {
                        AppError::InvalidRun("model request content is too large".to_owned())
                    })?;
                } else {
                    non_ascii = non_ascii.checked_add(1).ok_or_else(|| {
                        AppError::InvalidRun("model request content is too large".to_owned())
                    })?;
                }
            }
            let text_tokens = ascii
                .checked_add(3)
                .map(|value| value / 4)
                .and_then(|value| value.checked_add(non_ascii))
                .ok_or_else(|| {
                    AppError::InvalidRun("model request token estimate overflowed".to_owned())
                })?;
            total.checked_add(text_tokens).ok_or_else(|| {
                AppError::InvalidRun("model request token estimate overflowed".to_owned())
            })
        },
    )
}
