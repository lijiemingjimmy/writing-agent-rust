use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;

use crate::{
    AppError,
    agent::{AgentAnswer, AgentProgram, RunContext, TurnAction, UserTurn},
    corpus::{markdown::MarkdownKnowledgeTool, session_documents::SessionDocumentKnowledgeTool},
    domain::{FlowStage, RiskLevel, RouteDecision, RouteInput, SessionStateData},
    llm::{ModelMessage, ModelRequest},
    skills::{
        BranchController, BranchResolution, GeneralPromptContext, GroundingGuard, GuardPolicy,
        KnowledgeDecision, MaterialSearchService, PromptBuilder, PromptContext, SkillDefinition,
        SkillRegistry, SkillRouter, SlotFiller, SocraticPromptContext, ThinkingFlowController,
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
    domain_boundary::evaluate_domain_boundary,
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
    branch_controller: BranchController,
    context_max_chars: usize,
    context_recent_chars: usize,
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
        Self::new_with_context_limits(
            pool,
            registry,
            knowledge,
            web_enabled,
            super::conversation_memory::DEFAULT_CONTEXT_MAX_CHARS,
            super::conversation_memory::DEFAULT_RECENT_CHARS,
        )
    }

    pub fn new_with_context_limits(
        pool: SqlitePool,
        registry: SkillRegistry,
        knowledge: Arc<KnowledgeCoordinator>,
        web_enabled: bool,
        context_max_chars: usize,
        context_recent_chars: usize,
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
            registry: registry.clone(),
            slot_filler: SlotFiller::new(),
            thinking_flow: ThinkingFlowController::new(),
            material_search: MaterialSearchService::new(5),
            prompt_builder: PromptBuilder::new(),
            grounding_guard: GroundingGuard::new(),
            knowledge,
            web_enabled,
            branch_controller: BranchController::new(registry.clone()),
            context_max_chars,
            context_recent_chars,
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

        Self::start_phase(&context, "build_conversation_context").await?;
        let mut memory = ConversationMemory::build(
            &context,
            &recent_messages,
            &mut state,
            Some(turn.content.as_str()),
            self.context_max_chars,
            self.context_recent_chars,
        )
        .await;
        if let Some(summary) = memory.durable_summary.as_ref() {
            state.extra.insert(
                "conversation_memory_summary".to_owned(),
                Value::String(summary.clone()),
            );
        }
        Self::complete_phase(&context, "build_conversation_context").await?;

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
                "branch": branch_metadata_from_state(&state),
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

        let scope = evaluate_domain_boundary(&turn.content, false);
        if scope.redirect {
            Self::start_phase(&context, "domain_boundary").await?;
            let answer = scope.reply.as_deref().unwrap_or_default();
            let current_skill = state
                .extra
                .get("current_skill")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            state.extra.insert("awaiting_slots".to_owned(), json!([]));
            state.extra.remove("last_question");
            state.extra.insert(
                "latest_request".to_owned(),
                Value::String(turn.content.trim().to_owned()),
            );
            let route = compatibility_route(&state, current_skill.as_deref(), "general_response");
            let metadata = json!({
                "skill_id": current_skill,
                "selected_skill": null,
                "intent": "general_response",
                "answer_type": "general_response",
                "general": true,
                "general_response": true,
                "scope_redirected": true,
                "scope_reason": scope.reason,
                "used_corpus_files": [],
                "route_decision": route,
                "student_progress": compatibility_progress(&state, &route, current_skill.as_deref()),
                "used_web_search": false,
                "search_requested": false,
                "literature_search": idle_literature_search(),
                "web_search": idle_web_search(web),
                "guardrail_triggered": false,
                "guardrail": {"allowed": true, "violations": []},
                "branch": branch_metadata_from_state(&state),
            });
            self.messages
                .update_metadata(
                    user_message.id,
                    json!({
                        "run_id": context.run_id().to_legacy_hex(),
                        "skill_id": current_skill,
                        "intent": "general_message",
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
                    "domain_boundary",
                )
                .await?;
            return Ok(AgentAnswer::new(answer).with_metadata(metadata));
        }

        if turn.action == Some(TurnAction::Synthesize) {
            Self::start_phase(&context, "synthesize").await?;
            state.apply_user_message(&turn.content);
            let answer = answer_synthesis(&context, &memory, &state, &turn.content).await?;
            state
                .extra
                .insert("awaiting_slots".to_owned(), Value::Array(Vec::new()));
            state.extra.remove("last_question");
            let current_skill = state
                .extra
                .get("current_skill")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            state.extra.insert(
                "latest_request".to_owned(),
                Value::String(turn.content.trim().to_owned()),
            );
            state
                .extra
                .insert("last_synthesis".to_owned(), Value::String(answer.clone()));
            if current_skill.as_deref() == Some("socratic_review") {
                state.writing_context.thinking_stage = Some(FlowStage::SummaryReady);
            }
            let route = compatibility_route(&state, current_skill.as_deref(), "synthesize");
            let metadata = json!({
                "skill_id": current_skill,
                "selected_skill": current_skill,
                "action": "synthesize",
                "response_mode": "synthesize",
                "synthesis": true,
                "answer_type": "synthesis",
                "awaiting_slots": [],
                "used_corpus_files": [],
                "route_decision": route,
                "student_progress": compatibility_progress(&state, &route, current_skill.as_deref()),
                "used_web_search": false,
                "search_requested": false,
                "literature_search": idle_literature_search(),
                "web_search": idle_web_search(web),
                "guardrail_triggered": false,
                "guardrail": {"allowed": true, "violations": []},
                "branch": branch_metadata_from_state(&state),
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

        let router_text = conversation_router_text(&memory, &turn.content);
        let branch = self
            .branch_controller
            .resolve(&turn.content, &mut state, &router_text);
        if branch.needs_target {
            Self::start_phase(&context, "branch_switch_wait").await?;
            let answer = "可以。你直接说接下来想做什么，我会在这个对话里切换处理方式，前面的内容会继续保留。";
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
            let route = compatibility_route(&state, current_skill.as_deref(), "branch_switch");
            let metadata = json!({
                "skill_id": current_skill,
                "selected_skill": null,
                "used_corpus_files": [],
                "route_decision": route,
                "student_progress": compatibility_progress(&state, &route, current_skill.as_deref()),
                "general_response": true,
                "branch": branch.metadata(),
            });
            self.messages
                .update_metadata(
                    user_message.id,
                    json!({
                        "run_id": context.run_id().to_legacy_hex(),
                        "skill_id": current_skill,
                        "intent": "branch_switch",
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
                    "branch_switch_wait",
                )
                .await?;
            return Ok(AgentAnswer::new(answer).with_metadata(metadata));
        }
        let (routing_message, explicit_switch) = routing_message(&turn.content);
        if explicit_switch || branch.switched {
            state.extra.remove("awaiting_slots");
            state.extra.remove("collected_slots");
        }

        Self::start_phase(&context, "route_skill").await?;
        let route = self.route(&context, &turn.content, &state).await?;
        Self::complete_phase(&context, "route_skill").await?;

        if route.target_skill.is_none() {
            let fallback_scope = evaluate_domain_boundary(&turn.content, true);
            if fallback_scope.redirect {
                Self::start_phase(&context, "domain_boundary").await?;
                let answer = fallback_scope.reply.as_deref().unwrap_or_default();
                state.extra.insert("awaiting_slots".to_owned(), json!([]));
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
                    "skill_id": current_skill,
                    "selected_skill": null,
                    "intent": "general_response",
                    "answer_type": "general_response",
                    "general": true,
                    "general_response": true,
                    "scope_redirected": true,
                    "scope_reason": fallback_scope.reason,
                    "used_corpus_files": [],
                    "route_decision": route,
                    "student_progress": student_progress(&state, &route, current_skill.as_deref(), &[]),
                    "used_web_search": false,
                    "search_requested": false,
                    "literature_search": idle_literature_search(),
                    "web_search": idle_web_search(web),
                    "guardrail_triggered": false,
                    "guardrail": {"allowed": true, "violations": []},
                    "branch": branch.metadata(),
                });
                self.messages
                    .update_metadata(
                        user_message.id,
                        json!({
                            "run_id": context.run_id().to_legacy_hex(),
                            "skill_id": current_skill,
                            "intent": "general_message",
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
                        "domain_boundary",
                    )
                    .await?;
                return Ok(AgentAnswer::new(answer).with_metadata(metadata));
            }
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
                "branch": branch.metadata(),
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
                &branch,
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
        let session_document_query =
            build_session_document_query(routing_message, &state, &memory.recent_messages);
        let plan = knowledge_plan(
            skill,
            routing_message,
            &session_document_query,
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
                &branch,
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
                    knowledge: &knowledge,
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
            &branch,
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
    session_document_query: &str,
    web_enabled: bool,
    session_id: crate::domain::SessionId,
    material: Option<&crate::skills::MaterialSearchPlan>,
    decision: &KnowledgeDecision,
) -> KnowledgePlan {
    let mut tools = Vec::new();
    if decision.use_course_corpus {
        tools.push("course_corpus");
    }
    // Uploaded session evidence can inform every writing task, including Socratic topic
    // exploration. The tool remains session-scoped and returns no hits when no document matches.
    tools.push("session_documents");
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
            ("session_documents", _) => SearchRequest::new(session_document_query),
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

fn build_session_document_query(
    message: &str,
    state: &SessionStateData,
    recent_messages: &[crate::domain::Message],
) -> String {
    let writing = &state.writing_context;
    let mut parts = vec![message.trim().to_owned()];
    for value in [
        writing.topic.as_deref(),
        writing.research_question.as_deref(),
        writing.selected_direction.as_deref(),
        writing.suspected_mechanism.as_deref(),
        writing.selected_mechanism.as_deref(),
        writing.core_claim.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let value = value.trim();
        if !value.is_empty() && !parts.iter().any(|part| part == value) {
            parts.push(value.to_owned());
        }
    }
    let mut recent_user_messages = recent_messages
        .iter()
        .rev()
        .filter(|item| item.role == "user")
        .take(2)
        .collect::<Vec<_>>();
    recent_user_messages.reverse();
    for recent in recent_user_messages {
        let value = recent.content.trim();
        if !value.is_empty() && !parts.iter().any(|part| part == value) {
            parts.push(value.to_owned());
        }
    }
    parts.join("\n")
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
    branch: &BranchResolution,
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
    let session_documents =
        filtered_sources(&knowledge.hits, |provider| provider == "session_document");
    let session_document_failed = knowledge
        .metadata
        .provider_failures
        .iter()
        .any(|failure| failure.provider == "session_documents");
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
        "session_document_status": if session_document_failed {
            "failed"
        } else if session_documents.is_empty() {
            "no_hits"
        } else {
            "used"
        },
        "session_document_sources": session_documents,
        "provider_failures": knowledge.metadata.provider_failures,
        "guardrail_triggered": guardrail_triggered,
        "branch": branch.metadata(),
    })
}

fn branch_metadata_from_state(state: &SessionStateData) -> Value {
    let control = state.extra.get("branch_control").and_then(Value::as_object);
    let active_skill = control
        .and_then(|value| value.get("active_skill"))
        .cloned()
        .unwrap_or_else(|| {
            state
                .extra
                .get("current_skill")
                .cloned()
                .unwrap_or(Value::Null)
        });
    let mode = control
        .and_then(|value| value.get("mode"))
        .and_then(Value::as_str)
        .unwrap_or(if active_skill.is_null() {
            "unresolved"
        } else {
            "locked"
        });
    json!({
        "mode": mode,
        "active_skill": active_skill,
        "needs_target": mode == "awaiting_switch",
        "switched": false,
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
        "heading": hit.heading,
        "provider": hit.provider,
        "url": hit.url,
        "doi": hit.doi,
        "document_id": hit.metadata.get("document_id").cloned().unwrap_or(Value::Null),
        "chunk_id": hit.metadata.get("chunk_id").cloned().unwrap_or(Value::Null),
        "chunk_index": hit.metadata.get("chunk_index").cloned().unwrap_or(Value::Null),
    })
}

fn contains_any(text: &str, patterns: &[&str]) -> bool {
    let lowered = text.to_lowercase();
    patterns
        .iter()
        .any(|pattern| lowered.contains(&pattern.to_lowercase()))
}

fn is_direct_delivery_request(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    contains_any(
        &message,
        &[
            "直接写",
            "替我写",
            "完整范文",
            "生成全文",
            "写完整",
            "直接提交",
            "交作业",
        ],
    ) || (message.contains("帮我写")
        && contains_any(&message, &["作文", "论文", "报告", "作业", "正文", "段落"]))
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

fn conversation_router_text(memory: &ConversationMemory, current_message: &str) -> String {
    let mut lines = Vec::new();
    if let Some(summary) = memory.durable_summary.as_deref() {
        lines.push(format!("早期摘要：{summary}"));
    }
    lines.extend(
        memory
            .recent_messages
            .iter()
            .filter_map(|message| match message.role.as_str() {
                "user" => Some(format!("用户：{}", message.content.trim())),
                "assistant" => Some(format!("助手：{}", message.content.trim())),
                _ => None,
            }),
    );
    lines.push(format!("用户：{}", current_message.trim()));
    lines.join("\n")
}

async fn answer_synthesis(
    context: &RunContext,
    memory: &ConversationMemory,
    state: &SessionStateData,
    current_message: &str,
) -> Result<String, AppError> {
    let fallback = fallback_synthesis(memory, state);
    let mut transcript = Vec::new();
    if let Some(summary) = memory.durable_summary.as_deref() {
        transcript.push(format!("早期摘要：{summary}"));
    }
    transcript.extend(memory.recent_messages.iter().filter_map(
        |message| match message.role.as_str() {
            "user" => Some(format!("用户：{}", message.content.trim())),
            "assistant" => Some(format!("助教：{}", message.content.trim())),
            _ => None,
        },
    ));
    transcript.push(format!("用户：{}", current_message.trim()));
    let writing_context =
        serde_json::to_string_pretty(&state.writing_context).unwrap_or_else(|_| "{}".to_owned());
    let request = ModelRequest {
        messages: vec![
            ModelMessage::system(
                "你是写作与沟通课程助教。本轮是收束动作，不是继续追问。必须根据同一会话已有内容直接形成一份完整、可修改的写作思路。不得继续追问，不得要求学生再补信息，不得生成可直接提交的完整文章。信息不足处用‘暂定’说明并给出最佳可行方案。只输出 JSON。",
            ),
            ModelMessage::user(format!(
                "[同一会话完整上下文]\n{}\n\n[结构化写作状态]\n{}\n\n请输出以下 JSON 字段：\n{{\n  \"topic_positioning\": \"一句话说明选题对象、现象和边界\",\n  \"core_problem\": \"把研究问题写成解释任务，不向学生提问\",\n  \"working_thesis\": \"当前可成立的核心判断；不足时标记暂定\",\n  \"concept_path\": [\"需要界定或连接的概念及其作用\"],\n  \"article_structure\": [\"第一部分做什么\", \"第二部分做什么\", \"第三部分做什么\"],\n  \"materials\": [\"可使用的理论、案例或材料类型\"],\n  \"next_step\": \"一个可以立即执行的动作，不使用问句\"\n}}",
                transcript.join("\n"),
                writing_context
            )),
        ],
        temperature: Some(0.2),
    };
    match context
        .call_model("synthesize_writing_context", request)
        .await
    {
        Ok(response) => Ok(parse_synthesis(&response.content)
            .map(|payload| render_synthesis(&payload))
            .unwrap_or(fallback)),
        Err(AppError::Model(_)) => Ok(fallback),
        Err(error) => Err(error),
    }
}

fn fallback_synthesis(memory: &ConversationMemory, state: &SessionStateData) -> String {
    let writing = &state.writing_context;
    let topic = writing
        .topic
        .as_deref()
        .or(writing.initial_idea.as_deref())
        .or_else(|| {
            memory
                .recent_messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| message.content.as_str())
        })
        .unwrap_or("当前讨论的写作主题");
    let selected_path = writing
        .selected_path
        .as_deref()
        .or_else(|| {
            writing
                .extra
                .get("selected_direction")
                .and_then(Value::as_str)
        })
        .unwrap_or("现象、形成机制与适用边界");
    let theory_entry = writing
        .extra
        .get("theory_entry")
        .and_then(Value::as_str)
        .unwrap_or("关键概念界定与机制解释");
    render_synthesis(&json!({
        "topic_positioning": format!("暂定围绕“{topic}”展开，聚焦可被解释的具体对象和现象。"),
        "core_problem": format!("解释这一现象如何形成，并沿“{selected_path}”明确其影响与边界。"),
        "working_thesis": "暂定判断是：该现象并非单一的个人选择，而是情境压力、关系期待和交往机制共同作用的结果。",
        "concept_path": [
            format!("以“{theory_entry}”作为主要概念入口。"),
            "区分现象描述、原因解释和价值判断，避免把网络热词直接当作结论。"
        ],
        "article_structure": [
            "第一部分界定现象与核心概念，说明文章具体讨论什么。",
            "第二部分分析形成机制，用理论和材料建立因果链。",
            "第三部分讨论反例、条件边界与可能影响，收束核心判断。"
        ],
        "materials": [
            "课程理论或学术概念负责解释机制。",
            "典型案例、访谈或平台文本负责呈现现象。",
            "反例或对照情境负责检验判断的适用范围。"
        ],
        "next_step": "先写出核心判断，再为三部分各整理两条能够支撑它的材料。"
    }))
}

fn parse_synthesis(content: &str) -> Option<Value> {
    let trimmed = content.trim();
    let unwrapped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let json_text = unwrapped.strip_suffix("```").unwrap_or(unwrapped).trim();
    let payload: Value = serde_json::from_str(json_text).ok()?;
    payload
        .get("topic_positioning")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    Some(payload)
}

fn render_synthesis(payload: &Value) -> String {
    fn clean(value: Option<&Value>, fallback: &str) -> String {
        value
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(fallback)
            .trim()
            .replace(['？', '?'], "。")
    }
    fn bullets(value: Option<&Value>, fallback: &[&str]) -> String {
        let items = value
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|item| !item.trim().is_empty())
                    .collect::<Vec<_>>()
            })
            .filter(|items| !items.is_empty())
            .unwrap_or_else(|| fallback.to_vec());
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                format!("{}. {}", index + 1, item.trim().replace(['？', '?'], "。"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    format!(
        "## 选题定位\n{}\n\n## 核心问题\n{}\n\n## 核心判断\n{}\n\n## 概念路径\n{}\n\n## 文章结构\n{}\n\n## 可用材料\n{}\n\n## 下一步\n{}",
        clean(
            payload.get("topic_positioning"),
            "暂定围绕当前讨论的现象展开。"
        ),
        clean(
            payload.get("core_problem"),
            "解释这一现象的形成机制与影响边界。"
        ),
        clean(
            payload.get("working_thesis"),
            "暂定判断仍需用材料进一步检验。"
        ),
        bullets(
            payload.get("concept_path"),
            &["界定核心概念并说明它们之间的关系。"]
        ),
        bullets(
            payload.get("article_structure"),
            &["界定现象。", "解释机制。", "讨论边界。"]
        ),
        bullets(payload.get("materials"), &["理论材料、典型案例和反例。"]),
        clean(
            payload.get("next_step"),
            "先写出核心判断，再为每一部分配置材料。"
        ),
    )
}

fn corrupt_json(error: serde_json::Error) -> AppError {
    AppError::CorruptData(format!(
        "could not serialize canonical writing state: {error}"
    ))
}
