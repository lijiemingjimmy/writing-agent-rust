use std::{collections::BTreeSet, sync::Arc};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::SqlitePool;

use crate::{
    AppError,
    agent::{AgentAnswer, AgentProgram, RunContext, UserTurn},
    corpus::{markdown::MarkdownKnowledgeTool, session_documents::SessionDocumentKnowledgeTool},
    domain::{FlowStage, RiskLevel, RouteDecision, RouteInput, SessionStateData, WritingStage},
    llm::{ModelMessage, ModelRequest},
    skills::{
        GroundingGuard, GuardPolicy, PromptBuilder, PromptContext, SkillDefinition, SkillRegistry,
        SkillRouter, ThinkingFlowController,
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

const STRUCTURED_ROUTE_WARNING: &str =
    "structured route decision was invalid; deterministic fallback used";

pub struct WritingCoachProgram {
    sessions: SessionRepository,
    messages: MessageRepository,
    registry: SkillRegistry,
    router: SkillRouter,
    thinking_flow: ThinkingFlowController,
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
            thinking_flow: ThinkingFlowController::new(),
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
        context: &RunContext,
        message: &str,
        state: &SessionStateData,
    ) -> Result<RouteDecision, AppError> {
        let route_input = route_input(message, state);
        let deterministic = self.router.route(&route_input);
        if deterministic.target_skill.is_some() {
            return Ok(deterministic);
        }

        let request = ModelRequest {
            messages: vec![
                ModelMessage::system(
                    "Choose one installed writing-coach skill. Return JSON only with target_skill, confidence, and reason. Do not answer the user.",
                ),
                ModelMessage::user(format!(
                    "installed_skills={}\nlatest_user_message={}",
                    self.registry
                        .all()
                        .map(|skill| skill.id.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    message.trim()
                )),
            ],
            temperature: Some(0.0),
        };
        let raw = context.call_model("route_decision", request).await?;
        let parsed = serde_json::from_str::<StructuredRoute>(&raw.content)
            .ok()
            .filter(|decision| {
                decision.confidence.is_finite()
                    && (0.0..=1.0).contains(&decision.confidence)
                    && !decision.reason.trim().is_empty()
                    && self.registry.get(&decision.target_skill).is_some()
            });
        let Some(parsed) = parsed else {
            context
                .emit(
                    "decision.warning",
                    json!({"message": STRUCTURED_ROUTE_WARNING}),
                )
                .await?;
            return Ok(deterministic_fallback(message, state));
        };

        let mut decision = deterministic_fallback(message, state);
        decision.stage = stage_for_skill(&parsed.target_skill);
        decision.target_skill = Some(parsed.target_skill);
        decision.confidence = parsed.confidence;
        decision.reason = truncate_chars(parsed.reason.trim(), 240);
        Ok(decision)
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
        let mut recent_messages = self.messages.list_by_session(turn.session_id).await?;
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

        if is_general_turn(&turn.content, &state) {
            Self::start_phase(&context, "general_response").await?;
            let answer = general_response(&turn.content);
            let current_skill = state
                .extra
                .get("current_skill")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let route = compatibility_route(&state, current_skill, "general_message");
            let metadata = json!({
                "selected_skill": null,
                "skill_id": current_skill,
                "intent": "general_response",
                "answer_type": "general_response",
                "general": true,
                "general_response": true,
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
                        "intent": "general_message"
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
                    "general_response",
                )
                .await?;
            return Ok(AgentAnswer::new(answer).with_metadata(metadata));
        }

        let context_update = state.apply_user_message(&turn.content);
        if context_update.topic_changed {
            recent_messages.clear();
        }

        Self::start_phase(&context, "route_skill").await?;
        let route = self.route(&context, &turn.content, &state).await?;
        let skill_id = route
            .target_skill
            .as_deref()
            .unwrap_or("socratic_review")
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
        Self::complete_phase(&context, "route_skill").await?;

        Self::start_phase(&context, "update_writing_context").await?;
        let thinking_stage = if skill.id == "socratic_review" {
            let flow = self
                .thinking_flow
                .advance(&state.writing_context, &turn.content);
            state.writing_context.candidate_paths = flow.candidate_paths.clone();
            state.writing_context.thinking_stage = Some(flow.stage.clone());
            state.writing_context.flow_stage = Some(flow.stage.clone());
            state.writing_context.ready_for_refined_advice = flow.ready_for_summary;
            Some(flow.stage)
        } else {
            None
        };
        let revision_comparison = update_revision_state(&mut state, &turn.content);
        // The validated route owns the final stage for this turn. Context extraction may infer a
        // provisional stage, so apply it first and record the route last.
        record_route(&mut state, &route);
        Self::complete_phase(&context, "update_writing_context").await?;

        Self::start_phase(&context, "fill_required_slots").await?;
        let mut collected = collected_slots(&state);
        let awaited = awaiting_slots(&state);
        fill_slots(skill, &turn.content, &awaited, &mut collected);
        if state.extra.get("latest_draft").is_none()
            && let Some(draft) = collected.get("draft_text").and_then(Value::as_str)
        {
            state
                .extra
                .insert("latest_draft".to_owned(), Value::String(draft.to_owned()));
        }
        let missing = skill
            .required_slots
            .iter()
            .filter(|slot| !slot_has_value(&collected, slot))
            .cloned()
            .collect::<Vec<_>>();
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

        if let Some(slot) = missing.first() {
            let question = skill
                .slot_questions
                .as_ref()
                .and_then(|questions| questions.get(slot))
                .cloned()
                .unwrap_or_else(|| "请先补充当前任务最缺的信息。".to_owned());
            Self::start_phase(&context, "persist_answer").await?;
            let metadata = answer_metadata(
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
        let plan = knowledge_plan(skill, &turn.content, web_enabled, turn.session_id);
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

        Self::start_phase(&context, "build_prompt").await?;
        let prompt = self.prompt_builder.build(PromptContext {
            policies: self.registry.policies(),
            skill,
            state: &state,
            recent_messages: &recent_messages,
            knowledge: &knowledge,
            user_message: &turn.content,
            web_enabled,
        });
        Self::complete_phase(&context, "build_prompt").await?;

        Self::start_phase(&context, "call_model").await?;
        let response = context
            .call_model(
                "writing_coach_answer",
                ModelRequest {
                    messages: prompt,
                    temperature: Some(0.2),
                },
            )
            .await?;
        Self::complete_phase(&context, "call_model").await?;

        Self::start_phase(&context, "validate_grounding_and_guardrail").await?;
        let user_texts = recent_messages
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
        let mut metadata = answer_metadata(
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

#[derive(Deserialize)]
struct StructuredRoute {
    target_skill: String,
    confidence: f32,
    reason: String,
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

fn deterministic_fallback(message: &str, state: &SessionStateData) -> RouteDecision {
    let stage = if state.writing_context.stage.is_unknown() {
        WritingStage::Topic
    } else {
        state.writing_context.stage.clone()
    };
    RouteDecision {
        stage,
        intent: "clarify".to_owned(),
        risk: RiskLevel::NeedsSocratic,
        target_skill: Some("socratic_review".to_owned()),
        confidence: 0.55,
        reason: format!(
            "规则无法确定唯一任务，先用写作追问澄清：{}",
            truncate_chars(message.trim(), 48)
        ),
        required_context: vec!["motivation".to_owned()],
        needs_socratic: true,
    }
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

fn stage_for_skill(skill: &str) -> WritingStage {
    match skill {
        "socratic_review" | "novelty_eval" => WritingStage::Topic,
        "material_search" | "literature_reading" => WritingStage::Literature,
        "research_question_evaluator" => WritingStage::ResearchQuestion,
        "theory_fit_checker" => WritingStage::Theory,
        "method_feasibility_checker" => WritingStage::Method,
        "draft_diagnosis" | "writing_feedback" => WritingStage::DraftArgument,
        "course_policy_qa" | "ai_use_boundary_qa" => WritingStage::CoursePolicy,
        "academic_norm_check" => WritingStage::AcademicNorm,
        _ => WritingStage::Unknown("unknown".to_owned()),
    }
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

fn collected_slots(state: &SessionStateData) -> Map<String, Value> {
    state
        .extra
        .get("collected_slots")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn fill_slots(
    skill: &SkillDefinition,
    message: &str,
    awaited: &[String],
    slots: &mut Map<String, Value>,
) {
    let text = message.trim();
    if let Some(slot) = awaited.first()
        && !slot_has_value(slots, slot)
        && let Some(value) = extract_slot_value(slot, text, true)
    {
        slots.insert(slot.clone(), Value::String(value));
    }
    for slot in &skill.required_slots {
        if slot_has_value(slots, slot) {
            continue;
        }
        let value = extract_slot_value(slot, text, false);
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            slots.insert(slot.clone(), Value::String(value));
        }
    }
}

fn extract_slot_value(slot: &str, text: &str, awaited: bool) -> Option<String> {
    match slot {
        "question" => Some(text.to_owned()),
        "target_text" if text.chars().count() >= 12 => Some(text.to_owned()),
        "source_text" if text.chars().count() >= 30 => Some(text.to_owned()),
        "thinking_task"
            if contains_any(
                text,
                &["选题", "主题", "想写", "研究", "方向", "搭子", "朋友"],
            ) =>
        {
            Some("选题".to_owned())
        }
        "thinking_task" if contains_any(text, &["论证", "结构", "论点"]) => {
            Some("论证结构".to_owned())
        }
        "thinking_task" if contains_any(text, &["文献", "综述", "材料"]) => {
            Some("文献综述".to_owned())
        }
        "thinking_task" if contains_any(text, &["修改", "修订", "调整"]) => {
            Some("修改方案".to_owned())
        }
        "initial_idea" if text.chars().count() >= 8 => Some(text.to_owned()),
        "assignment_requirement" => after_label(text, &["作业要求", "要求", "题目"])
            .or_else(|| {
                (contains_any(text, &["字", "不少于", "不超过", "字数"])).then(|| text.to_owned())
            })
            .or_else(|| awaited.then(|| text.to_owned())),
        "draft_text" => after_label(text, &["全文", "文章", "初稿", "原文", "草稿"])
            .or_else(|| (awaited && text.chars().count() >= 20).then(|| text.to_owned())),
        "core_argument" => after_label(text, &["核心观点", "中心论点", "主旨", "论点"])
            .or_else(|| awaited.then(|| text.to_owned())),
        "feedback_goal" => after_label(text, &["目标", "希望", "重点看"])
            .or_else(|| awaited.then(|| text.to_owned())),
        "review_goal" => after_label(text, &["互评目标", "目标", "希望"])
            .or_else(|| awaited.then(|| text.to_owned())),
        _ => None,
    }
}

fn slot_has_value(slots: &Map<String, Value>, slot: &str) -> bool {
    slots.get(slot).is_some_and(|value| match value {
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Null => false,
        _ => true,
    })
}

fn after_label(text: &str, labels: &[&str]) -> Option<String> {
    labels.iter().find_map(|label| {
        let (_, rest) = text.split_once(label)?;
        let value = rest
            .trim_start_matches(['：', ':', '是', '为', ' '])
            .split(['\n', '。'])
            .next()
            .unwrap_or_default()
            .trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn knowledge_plan(
    skill: &SkillDefinition,
    message: &str,
    web_enabled: bool,
    session_id: crate::domain::SessionId,
) -> KnowledgePlan {
    let mut tools = Vec::new();
    let course = matches!(
        skill.id.as_str(),
        "ppt_qa"
            | "material_search"
            | "novelty_eval"
            | "research_question_evaluator"
            | "theory_fit_checker"
            | "method_feasibility_checker"
            | "course_policy_qa"
            | "academic_norm_check"
            | "draft_diagnosis"
            | "writing_feedback"
    );
    if course {
        tools.push("course_corpus");
    }
    if matches!(skill.id.as_str(), "draft_diagnosis" | "writing_feedback") {
        tools.push("session_documents");
    }
    if skill.id == "material_search" && web_enabled {
        tools.extend(["scholarly", "web"]);
    }
    let years = extract_year_range(message);
    KnowledgePlan::new(tools.into_iter().map(|tool| {
        PlannedSearch::new(
            tool,
            SearchRequest::new(message)
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
    let course_requested = matches!(
        skill.id.as_str(),
        "ppt_qa"
            | "material_search"
            | "novelty_eval"
            | "research_question_evaluator"
            | "theory_fit_checker"
            | "method_feasibility_checker"
            | "course_policy_qa"
            | "academic_norm_check"
            | "draft_diagnosis"
            | "writing_feedback"
    );
    let external_requested = web_state.requested && skill.id == "material_search";
    let web_enabled = web_state.enabled() && skill.id == "material_search";
    let web_attempted = web_enabled && !slot_question;
    let web_error = if skill.id != "material_search" || !web_state.requested {
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
        "student_progress": {
            "stage": route.stage.as_str(),
            "current_skill": skill.id,
            "awaiting_slots": missing,
        },
        "search_requested": !slot_question && (course_requested || web_attempted),
        "search_has_hits": !knowledge.hits.is_empty(),
        "external_search_requested": external_requested,
        "web_search_requested": web_state.requested,
        "used_web_search": !web.is_empty(),
        "knowledge_use": {
            "use_course_corpus": course_requested,
            "course_hit_count": used_corpus.len(),
            "use_external_search": web_attempted,
            "decider": "deterministic",
        },
        "literature_search": {
            "enabled": web_attempted,
            "triggered": skill.id == "material_search",
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
            "triggered": skill.id == "material_search",
            "error": web_error,
            "results": web,
        },
        "grounding_sources": knowledge.hits.iter().map(source_value).collect::<Vec<_>>(),
        "provider_failures": knowledge.metadata.provider_failures,
        "guardrail_triggered": guardrail_triggered,
    })
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
        "reset"
            | "/reset"
            | "exit"
            | "quit"
            | "退出"
            | "结束"
            | "停止"
            | "重置"
            | "清空"
            | "重新开始"
            | "换个任务"
    )
}

fn is_general_turn(message: &str, _state: &SessionStateData) -> bool {
    let text = message.trim().to_ascii_lowercase();
    matches!(
        text.as_str(),
        "你好"
            | "你好！"
            | "hi"
            | "hello"
            | "嗯"
            | "嗯嗯"
            | "哦"
            | "好的"
            | "谢谢"
            | "你不能直接回答吗"
            | "你不能直接回答吗？"
    ) || contains_any(&text, &["天气", "电影", "游戏", "吃什么", "笑话"])
}

fn general_response(message: &str) -> &'static str {
    if contains_any(message, &["天气", "电影", "游戏", "吃什么", "笑话"]) {
        "我主要帮助你梳理写作任务、选题、材料、论证和修改。你可以把当前写作问题发给我。"
    } else {
        "你好，我是写作学伴。你可以告诉我作业要求、想法或卡住的地方。"
    }
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn corrupt_json(error: serde_json::Error) -> AppError {
    AppError::CorruptData(format!(
        "could not serialize canonical writing state: {error}"
    ))
}
