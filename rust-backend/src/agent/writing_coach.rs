use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;

use crate::{
    AppError,
    agent::{AgentAnswer, AgentProgram, RunContext, TurnAction, UserTurn},
    corpus::{markdown::MarkdownKnowledgeTool, session_documents::SessionDocumentKnowledgeTool},
    domain::{FlowStage, RiskLevel, RouteDecision, RouteInput, SessionStateData},
    llm::ModelRequest,
    skills::{
        GeneralPromptContext, GroundingGuard, GuardPolicy, KnowledgeDecision,
        MaterialSearchService, PromptBuilder, PromptContext, SkillDefinition, SkillRegistry,
        SkillRouter, SlotFiller, SocraticPromptContext, ThinkingFlowController,
        build_knowledge_decision_prompt,
    },
    store::{
        runs::WritingTurnSkillEvent,
        sessions::{DocumentRepository, MessageRepository, SessionRepository},
    },
    tools::{
        KnowledgeBundle, KnowledgeCoordinator, KnowledgePlan, PlannedSearch, SearchHit,
        SearchRequest,
    },
};

use super::{
    conversation_memory::ConversationMemory,
    input_safety::{build_safety_response, classify_input},
};

pub struct WritingCoachProgram {
    sessions: SessionRepository,
    messages: MessageRepository,
    registry: SkillRegistry,
    router: SkillRouter,
    slot_filler: SlotFiller,
    thinking_flow: ThinkingFlowController,
    material_search: MaterialSearchService,
    prompt_builder: PromptBuilder,
    grounding_guard: GroundingGuard,
    knowledge: Arc<KnowledgeCoordinator>,
    web_enabled: bool,
}

#[derive(Clone, Copy)]
struct WebTurnState {
    requested: bool,
    available: bool,
    policy_enabled: bool,
}

impl WebTurnState {
    fn enabled(self) -> bool {
        self.requested && self.available && self.policy_enabled
    }
}

impl WritingCoachProgram {
    pub fn new(
        pool: SqlitePool,
        registry: SkillRegistry,
        knowledge: Arc<KnowledgeCoordinator>,
        web_enabled: bool,
    ) -> Self {
        let local_tools: Vec<Arc<dyn crate::tools::KnowledgeTool>> = vec![
            Arc::new(MarkdownKnowledgeTool::new(registry.clone())),
            Arc::new(SessionDocumentKnowledgeTool::new(DocumentRepository::new(
                pool.clone(),
            ))),
        ];
        let knowledge = Arc::new(knowledge.with_additional_tools(local_tools));
        Self {
            sessions: SessionRepository::new(pool.clone()),
            messages: MessageRepository::new(pool.clone()),
            router: SkillRouter::new(registry.clone()),
            registry,
            slot_filler: SlotFiller::new(),
            thinking_flow: ThinkingFlowController::new(),
            material_search: MaterialSearchService::new(5),
            prompt_builder: PromptBuilder::new(),
            grounding_guard: GroundingGuard::new(),
            knowledge,
            web_enabled,
        }
    }

    async fn start_phase(context: &RunContext, step: &str) -> Result<(), AppError> {
        context.emit("step.started", json!({"step": step})).await?;
        Ok(())
    }

    async fn complete_phase(context: &RunContext, step: &str) -> Result<(), AppError> {
        context
            .emit("step.completed", json!({"step": step}))
            .await?;
        Ok(())
    }

    async fn route(
        &self,
        _context: &RunContext,
        message: &str,
        state: &SessionStateData,
    ) -> Result<RouteDecision, AppError> {
        let route_input = route_input(message, state);
        Ok(self.router.route(&route_input))
    }

    async fn decide_knowledge_use(
        &self,
        context: &RunContext,
        skill: &SkillDefinition,
        message: &str,
        state: &SessionStateData,
        web_enabled: bool,
    ) -> Result<KnowledgeDecision, AppError> {
        let fallback = KnowledgeDecision::fallback(&skill.id, message, web_enabled);
        let request = ModelRequest {
            messages: build_knowledge_decision_prompt(&skill.id, message, state),
            temperature: Some(0.0),
        };
        match context.call_model("decide_knowledge_use", request).await {
            Ok(response) => {
                Ok(KnowledgeDecision::parse(&response.content, web_enabled).unwrap_or(fallback))
            }
            Err(AppError::Model(_)) => Ok(fallback),
            Err(error) => Err(error),
        }
    }
}

#[async_trait]
impl AgentProgram for WritingCoachProgram {
    async fn execute(&self, context: RunContext, turn: UserTurn) -> Result<AgentAnswer, AppError> {
        let web = WebTurnState {
            requested: turn.enable_web_search,
            available: self.knowledge.has_tool("web"),
            policy_enabled: self.web_enabled,
        };
        let web_enabled = web.enabled();
        Self::start_phase(&context, "accept_input").await?;
        let persisted = self.sessions.load_state(turn.session_id).await?;
        let mut state = SessionStateData::from_legacy_json(persisted.state_json)?;
        let recent_messages = self.messages.list_by_session(turn.session_id).await?;
        let mut memory = ConversationMemory::from_messages(
            &recent_messages,
            &state,
            Some(turn.content.as_str()),
        );
        if let Some(summary) = memory.durable_summary.as_ref() {
            state.extra.insert(
                "conversation_memory_summary".to_owned(),
                Value::String(summary.clone()),
            );
        }
        let user_message = self
            .messages
            .add(
                turn.session_id,
                "user",
                turn.content.trim(),
                Some(
                    json!({"run_id": context.run_id().to_legacy_hex(), "intent": "skill_message"}),
                ),
            )
            .await?;
        Self::complete_phase(&context, "accept_input").await?;

        Self::start_phase(&context, "classify_input_safety").await?;
        let safety = classify_input(&turn.content);
        Self::complete_phase(&context, "classify_input_safety").await?;
        if !safety.is_safe() {
            Self::start_phase(&context, "safety_intercept").await?;
            let answer = build_safety_response(safety)
                .ok_or_else(|| AppError::InvalidRun("unsafe input has no response".to_owned()))?;
            let current_skill = state
                .extra
                .get("current_skill")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let route = compatibility_route(&state, current_skill, "safety_intercept");
            let safety_metadata = json!({
                "category": safety.category,
                "severity": safety.severity,
                "action": safety.action,
                "reason_code": safety.reason_code,
            });
            let metadata = json!({
                "skill_id": current_skill,
                "selected_skill": current_skill,
                "answer_type": "safety_response",
                "awaiting_slots": awaiting_slots(&state),
                "used_corpus_files": [],
                "route_decision": route,
                "student_progress": compatibility_progress(&state, &route, current_skill),
                "safety": safety_metadata,
                "used_web_search": false,
                "search_requested": false,
                "literature_search": idle_literature_search(),
                "web_search": idle_web_search(web),
                "guardrail_triggered": true,
                "guardrail": {"allowed": false, "violations": [safety.reason_code]},
            });
            self.messages
                .update_metadata(
                    user_message.id,
                    json!({
                        "run_id": context.run_id().to_legacy_hex(),
                        "skill_id": current_skill,
                        "intent": "safety_intercept",
                        "safety": safety_metadata,
                    }),
                )
                .await?;
            context
                .persist_terminal_writing_turn(
                    turn.session_id,
                    serde_json::to_value(&state).map_err(corrupt_json)?,
                    answer,
                    metadata.clone(),
                    &[],
                    "safety_intercept",
                )
                .await?;
            return Ok(AgentAnswer::new(answer).with_metadata(metadata));
        }

        if turn.action == Some(TurnAction::Synthesize) {
            Self::start_phase(&context, "synthesize").await?;
            state.apply_user_message(&turn.content);
            let answer = synthesize_writing_context(&state);
            state
                .extra
                .insert("awaiting_slots".to_owned(), Value::Array(Vec::new()));
            state.extra.remove("last_question");
            let current_skill = state
                .extra
                .get("current_skill")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let route = compatibility_route(&state, current_skill, "synthesize");
            let metadata = json!({
                "skill_id": current_skill,
                "selected_skill": current_skill,
                "action": "synthesize",
                "answer_type": "synthesis",
                "awaiting_slots": [],
                "used_corpus_files": [],
                "route_decision": route,
                "student_progress": compatibility_progress(&state, &route, current_skill),
                "used_web_search": false,
                "search_requested": false,
                "literature_search": idle_literature_search(),
                "web_search": idle_web_search(web),
                "guardrail_triggered": false,
                "guardrail": {"allowed": true, "violations": []},
            });
            self.messages
                .update_metadata(
                    user_message.id,
                    json!({
                        "run_id": context.run_id().to_legacy_hex(),
                        "skill_id": current_skill,
                        "intent": "synthesize",
                        "action": "synthesize",
                    }),
                )
                .await?;
            context
                .persist_terminal_writing_turn(
                    turn.session_id,
                    serde_json::to_value(&state).map_err(corrupt_json)?,
                    &answer,
                    metadata.clone(),
                    &[],
                    "synthesize",
                )
                .await?;
            return Ok(AgentAnswer::new(answer).with_metadata(metadata));
        }

        if is_reset_command(&turn.content) {
            Self::start_phase(&context, "reset_context").await?;
            state = SessionStateData::default();
            let answer = "已清空当前写作任务。你可以直接告诉我新的作业、主题或困惑。";
            let route = compatibility_route(&state, None, "reset");
            let metadata = json!({
                "skill_id": null,
                "selected_skill": null,
                "used_corpus_files": [],
                "route_decision": route,
                "student_progress": compatibility_progress(&state, &route, None),
                "general_response": true,
                "reset": true,
                "used_web_search": false,
                "search_requested": false,
                "literature_search": idle_literature_search(),
                "web_search": idle_web_search(web),
                "guardrail_triggered": false,
                "guardrail": {"allowed": true, "violations": []},
            });
            self.messages
                .update_metadata(
                    user_message.id,
                    json!({
                        "run_id": context.run_id().to_legacy_hex(),
                        "skill_id": null,
                        "intent": "reset"
                    }),
                )
                .await?;
            context
                .persist_terminal_writing_turn(
                    turn.session_id,
                    serde_json::to_value(&state).map_err(corrupt_json)?,
                    answer,
                    metadata.clone(),
                    &[],
                    "reset_context",
                )
                .await?;
            return Ok(AgentAnswer::new(answer).with_metadata(metadata));
        }

        let (routing_message, explicit_switch) = routing_message(&turn.content);
        if explicit_switch {
            state.extra.remove("awaiting_slots");
            state.extra.remove("collected_slots");
        }

        Self::start_phase(&context, "route_skill").await?;
        let route = self.route(&context, &turn.content, &state).await?;
        Self::complete_phase(&context, "route_skill").await?;

        if route.target_skill.is_none() {
            state.update_from_user(routing_message, None, &Map::new());
            memory.refresh_confirmed_facts(&state);
            Self::start_phase(&context, "general_response").await?;
            let prompt = self.prompt_builder.build_general(GeneralPromptContext {
                state: &state,
                recent_messages: &memory.recent_messages,
                durable_summary: memory.durable_summary.as_deref(),
                confirmed_facts: &memory.confirmed_facts,
                user_message: &turn.content,
            });
            let response = context
                .call_model(
                    "general_answer",
                    ModelRequest {
                        messages: prompt,
                        temperature: Some(0.3),
                    },
                )
                .await?;
            let empty_knowledge = KnowledgeBundle::default();
            let user_texts = memory
                .recent_messages
                .iter()
                .filter(|message| message.role == "user")
                .map(|message| message.content.clone())
                .chain(std::iter::once(turn.content.clone()));
            let guarded = self.grounding_guard.validate(
                &response.content,
                &empty_knowledge,
                GuardPolicy::default().with_user_texts(user_texts),
            );
            state
                .extra
                .insert("awaiting_slots".to_owned(), Value::Array(Vec::new()));
            state.extra.remove("last_question");
            state.extra.insert(
                "latest_request".to_owned(),
                Value::String(turn.content.trim().to_owned()),
            );
            let current_skill = state
                .extra
                .get("current_skill")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let metadata = json!({
                "selected_skill": null,
                "skill_id": current_skill,
                "intent": "general_response",
                "answer_type": "general_response",
                "general": true,
                "general_response": true,
                "used_corpus_files": [],
                "route_decision": route,
                "student_progress": student_progress(&state, &route, current_skill.as_deref(), &[]),
                "used_web_search": false,
                "search_requested": false,
                "literature_search": idle_literature_search(),
                "web_search": idle_web_search(web),
                "guardrail_triggered": guarded.triggered,
                "guardrail": {
                    "allowed": guarded.allowed,
                    "violations": guarded.violations,
                    "grounding_valid": guarded.grounding_valid,
                },
            });
            self.messages
                .update_metadata(
                    user_message.id,
                    json!({
                        "run_id": context.run_id().to_legacy_hex(),
                        "skill_id": current_skill,
                        "intent": "general_message"
                    }),
                )
                .await?;
            context
                .persist_terminal_writing_turn(
                    turn.session_id,
                    serde_json::to_value(&state).map_err(corrupt_json)?,
                    &guarded.answer,
                    metadata.clone(),
                    &[],
                    "general_response",
                )
                .await?;
            return Ok(AgentAnswer::new(guarded.answer).with_metadata(metadata));
        }

        let skill_id = route
            .target_skill
            .as_deref()
            .expect("general turns returned before skill lookup")
            .to_owned();
        let skill = self
            .registry
            .get(&skill_id)
            .ok_or_else(|| AppError::InvalidRun("selected skill is not installed".to_owned()))?;
        self.messages
            .update_metadata(
                user_message.id,
                json!({
                    "run_id": context.run_id().to_legacy_hex(),
                    "skill_id": skill.id,
                    "intent": route.intent,
                    "route_decision": route,
                }),
            )
            .await?;

        Self::start_phase(&context, "fill_required_slots").await?;
        let collected = self.slot_filler.fill(skill, &state, routing_message);
        if state.extra.get("latest_draft").is_none()
            && let Some(draft) = collected.get("draft_text").and_then(Value::as_str)
        {
            state
                .extra
                .insert("latest_draft".to_owned(), Value::String(draft.to_owned()));
        }
        let missing = self.slot_filler.missing_slots(skill, &collected);
        state.task_type = Some(skill.id.clone());
        state
            .extra
            .insert("current_skill".to_owned(), Value::String(skill.id.clone()));
        state.extra.insert(
            "collected_slots".to_owned(),
            Value::Object(collected.clone()),
        );
        state.extra.insert(
            "awaiting_slots".to_owned(),
            serde_json::to_value(&missing).unwrap_or_else(|_| json!([])),
        );
        Self::complete_phase(&context, "fill_required_slots").await?;

        Self::start_phase(&context, "update_writing_context").await?;
        let context_update = state.update_from_user(routing_message, Some(&skill.id), &collected);
        state.task_type = Some(skill.id.clone());
        state
            .extra
            .insert("current_skill".to_owned(), Value::String(skill.id.clone()));
        state.extra.insert(
            "collected_slots".to_owned(),
            Value::Object(collected.clone()),
        );
        state.extra.insert(
            "awaiting_slots".to_owned(),
            serde_json::to_value(&missing).unwrap_or_else(|_| json!([])),
        );
        memory.refresh_confirmed_facts(&state);
        let flow_decision = if skill.id == "socratic_review" {
            let flow = self
                .thinking_flow
                .advance(&state.writing_context, routing_message);
            if let Some(selected) = flow.selected_option.as_deref()
                && let Some(path) = state
                    .writing_context
                    .candidate_paths
                    .iter()
                    .find(|path| path.index == selected)
                    .cloned()
            {
                state.writing_context.selected_path_id = Some(selected.to_owned());
                state.writing_context.selected_path = Some(path.title.clone());
                state.writing_context.selected_direction = Some(path.title.clone());
                state.writing_context.selected_path_detail = Some(path);
            }
            state.writing_context.candidate_paths = flow.candidate_paths.clone();
            state.writing_context.thinking_stage = Some(flow.stage.clone());
            state.writing_context.flow_stage = Some(flow.stage.clone());
            state.writing_context.ready_for_refined_advice = flow.stage == FlowStage::RefinedAdvice;
            Some(flow)
        } else {
            None
        };
        let thinking_stage = flow_decision.as_ref().map(|flow| flow.stage.clone());
        let revision_comparison = update_revision_state(&mut state, routing_message);
        record_route(&mut state, &route);
        Self::complete_phase(&context, "update_writing_context").await?;

        if !missing.is_empty() {
            let question = self.slot_filler.next_question(skill, &missing);
            Self::start_phase(&context, "persist_answer").await?;
            let metadata = answer_metadata(
                &state,
                skill,
                &route,
                &KnowledgeBundle::default(),
                web,
                false,
                true,
                &missing,
                thinking_stage.as_ref(),
                revision_comparison,
                context_update.topic_changed,
                None,
            );
            context
                .persist_terminal_writing_turn(
                    turn.session_id,
                    serde_json::to_value(&state).map_err(corrupt_json)?,
                    &question,
                    metadata.clone(),
                    &[WritingTurnSkillEvent::new(
                        &skill.id,
                        "selected",
                        json!({
                            "run_id": context.run_id().to_legacy_hex(),
                            "awaiting_slots": missing,
                            "route_decision": route,
                        }),
                    )],
                    "persist_answer",
                )
                .await?;
            return Ok(AgentAnswer::new(question).with_metadata(metadata));
        }

        Self::start_phase(&context, "decide_knowledge_use").await?;
        let knowledge_decision = if skill.id == "material_search" {
            KnowledgeDecision {
                use_course_corpus: true,
                use_external_search: web_enabled,
                query: routing_message.to_owned(),
                reason: "材料检索 skill 直接执行结构化检索".to_owned(),
                decider: "deterministic".to_owned(),
            }
        } else {
            self.decide_knowledge_use(&context, skill, routing_message, &state, web_enabled)
                .await?
        };
        let deterministic_material_reply = skill.id == "material_search"
            || (skill.id == "novelty_eval" && knowledge_decision.use_external_search);
        let material_plan = deterministic_material_reply.then(|| {
            self.material_search
                .build_plan(routing_message, &state.writing_context)
        });
        let plan = knowledge_plan(
            skill,
            routing_message,
            web_enabled,
            turn.session_id,
            material_plan.as_ref(),
            &knowledge_decision,
        );
        Self::complete_phase(&context, "decide_knowledge_use").await?;

        Self::start_phase(&context, "search_knowledge").await?;
        let knowledge = if plan.searches.is_empty() {
            KnowledgeBundle::default()
        } else {
            context
                .execute_knowledge(self.knowledge.as_ref(), plan)
                .await?
        };
        Self::complete_phase(&context, "search_knowledge").await?;

        if deterministic_material_reply && let Some(material_plan) = material_plan.as_ref() {
            Self::start_phase(&context, "build_material_reply").await?;
            let raw_reply =
                self.material_search
                    .build_reply(material_plan, &knowledge, web_enabled);
            Self::complete_phase(&context, "build_material_reply").await?;
            Self::start_phase(&context, "validate_grounding_and_guardrail").await?;
            let user_texts = memory
                .recent_messages
                .iter()
                .filter(|message| message.role == "user")
                .map(|message| message.content.clone())
                .chain(std::iter::once(turn.content.clone()));
            let guarded = self.grounding_guard.validate(
                &raw_reply,
                &knowledge,
                GuardPolicy::default().with_user_texts(user_texts),
            );
            Self::complete_phase(&context, "validate_grounding_and_guardrail").await?;
            Self::start_phase(&context, "persist_answer").await?;
            state
                .extra
                .insert("awaiting_slots".to_owned(), Value::Array(Vec::new()));
            state.extra.remove("last_question");
            state.update_after_reply(&guarded.answer, Some(&skill.id));
            let mut metadata = answer_metadata(
                &state,
                skill,
                &route,
                &knowledge,
                web,
                guarded.triggered,
                false,
                &[],
                thinking_stage.as_ref(),
                revision_comparison,
                context_update.topic_changed,
                Some(&knowledge_decision),
            );
            metadata["grounding_valid"] = Value::Bool(guarded.grounding_valid);
            metadata["grounding_sources"] =
                serde_json::to_value(&guarded.sources).unwrap_or_else(|_| json!([]));
            metadata["guardrail"] = json!({
                "allowed": guarded.allowed,
                "violations": guarded.violations,
            });
            enrich_material_search_metadata(&mut metadata, material_plan, &knowledge);
            context
                .persist_terminal_writing_turn(
                    turn.session_id,
                    serde_json::to_value(&state).map_err(corrupt_json)?,
                    &guarded.answer,
                    metadata.clone(),
                    &[
                        WritingTurnSkillEvent::new(
                            &skill.id,
                            "selected",
                            json!({"run_id": context.run_id().to_legacy_hex(), "awaiting_slots": [], "route_decision": route}),
                        ),
                        WritingTurnSkillEvent::new(
                            &skill.id,
                            "answered",
                            json!({"run_id": context.run_id().to_legacy_hex(), "used_corpus_files": metadata["used_corpus_files"], "guardrail_triggered": guarded.triggered, "grounding_valid": guarded.grounding_valid}),
                        ),
                    ],
                    "persist_answer",
                )
                .await?;
            return Ok(AgentAnswer::new(guarded.answer).with_metadata(metadata));
        }

        Self::start_phase(&context, "build_prompt").await?;
        let prompt = if let Some(flow) = flow_decision.as_ref() {
            self.prompt_builder
                .build_socratic_humanizer(SocraticPromptContext {
                    policies: self.registry.policies(),
                    skill,
                    state: &state,
                    flow,
                    recent_messages: &memory.recent_messages,
                    user_message: routing_message,
                })
        } else {
            self.prompt_builder.build(PromptContext {
                policies: self.registry.policies(),
                skill,
                state: &state,
                recent_messages: &memory.recent_messages,
                durable_summary: memory.durable_summary.as_deref(),
                confirmed_facts: &memory.confirmed_facts,
                knowledge: &knowledge,
                user_message: routing_message,
                web_enabled,
            })
        };
        Self::complete_phase(&context, "build_prompt").await?;

        Self::start_phase(&context, "call_model").await?;
        let response = context
            .call_model(
                "writing_coach_answer",
                ModelRequest {
                    messages: prompt,
                    temperature: Some(if flow_decision.is_some() { 0.55 } else { 0.2 }),
                },
            )
            .await?;
        Self::complete_phase(&context, "call_model").await?;

        Self::start_phase(&context, "validate_grounding_and_guardrail").await?;
        let user_texts = memory
            .recent_messages
            .iter()
            .filter(|message| message.role == "user")
            .map(|message| message.content.clone())
            .chain(std::iter::once(turn.content.clone()));
        let guard_policy = GuardPolicy::default()
            .with_user_texts(user_texts)
            .with_direct_delivery_risk(
                route.risk == RiskLevel::GhostwritingRisk
                    || is_direct_delivery_request(&turn.content),
            );
        let guarded = self
            .grounding_guard
            .validate(&response.content, &knowledge, guard_policy);
        Self::complete_phase(&context, "validate_grounding_and_guardrail").await?;

        Self::start_phase(&context, "persist_answer").await?;
        state
            .extra
            .insert("awaiting_slots".to_owned(), Value::Array(Vec::new()));
        state.extra.remove("last_question");
        state.update_after_reply(&guarded.answer, Some(&skill.id));
        let mut metadata = answer_metadata(
            &state,
            skill,
            &route,
            &knowledge,
            web,
            guarded.triggered,
            false,
            &[],
            thinking_stage.as_ref(),
            revision_comparison,
            context_update.topic_changed,
            Some(&knowledge_decision),
        );
        metadata["grounding_valid"] = Value::Bool(guarded.grounding_valid);
        metadata["grounding_sources"] =
            serde_json::to_value(&guarded.sources).unwrap_or_else(|_| json!([]));
        metadata["guardrail"] = json!({
            "allowed": guarded.allowed,
            "violations": guarded.violations,
        });
        context
            .persist_terminal_writing_turn(
                turn.session_id,
                serde_json::to_value(&state).map_err(corrupt_json)?,
                &guarded.answer,
                metadata.clone(),
                &[
                    WritingTurnSkillEvent::new(
                        &skill.id,
                        "selected",
                        json!({
                            "run_id": context.run_id().to_legacy_hex(),
                            "awaiting_slots": [],
                            "route_decision": route,
                        }),
                    ),
                    WritingTurnSkillEvent::new(
                        &skill.id,
                        "answered",
                        json!({
                            "run_id": context.run_id().to_legacy_hex(),
                            "used_corpus_files": metadata["used_corpus_files"],
                            "guardrail_triggered": guarded.triggered,
                            "grounding_valid": guarded.grounding_valid,
                        }),
                    ),
                ],
                "persist_answer",
            )
            .await?;
        Ok(AgentAnswer::new(guarded.answer).with_metadata(metadata))
    }
}

fn route_input(message: &str, state: &SessionStateData) -> RouteInput {
    let awaiting = awaiting_slots(state);
    let collected = state
        .extra
        .get("collected_slots")
        .and_then(Value::as_object)
        .is_some_and(|slots| !slots.is_empty());
    let mut input = RouteInput::new(message)
        .with_writing_context(
            serde_json::to_value(&state.writing_context).unwrap_or_else(|_| json!({})),
        )
        .with_awaiting_slots(awaiting)
        .with_collected_slots(collected);
    if let Some(skill) = state.extra.get("current_skill").and_then(Value::as_str)
        && !skill.is_empty()
    {
        input = input.with_current_skill(skill);
    }
    input
}

fn awaiting_slots(state: &SessionStateData) -> Vec<String> {
    state
        .extra
        .get("awaiting_slots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

fn compatibility_route(
    state: &SessionStateData,
    target_skill: Option<&str>,
    intent: &str,
) -> RouteDecision {
    RouteDecision {
        stage: state.writing_context.stage.clone(),
        intent: intent.to_owned(),
        risk: RiskLevel::DirectOk,
        target_skill: target_skill.map(str::to_owned),
        confidence: 1.0,
        reason: "deterministic compatibility path".to_owned(),
        required_context: Vec::new(),
        needs_socratic: false,
    }
}

fn compatibility_progress(
    state: &SessionStateData,
    route: &RouteDecision,
    current_skill: Option<&str>,
) -> Value {
    let pending = awaiting_slots(state);
    json!({
        "stage": route.stage.as_str(),
        "intent": route.intent,
        "current_skill": current_skill,
        "thinking_stage": state.writing_context.thinking_stage.as_ref().map(FlowStage::as_str),
        "thinking_task": state.writing_context.extra.get("thinking_task"),
        "topic": state.writing_context.topic,
        "research_question": state.writing_context.research_question,
        "selected_path": state.writing_context.selected_direction,
        "choice_reason": state.writing_context.choice_reason,
        "socratic_rounds": state.writing_context.socratic_rounds,
        "pending_questions": pending,
        "next_task": if pending.is_empty() {
            "继续补充主题、观察和材料，我会把它推进成可研究问题。"
        } else {
            "先回答当前追问，再进入下一步建议。"
        },
        "needs_teacher_confirmation": false,
    })
}

fn student_progress(
    state: &SessionStateData,
    route: &RouteDecision,
    current_skill: Option<&str>,
    awaiting: &[String],
) -> Value {
    let writing = &state.writing_context;
    let pending = ["pending_questions", "unanswered_questions"]
        .into_iter()
        .filter_map(|key| writing.extra.get(key).and_then(Value::as_array))
        .find(|items| !items.is_empty())
        .map(|items| items.iter().take(3).cloned().collect::<Vec<_>>())
        .unwrap_or_else(|| {
            awaiting
                .iter()
                .take(3)
                .cloned()
                .map(Value::String)
                .collect()
        });
    let next_task = if !pending.is_empty() {
        "先回答当前追问，再进入下一步建议。"
    } else if route
        .required_context
        .iter()
        .any(|item| item == "motivation")
    {
        "补充你为什么关心这个题，以及最初观察到的具体场景。"
    } else if route
        .required_context
        .iter()
        .any(|item| item == "choice_reason")
    {
        "说明为什么选择这个方向，也说一个没有选择其他方向的理由。"
    } else if route
        .required_context
        .iter()
        .any(|item| item == "material_source")
    {
        "列出你能拿到的材料来源，例如访谈、帖子、课程案例或个人经历。"
    } else if writing.thinking_stage == Some(FlowStage::EvidenceCheck) {
        "补 1 个支持案例和 1 个可能反例，用来检验题目是否站得住。"
    } else if writing.research_question.is_some() {
        "检查研究问题的对象、机制、材料和反方观点是否都清楚。"
    } else {
        "继续补充主题、观察和材料，我会把它推进成可研究问题。"
    };
    json!({
        "stage": route.stage.as_str(),
        "intent": writing.extra.get("last_intent").and_then(Value::as_str).unwrap_or(&route.intent),
        "current_skill": current_skill,
        "thinking_stage": writing.thinking_stage.as_ref().or(writing.flow_stage.as_ref()).map(FlowStage::as_str),
        "thinking_task": writing.extra.get("thinking_task"),
        "topic": writing.topic.as_deref().or(writing.initial_idea.as_deref()),
        "context_summary": writing.context_summary,
        "research_question": writing.research_question,
        "selected_path": writing.selected_path.as_deref().or(writing.selected_direction.as_deref()),
        "choice_reason": writing.choice_reason,
        "socratic_rounds": writing.socratic_rounds,
        "pending_questions": pending,
        "next_task": next_task,
        "needs_teacher_confirmation": writing.extra.get("unanswered_questions").and_then(Value::as_array).is_some_and(|items| !items.is_empty()),
    })
}

fn idle_literature_search() -> Value {
    json!({
        "enabled": false,
        "triggered": false,
        "error": null,
        "results": [],
    })
}

fn idle_web_search(state: WebTurnState) -> Value {
    json!({
        "requested": state.requested,
        "consent": state.requested,
        "available": state.available,
        "policy_enabled": state.policy_enabled,
        "enabled": false,
        "attempted": false,
        "used": false,
        "triggered": false,
        "error": null,
        "results": [],
    })
}

fn record_route(state: &mut SessionStateData, route: &RouteDecision) {
    state.writing_context.stage = route.stage.clone();
    state.writing_context.extra.insert(
        "last_intent".to_owned(),
        Value::String(route.intent.clone()),
    );
    let route_value = serde_json::to_value(route).unwrap_or_else(|_| json!({}));
    state
        .writing_context
        .extra
        .insert("route_decision".to_owned(), route_value.clone());
    let history = state
        .writing_context
        .extra
        .entry("route_history".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(history) = history.as_array_mut() {
        history.push(route_value);
        if history.len() > 12 {
            history.drain(..history.len() - 12);
        }
    }
}

fn knowledge_plan(
    skill: &SkillDefinition,
    message: &str,
    web_enabled: bool,
    session_id: crate::domain::SessionId,
    material: Option<&crate::skills::MaterialSearchPlan>,
    decision: &KnowledgeDecision,
) -> KnowledgePlan {
    let mut tools = Vec::new();
    if decision.use_course_corpus {
        tools.push("course_corpus");
    }
    if matches!(skill.id.as_str(), "draft_diagnosis" | "writing_feedback") {
        tools.push("session_documents");
    }
    if decision.use_external_search && web_enabled {
        tools.extend(["scholarly", "web"]);
    }
    let years = material
        .map(|plan| (plan.year_from, plan.year_to))
        .unwrap_or_else(|| extract_year_range(message));
    KnowledgePlan::new(tools.into_iter().map(|tool| {
        let request = match (tool, material) {
            ("course_corpus", Some(plan)) => SearchRequest::new(&plan.corpus_query),
            ("scholarly", Some(plan)) => SearchRequest::from_terms(plan.query_terms.clone())
                .with_max_results(plan.max_results),
            ("web", Some(plan)) => SearchRequest::new(&plan.web_query),
            _ => SearchRequest::new(message),
        };
        PlannedSearch::new(
            tool,
            request
                .with_year_range(years.0, years.1)
                .with_session_id(session_id)
                .with_target_skill_id(&skill.id),
        )
    }))
}

fn extract_year_range(message: &str) -> (Option<i32>, Option<i32>) {
    let years = message
        .split(|character: char| !character.is_ascii_digit())
        .filter(|value| value.len() == 4)
        .filter_map(|value| value.parse::<i32>().ok())
        .filter(|year| (1900..=2200).contains(year))
        .collect::<Vec<_>>();
    match years.as_slice() {
        [year, ..] if contains_any(message, &["后", "以后", "起"]) => (Some(*year), None),
        [year, ..] if contains_any(message, &["前", "以前", "截止"]) => (None, Some(*year)),
        [start, end, ..] => (Some(*start), Some(*end)),
        [year] => (Some(*year), Some(*year)),
        _ => (None, None),
    }
}

fn update_revision_state(state: &mut SessionStateData, message: &str) -> Value {
    let is_revision = contains_any(
        message,
        &["修改稿", "修改后", "新版全文", "revision", "revised"],
    );
    if !is_revision {
        if contains_any(message, &["初稿", "草稿", "原稿", "原文"]) {
            state.extra.insert(
                "latest_draft".to_owned(),
                Value::String(message.trim().to_owned()),
            );
        }
        return json!({"is_revision": false});
    }
    let previous = state
        .extra
        .get("latest_draft")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            state
                .extra
                .get("revision_history")
                .and_then(Value::as_array)
                .and_then(|items| items.last())
                .and_then(|item| item.get("revision_text"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let history = state
        .extra
        .entry("revision_history".to_owned())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(history) = history.as_array_mut() {
        history.push(json!({
            "revision_text": message.trim(),
            "status": "submitted",
        }));
    }
    state.extra.insert(
        "latest_draft".to_owned(),
        Value::String(message.trim().to_owned()),
    );
    json!({
        "is_revision": true,
        "has_previous_revision": previous.is_some(),
        "previous_length": previous.as_deref().map(str::chars).map(Iterator::count),
        "current_length": message.chars().count(),
    })
}

#[allow(clippy::too_many_arguments)]
fn answer_metadata(
    state: &SessionStateData,
    skill: &SkillDefinition,
    route: &RouteDecision,
    knowledge: &KnowledgeBundle,
    web_state: WebTurnState,
    guardrail_triggered: bool,
    slot_question: bool,
    missing: &[String],
    thinking_stage: Option<&FlowStage>,
    revision_comparison: Value,
    topic_changed: bool,
    knowledge_decision: Option<&KnowledgeDecision>,
) -> Value {
    let used_corpus = knowledge
        .hits
        .iter()
        .filter(|hit| matches!(hit.provider.as_str(), "corpus" | "course_corpus"))
        .map(|hit| hit.source.clone())
        .collect::<BTreeSet<_>>();
    let literature = filtered_sources(&knowledge.hits, |provider| {
        !matches!(
            provider,
            "corpus" | "course_corpus" | "session_document" | "web" | "searxng" | "bing" | "brave"
        )
    });
    let web = filtered_sources(&knowledge.hits, |provider| {
        matches!(provider, "web" | "searxng" | "bing" | "brave")
    });
    let course_requested = knowledge_decision.is_some_and(|decision| decision.use_course_corpus);
    let external_requested =
        knowledge_decision.is_some_and(|decision| decision.use_external_search);
    let structured_search =
        skill.id == "material_search" || (skill.id == "novelty_eval" && external_requested);
    let web_enabled = web_state.enabled() && external_requested;
    let web_attempted = web_enabled && !slot_question;
    let web_error = if !structured_search || !web_state.requested {
        None
    } else if !web_state.available {
        Some("web_search_unavailable")
    } else if !web_state.policy_enabled {
        Some("web_search_disabled")
    } else {
        None
    };
    json!({
        "skill_id": skill.id,
        "selected_skill": skill.id,
        "answer_type": if slot_question { "slot_question" } else { "answer" },
        "awaiting_slots": missing,
        "used_corpus_files": used_corpus,
        "route_decision": route,
        "thinking_stage": thinking_stage.map(FlowStage::as_str),
        "topic_changed": topic_changed,
        "revision_comparison": revision_comparison,
        "student_progress": student_progress(state, route, Some(&skill.id), missing),
        "search_requested": !slot_question && (course_requested || web_attempted),
        "search_has_hits": !knowledge.hits.is_empty(),
        "external_search_requested": external_requested,
        "web_search_requested": web_state.requested,
        "used_web_search": !web.is_empty(),
        "knowledge_use": {
            "use_course_corpus": course_requested,
            "course_hit_count": used_corpus.len(),
            "use_external_search": web_attempted,
            "query": knowledge_decision.map(|decision| decision.query.as_str()),
            "reason": knowledge_decision.map(|decision| decision.reason.as_str()),
            "decider": knowledge_decision.map(|decision| decision.decider.as_str()).unwrap_or("not_run"),
        },
        "literature_search": {
            "enabled": web_attempted,
            "available": web_state.available,
            "requested": web_state.requested,
            "triggered": structured_search,
            "error": web_error,
            "results": literature,
        },
        "web_search": {
            "requested": web_state.requested,
            "consent": web_state.requested,
            "available": web_state.available,
            "policy_enabled": web_state.policy_enabled,
            "enabled": web_enabled,
            "attempted": web_attempted,
            "used": !web.is_empty(),
            "triggered": structured_search,
            "error": web_error,
            "results": web,
        },
        "grounding_sources": knowledge.hits.iter().map(source_value).collect::<Vec<_>>(),
        "provider_failures": knowledge.metadata.provider_failures,
        "guardrail_triggered": guardrail_triggered,
    })
}

fn enrich_material_search_metadata(
    metadata: &mut Value,
    plan: &crate::skills::MaterialSearchPlan,
    knowledge: &KnowledgeBundle,
) {
    metadata["literature_search"]["provider"] = json!("scholarly");
    metadata["literature_search"]["query"] = json!(plan.query_terms);
    metadata["literature_search"]["year_from"] = json!(plan.year_from);
    metadata["literature_search"]["year_to"] = json!(plan.year_to);
    metadata["web_search"]["query"] = json!(plan.web_query);

    let is_web_provider = |provider: &str| {
        matches!(provider, "web" | "searxng" | "bing" | "brave") || provider.starts_with("web:")
    };
    if let Some(failure) = knowledge
        .metadata
        .provider_failures
        .iter()
        .find(|failure| !is_web_provider(&failure.provider))
    {
        metadata["literature_search"]["error"] = json!(failure.message);
    }
    if let Some(failure) = knowledge
        .metadata
        .provider_failures
        .iter()
        .find(|failure| is_web_provider(&failure.provider))
    {
        metadata["web_search"]["error"] = json!(failure.message);
    }
}

fn filtered_sources(hits: &[SearchHit], include: impl Fn(&str) -> bool) -> Vec<Value> {
    hits.iter()
        .filter(|hit| include(&hit.provider))
        .map(source_value)
        .collect()
}

fn source_value(hit: &SearchHit) -> Value {
    json!({
        "source": hit.source,
        "title": hit.title,
        "provider": hit.provider,
        "url": hit.url,
        "doi": hit.doi,
    })
}

fn contains_any(text: &str, patterns: &[&str]) -> bool {
    let lowered = text.to_lowercase();
    patterns
        .iter()
        .any(|pattern| lowered.contains(&pattern.to_lowercase()))
}

fn is_direct_delivery_request(message: &str) -> bool {
    contains_any(
        &message.to_ascii_lowercase(),
        &[
            "帮我写",
            "直接写",
            "替我写",
            "完整范文",
            "生成全文",
            "写完整",
            "直接提交",
            "交作业",
        ],
    )
}

fn is_reset_command(message: &str) -> bool {
    matches!(
        message.trim().to_ascii_lowercase().as_str(),
        "reset" | "/reset" | "重置" | "清空" | "重新开始" | "换个任务"
    )
}

fn routing_message(message: &str) -> (&str, bool) {
    for marker in ["切换分支", "切换到"] {
        if let Some((_, payload)) = message.split_once(marker) {
            return (
                payload.trim_start_matches(['：', ':', '，', ',', '。', ' ']),
                true,
            );
        }
    }
    (message.trim(), false)
}

fn synthesize_writing_context(state: &SessionStateData) -> String {
    let writing = &state.writing_context;
    let topic = writing
        .topic
        .as_deref()
        .or(writing.initial_idea.as_deref())
        .unwrap_or("当前写作方向");
    let claim = writing
        .core_claim
        .as_deref()
        .or(writing.confusion_point.as_deref())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("围绕“{topic}”解释一个具体、可观察且存在争议的现象"));
    let evidence = if writing.evidence.is_empty() {
        "个人观察、访谈或可核验的公开材料".to_owned()
    } else {
        writing
            .evidence
            .iter()
            .take(3)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("、")
    };
    format!(
        "## 选题雏形\n\n{topic}\n\n\
## 核心判断\n\n{claim}。写作重点是解释它如何发生、受什么条件影响，而不是只描述现象。\n\n\
## 概念关系\n\n先界定核心概念，再区分相近概念，最后说明它们之间可能存在的机制关系。\n\n\
## 论证路径\n\n1. 用具体场景界定现象和讨论范围。\n\
2. 提出核心机制，并解释各环节如何连接。\n\
3. 用支持材料和反例检验判断。\n\
4. 说明判断成立的条件、边界及可能反驳。\n\n\
## 材料建议\n\n优先整理：{evidence}。材料必须能够支撑机制判断，而不只是证明现象存在。\n\n\
## 待核实事项\n\n核实概念来源、材料代表性和反例；未确认的作者、理论和数据不要写成事实。"
    )
}

fn corrupt_json(error: serde_json::Error) -> AppError {
    AppError::CorruptData(format!(
        "could not serialize canonical writing state: {error}"
    ))
}
