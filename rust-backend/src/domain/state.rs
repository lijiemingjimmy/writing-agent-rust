use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::{AppError, domain::WritingStage};

const TOPIC_LEAD_PREFIXES: &[&str] = &["我想", "想要", "想", "准备", "打算", "要"];
const TOPIC_ACTION_PREFIXES: &[&str] = &["写一篇", "写一个", "写个", "写", "研究", "讨论", "分析"];
const NEW_TOPIC_PREFIXES: &[&str] = &["我想换成", "想换成", "换成", "我想改成", "改成"];
const SHORT_TOPIC_SIGNAL_MARKERS: &[&str] = &["搭子", "朋友", "小组合作", "小组分工", "分工"];
const FOLLOWUP_TOPIC_BLOCKERS: &[&str] = &[
    "因为",
    "不是",
    "当然是",
    "就是",
    "比如",
    "我发现",
    "观察到",
    "不干活",
    "拖了进度",
    "不好意思",
    "抹不下脸",
    "碍于面子",
    "替人干活",
    "默认选项",
];
const MOTIVATION_MARKERS: &[&str] = &["主要是因为", "是因为", "因为"];
const WEAK_WRITABILITY_MOTIVATION_MARKERS: &[&str] = &["好写", "材料容易找"];
const SCENE_MARKERS: &[&str] = &[
    "某次",
    "课程作业",
    "大作业",
    "小组作业",
    "社团项目",
    "身边同学",
    "真实经历",
    "我经历",
    "拖了进度",
    "不干活",
    "替他",
    "替人",
    "分工不均",
];
const EVIDENCE_MARKERS: &[&str] = &[
    "证据",
    "例子",
    "案例",
    "材料",
    "访谈",
    "数据",
    "经历",
    "课程作业",
    "大作业",
    "小组作业",
    "不干活",
    "拖了进度",
    "替他",
    "替人",
];
const EVIDENCE_REJECTION_MARKERS: &[&str] = &[
    "别给我文献",
    "不要文献",
    "不用文献",
    "谁让你给我文献",
    "不是让你给文献",
    "不是让你找文献",
    "不是让你搜文献",
];
const CLAIM_MARKERS: &[&str] = &[
    "核心论点是",
    "核心观点是",
    "我的观点是",
    "我认为",
    "我想证明",
    "论点是",
];
const CHOICE_REASON_MARKERS: &[&str] = &["因为", "我选", "选择", "没选", "不选", "更适合", "感受"];
const COUNTEREXAMPLE_MARKERS: &[&str] = &["反例", "反方", "反驳", "相反", "不一定", "但是也可能"];
const NEGATED_EVIDENCE_MARKERS: &[&str] = &[
    "不成立",
    "没有证据",
    "没证据",
    "无法证明",
    "不能证明",
    "不是证据",
    "并非证据",
    "没有例子",
    "没有案例",
    "没有材料",
    "没有访谈",
    "没有数据",
];
const EMBEDDED_CHOICE_TOKENS: &[(&str, &str)] = &[
    ("第一个", "1"),
    ("第1个", "1"),
    ("1号方向", "1"),
    ("方向一", "1"),
    ("第二个", "2"),
    ("第2个", "2"),
    ("2号方向", "2"),
    ("方向二", "2"),
    ("第三个", "3"),
    ("第3个", "3"),
    ("3号方向", "3"),
    ("方向三", "3"),
    ("第四个", "4"),
    ("第4个", "4"),
    ("4号方向", "4"),
    ("方向四", "4"),
    ("第五个", "5"),
    ("第5个", "5"),
    ("5号方向", "5"),
    ("方向五", "5"),
    ("第六个", "6"),
    ("第6个", "6"),
    ("6号方向", "6"),
    ("方向六", "6"),
];
const EMBEDDED_CHOICE_INTENT_PREFIXES: &[&str] = &[
    "我觉得",
    "觉得",
    "我认为",
    "认为",
    "我想试试",
    "想试试",
    "试试",
    "我想选",
    "想选",
    "我选择",
    "我选",
    "选择",
    "就选",
    "要选",
    "更喜欢",
    "倾向于",
    "倾向",
];
const EMBEDDED_CHOICE_SUFFIXES: &[&str] = &[
    "方向",
    "更好",
    "更合适",
    "更适合",
    "比较好",
    "可以",
    "就行",
    "吧",
    "了",
];

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CandidatePath {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub index: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub core_question: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub material_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub risk: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlowStage {
    MotivationProbe,
    CandidatePaths,
    ChoiceReflection,
    EvidenceCheck,
    RefinedAdvice,
    SummaryReady,
    Unknown(String),
}

impl FlowStage {
    pub fn as_str(&self) -> &str {
        match self {
            Self::MotivationProbe => "motivation_probe",
            Self::CandidatePaths => "candidate_paths",
            Self::ChoiceReflection => "choice_reflection",
            Self::EvidenceCheck => "evidence_check",
            Self::RefinedAdvice => "refined_advice",
            Self::SummaryReady => "summary_ready",
            Self::Unknown(value) => value,
        }
    }
}

impl From<&str> for FlowStage {
    fn from(value: &str) -> Self {
        match value {
            "motivation_probe" => Self::MotivationProbe,
            "candidate_paths" => Self::CandidatePaths,
            "choice_reflection" => Self::ChoiceReflection,
            "evidence_check" => Self::EvidenceCheck,
            "refined_advice" => Self::RefinedAdvice,
            "summary_ready" => Self::SummaryReady,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

impl Serialize for FlowStage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FlowStage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(|value| Self::from(value.as_str()))
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct WritingContext {
    #[serde(default, deserialize_with = "deserialize_stage_or_default")]
    pub stage: WritingStage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_direction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_question: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_idea: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motivation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_scene: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confusion_point: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspected_mechanism: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_mechanism: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_claim: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_path_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_path_detail: Option<CandidatePath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_stage: Option<FlowStage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flow_stage: Option<FlowStage>,
    #[serde(
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub candidate_paths: Vec<CandidatePath>,
    #[serde(
        rename = "evidence_items",
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub evidence: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub counterexamples: Vec<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub counterarguments: Vec<String>,
    #[serde(default)]
    pub ready_for_refined_advice: bool,
    #[serde(default)]
    pub socratic_rounds: u32,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
    #[serde(skip)]
    stage_present: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SessionStateData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(default, skip_serializing_if = "WritingContext::is_empty")]
    pub writing_context: WritingContext,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ContextUpdate {
    pub topic_changed: bool,
    pub motivation_captured: bool,
    pub scene_captured: bool,
    pub claim_captured: bool,
    pub evidence_added: bool,
    pub selected_path_id: Option<String>,
}

impl SessionStateData {
    pub fn from_legacy_json(value: Value) -> Result<Self, AppError> {
        let mut object = value.as_object().cloned().ok_or_else(|| {
            AppError::CorruptData("legacy session state must be a JSON object".to_owned())
        })?;
        let writing_context = WritingContext::from_legacy_json(
            object
                .remove("writing_context")
                .unwrap_or_else(|| Value::Object(Map::new())),
        )?;
        let task_type = match object.remove("task_type") {
            Some(Value::String(value)) => Some(value),
            Some(Value::Null) | None => None,
            Some(value) => {
                object.insert("task_type".to_owned(), value);
                None
            }
        };
        Ok(Self {
            task_type,
            writing_context,
            extra: object,
        })
    }

    pub fn apply_user_message(&mut self, message: &str) -> ContextUpdate {
        let update = self.writing_context.apply_user_message(message);
        if update.topic_changed {
            self.task_type = None;
            for key in [
                "current_skill",
                "awaiting_slots",
                "collected_slots",
                "latest_draft",
                "revision_history",
            ] {
                self.extra.remove(key);
            }
        }
        update
    }

    pub fn update_from_user(
        &mut self,
        message: &str,
        skill_id: Option<&str>,
        collected: &Map<String, Value>,
    ) -> ContextUpdate {
        let previous_rounds = self.writing_context.socratic_rounds;
        let previous_topic = self.writing_context.topic.clone();
        let update = self.apply_user_message(message);
        if skill_id != Some("socratic_review") {
            self.writing_context.socratic_rounds = previous_rounds;
        }

        let topic = self.writing_context.topic.clone();
        self.writing_context.extra.insert(
            "latest_turn".to_owned(),
            serde_json::json!({
                "text": truncate_chars(message.trim(), 240),
                "topic": topic,
                "is_self_contained": topic.is_some() && extract_topic(message).is_some_and(|(_, explicit)| explicit),
                "starts_new_topic": update.topic_changed,
            }),
        );
        if skill_id == Some("socratic_review") {
            if let Some(task) = collected.get("thinking_task").and_then(Value::as_str) {
                self.writing_context
                    .extra
                    .insert("thinking_task".to_owned(), Value::String(task.to_owned()));
            }
            if self.writing_context.initial_idea.is_none()
                && let Some(idea) = collected.get("initial_idea").and_then(Value::as_str)
            {
                self.writing_context.initial_idea = Some(truncate_chars(idea, 240));
            }
        }
        if skill_id == Some("novelty_eval") {
            let keywords = extract_search_keywords(message, &self.writing_context);
            if !keywords.is_empty() && !is_literature_lookup_message(message) {
                self.writing_context.extra.insert(
                    "search_keywords".to_owned(),
                    Value::Array(keywords.into_iter().map(Value::String).collect()),
                );
            }
            if contains_any(message, &["文献", "理论", "材料", "论据", "资料"]) {
                self.writing_context.extra.insert(
                    "material_gap".to_owned(),
                    Value::String("需要可核验的文献、理论或案例材料".to_owned()),
                );
            }
        }
        if update.topic_changed && previous_topic != self.writing_context.topic {
            self.writing_context
                .extra
                .insert("thinking_task".to_owned(), Value::String("选题".to_owned()));
        }
        self.writing_context.context_summary = self.writing_context.build_context_summary();
        update
    }

    pub fn update_after_reply(&mut self, reply: &str, skill_id: Option<&str>) {
        if matches!(skill_id, Some("novelty_eval" | "socratic_review")) {
            let options = extract_numbered_options(reply);
            if !options.is_empty()
                && reply_contains_thinking_options(reply)
                && (skill_id == Some("novelty_eval")
                    || (self.writing_context.candidate_paths.is_empty()
                        && matches!(
                            self.writing_context.thinking_stage,
                            Some(FlowStage::CandidatePaths | FlowStage::Unknown(_))
                        )))
            {
                self.writing_context.candidate_paths = options;
            }
        }
        if skill_id == Some("socratic_review") {
            let mut questions = extract_questions(reply);
            if self.writing_context.thinking_stage == Some(FlowStage::EvidenceCheck)
                && reply.contains("过程证据")
            {
                questions = vec!["补一个过程证据和一个反例，检验这个题目能不能站住。".to_owned()];
            }
            if self.writing_context.thinking_stage == Some(FlowStage::CandidatePaths) {
                self.writing_context
                    .extra
                    .insert("unanswered_questions".to_owned(), Value::Array(Vec::new()));
            } else if !questions.is_empty() {
                let questions = questions.into_iter().take(3).collect::<Vec<_>>();
                let history = self
                    .writing_context
                    .extra
                    .entry("socratic_questions".to_owned())
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(history) = history.as_array_mut() {
                    for question in &questions {
                        if !history.iter().any(|item| item.as_str() == Some(question)) {
                            history.push(Value::String(question.clone()));
                        }
                    }
                    if history.len() > 10 {
                        history.drain(..history.len() - 10);
                    }
                }
                self.writing_context.extra.insert(
                    "unanswered_questions".to_owned(),
                    Value::Array(questions.into_iter().map(Value::String).collect()),
                );
            }
            if reply.contains("面批前摘要") {
                self.writing_context.extra.insert(
                    "pre_conference_summary".to_owned(),
                    Value::String(reply.to_owned()),
                );
            }
        }
        self.writing_context.context_summary = self.writing_context.build_context_summary();
    }
}

impl WritingContext {
    pub fn from_legacy_json(value: Value) -> Result<Self, AppError> {
        if value.is_null() {
            return Ok(Self::default());
        }
        if !value.is_object() {
            return Err(AppError::CorruptData(
                "legacy writing_context must be a JSON object or null".to_owned(),
            ));
        }
        let stage_present = value.get("stage").and_then(Value::as_str).is_some();
        let mut context: Self = serde_json::from_value(value).map_err(|error| {
            AppError::CorruptData(format!("could not parse legacy writing_context: {error}"))
        })?;
        context.stage_present = stage_present;
        Ok(context)
    }

    pub fn apply_user_message(&mut self, message: &str) -> ContextUpdate {
        let text = message.trim();
        if text.is_empty() {
            return ContextUpdate::default();
        }

        self.socratic_rounds = self.socratic_rounds.saturating_add(1);
        let mut update = ContextUpdate::default();

        if let Some((topic, explicit)) = extract_topic(text) {
            let existing = self.topic.clone().unwrap_or_default();
            let changed = !existing.is_empty() && explicit && !same_topic_family(&existing, &topic);
            if changed {
                self.clear_topic_dependents();
                update.topic_changed = true;
            }
            if existing.is_empty() || changed || same_topic_family(&existing, &topic) {
                self.topic = Some(topic.clone());
                self.stage = WritingStage::Topic;
                self.stage_present = true;
                if self.initial_idea.is_none() || changed {
                    self.initial_idea = Some(truncate_chars(text, 240));
                }
                if changed {
                    self.extra
                        .insert("thinking_task".to_owned(), Value::String("选题".to_owned()));
                }
            }
        }

        if contains_any(text, WEAK_WRITABILITY_MOTIVATION_MARKERS) && !text.contains("不好写") {
            self.motivation = Some("觉得题目好写/材料容易找".to_owned());
            update.motivation_captured = true;
        } else if let Some(motivation) = extract_after_markers(text, MOTIVATION_MARKERS, 4) {
            self.motivation = Some(motivation);
            update.motivation_captured = true;
        }
        if let Some(scene) = extract_observed_scene(text) {
            self.observed_scene = Some(scene);
            update.scene_captured = true;
        }
        if let Some(confusion) = extract_confusion_point(text) {
            self.confusion_point = Some(confusion);
        }
        if let Some(mechanism) = extract_suspected_mechanism(text) {
            self.suspected_mechanism = Some(mechanism);
        }
        if let Some(mechanism) = extract_selected_mechanism(text) {
            self.selected_mechanism = Some(mechanism);
        }
        if let Some(claim) = extract_after_markers(text, CLAIM_MARKERS, 4) {
            self.core_claim = Some(claim);
            update.claim_captured = true;
        }

        if let Some(selected) = parse_numbered_choice(text)
            && let Some(path) = self
                .candidate_paths
                .iter()
                .find(|path| path.index == selected)
        {
            let path = path.clone();
            self.selected_path_id = Some(selected.clone());
            self.selected_path = Some(path.title.clone());
            self.selected_direction = Some(path.title.clone());
            self.selected_path_detail = Some(path);
            update.selected_path_id = Some(selected);
        } else if self.selected_direction.is_some() && contains_any(text, CHOICE_REASON_MARKERS) {
            self.choice_reason = Some(truncate_chars(text, 240));
        }

        if let Some(evidence) = extract_positive_evidence(text) {
            update.evidence_added = append_unique(&mut self.evidence, evidence, 8);
        }
        if contains_any(text, COUNTEREXAMPLE_MARKERS) {
            let item = truncate_chars(text, 240);
            append_unique(&mut self.counterexamples, item.clone(), 6);
            append_unique(&mut self.counterarguments, item, 6);
        }

        self.context_summary = self.build_context_summary();

        update
    }

    fn clear_topic_dependents(&mut self) {
        self.selected_direction = None;
        self.research_question = None;
        self.motivation = None;
        self.observed_scene = None;
        self.confusion_point = None;
        self.suspected_mechanism = None;
        self.selected_mechanism = None;
        self.context_summary = None;
        self.core_claim = None;
        self.selected_path_id = None;
        self.selected_path = None;
        self.selected_path_detail = None;
        self.choice_reason = None;
        self.thinking_stage = Some(FlowStage::MotivationProbe);
        self.flow_stage = Some(FlowStage::MotivationProbe);
        self.candidate_paths.clear();
        self.evidence.clear();
        self.counterexamples.clear();
        self.counterarguments.clear();
        self.ready_for_refined_advice = false;
        self.socratic_rounds = 1;
        for key in [
            "theory_entry",
            "material_gap",
            "rejected_paths",
            "rejected_path_reason",
            "reflection_notes",
            "socratic_questions",
            "pending_questions",
            "unanswered_questions",
            "pre_conference_summary",
            "thinking_task",
            "last_intent",
            "route_decision",
            "route_history",
        ] {
            self.extra.remove(key);
        }
    }

    fn is_empty(&self) -> bool {
        !self.has_serializable_stage()
            && self.topic.is_none()
            && self.selected_direction.is_none()
            && self.research_question.is_none()
            && self.initial_idea.is_none()
            && self.motivation.is_none()
            && self.observed_scene.is_none()
            && self.confusion_point.is_none()
            && self.suspected_mechanism.is_none()
            && self.selected_mechanism.is_none()
            && self.context_summary.is_none()
            && self.core_claim.is_none()
            && self.selected_path_id.is_none()
            && self.selected_path.is_none()
            && self.selected_path_detail.is_none()
            && self.choice_reason.is_none()
            && self.thinking_stage.is_none()
            && self.flow_stage.is_none()
            && self.candidate_paths.is_empty()
            && self.evidence.is_empty()
            && self.counterexamples.is_empty()
            && self.counterarguments.is_empty()
            && !self.ready_for_refined_advice
            && self.socratic_rounds == 0
            && self.extra.is_empty()
    }

    fn has_serializable_stage(&self) -> bool {
        self.stage_present || self.stage != WritingStage::default()
    }

    fn build_context_summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        let topic = self.topic.as_deref().or(self.initial_idea.as_deref());
        if let Some(topic) = topic {
            parts.push(format!("主题：{topic}"));
        }
        if let Some(initial_idea) = self.initial_idea.as_deref()
            && Some(initial_idea) != topic
        {
            parts.push(format!("学生原始表述：{initial_idea}"));
        }
        for (label, value) in [
            ("已知场景", self.observed_scene.as_deref()),
            ("动机/触发点", self.motivation.as_deref()),
            ("真正困惑", self.confusion_point.as_deref()),
            ("学生猜测机制", self.suspected_mechanism.as_deref()),
            ("已选关键机制", self.selected_mechanism.as_deref()),
            ("已选路径", self.selected_path.as_deref()),
            ("选择理由", self.choice_reason.as_deref()),
            ("研究问题", self.research_question.as_deref()),
        ] {
            if let Some(value) = value {
                parts.push(format!("{label}：{value}"));
            }
        }
        (!parts.is_empty()).then(|| parts.join("；"))
    }
}

impl Serialize for WritingContext {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = self.extra.clone();
        if self.has_serializable_stage() {
            object.insert(
                "stage".to_owned(),
                Value::String(self.stage.as_str().to_owned()),
            );
        }
        insert_string(&mut object, "topic", self.topic.as_ref());
        insert_string(
            &mut object,
            "selected_direction",
            self.selected_direction.as_ref(),
        );
        insert_string(
            &mut object,
            "research_question",
            self.research_question.as_ref(),
        );
        insert_string(&mut object, "initial_idea", self.initial_idea.as_ref());
        insert_string(&mut object, "motivation", self.motivation.as_ref());
        insert_string(&mut object, "observed_scene", self.observed_scene.as_ref());
        insert_string(
            &mut object,
            "confusion_point",
            self.confusion_point.as_ref(),
        );
        insert_string(
            &mut object,
            "suspected_mechanism",
            self.suspected_mechanism.as_ref(),
        );
        insert_string(
            &mut object,
            "selected_mechanism",
            self.selected_mechanism.as_ref(),
        );
        insert_string(
            &mut object,
            "context_summary",
            self.context_summary.as_ref(),
        );
        insert_string(&mut object, "core_claim", self.core_claim.as_ref());
        insert_string(
            &mut object,
            "selected_path_id",
            self.selected_path_id.as_ref(),
        );
        insert_string(&mut object, "selected_path", self.selected_path.as_ref());
        if let Some(detail) = &self.selected_path_detail {
            object.insert(
                "selected_path_detail".to_owned(),
                serde_json::to_value(detail).map_err(serde::ser::Error::custom)?,
            );
        }
        insert_string(&mut object, "choice_reason", self.choice_reason.as_ref());
        insert_flow_stage(&mut object, "thinking_stage", self.thinking_stage.as_ref());
        insert_flow_stage(&mut object, "flow_stage", self.flow_stage.as_ref());
        insert_nonempty_collection(&mut object, "candidate_paths", &self.candidate_paths)
            .map_err(serde::ser::Error::custom)?;
        insert_nonempty_collection(&mut object, "evidence_items", &self.evidence)
            .map_err(serde::ser::Error::custom)?;
        insert_nonempty_collection(&mut object, "counterexamples", &self.counterexamples)
            .map_err(serde::ser::Error::custom)?;
        insert_nonempty_collection(&mut object, "counterarguments", &self.counterarguments)
            .map_err(serde::ser::Error::custom)?;
        if self.ready_for_refined_advice {
            object.insert("ready_for_refined_advice".to_owned(), Value::Bool(true));
        }
        if self.socratic_rounds != 0 {
            object.insert(
                "socratic_rounds".to_owned(),
                Value::Number(self.socratic_rounds.into()),
            );
        }
        Value::Object(object).serialize(serializer)
    }
}

fn insert_string(object: &mut Map<String, Value>, key: &str, value: Option<&String>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Value::String(value.clone()));
    }
}

fn insert_flow_stage(object: &mut Map<String, Value>, key: &str, value: Option<&FlowStage>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Value::String(value.as_str().to_owned()));
    }
}

fn insert_nonempty_collection<T: Serialize>(
    object: &mut Map<String, Value>,
    key: &str,
    values: &[T],
) -> Result<(), serde_json::Error> {
    if !values.is_empty() {
        object.insert(key.to_owned(), serde_json::to_value(values)?);
    }
    Ok(())
}

fn extract_topic(text: &str) -> Option<(String, bool)> {
    for clause in text
        .split(['，', ',', '。', '！', '？', '；', ';', '\n'])
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
    {
        if let Some(topic) = extract_explicit_topic_clause(clause) {
            return Some((topic, true));
        }
    }
    if text.chars().count() <= 40
        && contains_any(text, SHORT_TOPIC_SIGNAL_MARKERS)
        && !contains_any(text, FOLLOWUP_TOPIC_BLOCKERS)
    {
        return trim_topic(text).map(|topic| (topic, false));
    }
    None
}

fn extract_explicit_topic_clause(sentence: &str) -> Option<String> {
    for prefix in NEW_TOPIC_PREFIXES {
        if let Some(rest) = sentence.strip_prefix(prefix) {
            return trim_topic(strip_first_prefix(rest.trim(), TOPIC_ACTION_PREFIXES));
        }
    }

    let without_lead = strip_first_prefix(sentence, TOPIC_LEAD_PREFIXES);
    for action in TOPIC_ACTION_PREFIXES {
        if let Some(rest) = without_lead.strip_prefix(action) {
            return trim_topic(rest);
        }
    }
    None
}

fn trim_topic(value: &str) -> Option<String> {
    let topic = value
        .trim_matches(|character: char| {
            character.is_whitespace() || "：: 了，。！？,!?".contains(character)
        })
        .trim_start_matches("这个")
        .trim_start_matches("有关")
        .trim_start_matches("关于")
        .trim()
        .to_owned();
    (topic.chars().count() >= 2).then_some(topic)
}

fn strip_first_prefix<'a>(value: &'a str, prefixes: &[&str]) -> &'a str {
    prefixes
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))
        .unwrap_or(value)
        .trim()
}

fn same_topic_family(current: &str, topic: &str) -> bool {
    let left = normalize_topic(current);
    let right = normalize_topic(topic);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left.contains(&right) || right.contains(&left) || shared_bigrams(&left, &right) >= 2
}

fn normalize_topic(value: &str) -> String {
    let mut normalized: String = value
        .chars()
        .filter(|character| !character.is_whitespace() && !"，。！？、:：".contains(*character))
        .collect();
    for token in [
        "我想",
        "想要",
        "准备",
        "打算",
        "研究",
        "讨论",
        "分析",
        "关于",
        "有关",
        "的话题",
        "这个话题",
        "这个问题",
        "的问题",
        "一篇",
        "一个",
    ] {
        normalized = normalized.replace(token, "");
    }
    normalized
}

fn shared_bigrams(left: &str, right: &str) -> usize {
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    left_chars
        .windows(2)
        .filter(|pair| right_chars.windows(2).any(|candidate| candidate == *pair))
        .count()
}

fn extract_after_markers(text: &str, markers: &[&str], minimum_chars: usize) -> Option<String> {
    markers.iter().find_map(|marker| {
        let (_, rest) = text.split_once(marker)?;
        let value = first_sentence(rest).trim_matches(|character: char| {
            character.is_whitespace() || "：: ，,".contains(character)
        });
        (value.chars().count() >= minimum_chars).then(|| truncate_chars(value, 240))
    })
}

fn first_sentence(text: &str) -> &str {
    text.split(['。', '！', '？', '\n'])
        .next()
        .unwrap_or(text)
        .trim()
}

fn clean_scene(text: &str) -> String {
    let mut scene = truncate_chars(text, 240);
    for prefix in ["当然是", "就是", "不是，", "不是,"] {
        if let Some(rest) = scene.strip_prefix(prefix) {
            scene = rest.trim().to_owned();
            break;
        }
    }
    scene
}

fn extract_observed_scene(text: &str) -> Option<String> {
    if contains_any(text, &["为什么替人干活", "替人干活反而", "默认选项"]) {
        return None;
    }
    if text.starts_with("不是")
        && !contains_any(
            text,
            &["拖了进度", "拖进度", "大作业", "课程作业", "小组作业"],
        )
    {
        return None;
    }
    contains_any(text, SCENE_MARKERS).then(|| clean_scene(text))
}

fn extract_confusion_point(text: &str) -> Option<String> {
    if contains_any(text, &["为什么替人干活", "替人干活反而", "默认选项"])
        || (text.contains("拖了进度") && contains_any(text, &["替他", "替人", "做了"]))
    {
        return Some("为什么替人干活反而成了默认选项".to_owned());
    }
    if contains_any(text, &["不敢催", "不好意思说", "抹不下脸", "碍于面子"]) {
        return Some("有人拖进度后，其他成员为什么不敢催，最后替他完成".to_owned());
    }
    if text.contains("分工不均") && contains_any(text, &["正常进行", "仍然", "但是"]) {
        return Some("为什么分工不均但小组仍能正常推进".to_owned());
    }
    None
}

fn extract_suspected_mechanism(text: &str) -> Option<String> {
    if contains_any(text, &["抹不下脸", "碍于面子", "不好意思"]) {
        return Some("面子压力/关系顾虑".to_owned());
    }
    if text.contains("评分") {
        return Some("评分规则".to_owned());
    }
    contains_any(text, &["责任", "没人管", "监督"]).then(|| "责任分散或监督不足".to_owned())
}

fn extract_selected_mechanism(text: &str) -> Option<String> {
    for (markers, value) in [
        (&["关系成本", "关系变僵", "撕破脸"][..], "关系成本"),
        (&["评价成本", "斤斤计较", "不近人情"][..], "评价成本"),
        (
            &["成绩成本", "成绩风险", "影响最终作业", "影响最后成绩"][..],
            "成绩成本",
        ),
        (&["拖延者", "知道别人不好意思催"][..], "拖延者预期"),
        (&["承担者", "自己补上", "代做"][..], "承担者代做"),
        (&["评分规则", "共同成绩", "过程评价"][..], "评分规则"),
    ] {
        if contains_any(text, markers) {
            return Some(value.to_owned());
        }
    }
    None
}

fn parse_numbered_choice(text: &str) -> Option<String> {
    let exact = [
        ("第一个", "1"),
        ("第二个", "2"),
        ("第三个", "3"),
        ("第四个", "4"),
        ("第五个", "5"),
        ("第六个", "6"),
        ("方向一", "1"),
        ("方向二", "2"),
        ("方向三", "3"),
        ("方向四", "4"),
        ("方向五", "5"),
        ("方向六", "6"),
    ];
    let mut value = text.trim().to_lowercase();
    while let Some(stripped) = ["了", "吧", "可以"]
        .iter()
        .find_map(|suffix| value.strip_suffix(suffix))
    {
        value = stripped.trim().to_owned();
    }
    if let Some((_, number)) = exact.iter().find(|(phrase, _)| value == *phrase) {
        return Some((*number).to_owned());
    }
    if let Some(number) = embedded_choice_number(&value) {
        return Some(number);
    }
    value = strip_owned_prefix(
        value,
        &[
            "我选择",
            "我想选",
            "我选",
            "选择",
            "想选",
            "就选",
            "要",
            "选",
        ],
    );
    value = strip_owned_prefix(value, &["方向", "第"]);
    for suffix in ["个方向", "号方向", "方向", "题目", "个", "号"] {
        if let Some(stripped) = value.strip_suffix(suffix) {
            value = stripped.trim().to_owned();
            break;
        }
    }
    match value.as_str() {
        "1" | "一" => Some("1".to_owned()),
        "2" | "二" => Some("2".to_owned()),
        "3" | "三" => Some("3".to_owned()),
        "4" | "四" => Some("4".to_owned()),
        "5" | "五" => Some("5".to_owned()),
        "6" | "六" => Some("6".to_owned()),
        _ => None,
    }
}

fn embedded_choice_number(value: &str) -> Option<String> {
    for (token, number) in EMBEDDED_CHOICE_TOKENS {
        for (index, _) in value.match_indices(token) {
            let prefix = &value[..index];
            let suffix = &value[index + token.len()..];
            if has_embedded_choice_left_boundary(prefix)
                && has_embedded_choice_right_boundary(suffix)
            {
                return Some((*number).to_owned());
            }
        }
    }
    None
}

fn has_embedded_choice_left_boundary(prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    let trimmed = prefix.trim_end();
    EMBEDDED_CHOICE_INTENT_PREFIXES
        .iter()
        .any(|intent| trimmed.ends_with(intent))
        || prefix.chars().next_back().is_some_and(is_choice_boundary)
}

fn has_embedded_choice_right_boundary(suffix: &str) -> bool {
    suffix.is_empty()
        || suffix.chars().next().is_some_and(is_choice_boundary)
        || EMBEDDED_CHOICE_SUFFIXES
            .iter()
            .any(|ending| suffix.starts_with(ending))
}

fn is_choice_boundary(character: char) -> bool {
    character.is_whitespace() || "，,。.！!？?；;:：、".contains(character)
}

fn strip_owned_prefix(mut value: String, prefixes: &[&str]) -> String {
    if let Some(prefix) = prefixes.iter().find(|prefix| value.starts_with(**prefix)) {
        value = value[prefix.len()..].trim().to_owned();
    }
    value
}

fn contains_any(text: &str, rules: &[&str]) -> bool {
    rules.iter().any(|rule| text.contains(rule))
}

fn extract_positive_evidence(text: &str) -> Option<String> {
    if contains_any(text, EVIDENCE_REJECTION_MARKERS) {
        return None;
    }
    let positive_clauses: Vec<&str> = text
        .split(['，', ',', '。', '！', '？', '；', ';', '\n'])
        .map(str::trim)
        .filter(|clause| !clause.is_empty())
        .filter(|clause| contains_any(clause, EVIDENCE_MARKERS))
        .filter(|clause| !contains_any(clause, COUNTEREXAMPLE_MARKERS))
        .filter(|clause| !contains_any(clause, NEGATED_EVIDENCE_MARKERS))
        .collect();
    (!positive_clauses.is_empty()).then(|| truncate_chars(&positive_clauses.join("，"), 240))
}

fn append_unique(items: &mut Vec<String>, value: String, limit: usize) -> bool {
    if items.contains(&value) {
        return false;
    }
    items.push(value);
    if items.len() > limit {
        items.drain(..items.len() - limit);
    }
    true
}

fn extract_search_keywords(message: &str, context: &WritingContext) -> Vec<String> {
    let basis = [
        Some(message),
        context.topic.as_deref(),
        context.selected_direction.as_deref(),
        context.research_question.as_deref(),
        context.extra.get("theory_entry").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    let mut keywords = Vec::new();
    for (needle, keyword) in [
        ("搭子", "搭子"),
        ("朋友", "朋友关系"),
        ("小组合作", "小组合作"),
        ("分工", "分工不均"),
        ("搭便车", "搭便车"),
        ("团队", "团队合作"),
        ("社会惰化", "社会惰化"),
        ("友谊", "友谊"),
        ("弱连接", "弱连接"),
        ("弱关系", "弱连接"),
        ("强连接", "强连接"),
        ("社会交换", "社会交换理论"),
        ("社会网络", "社会网络理论"),
        ("情感支持", "情感支持"),
        ("同伴关系", "同伴关系"),
        ("青年社交", "青年社交"),
        ("大学生", "大学生"),
        ("AI", "AI写作"),
        ("人工智能", "AI写作"),
        ("学术自我效能", "学术自我效能"),
        ("教育", "教育"),
        ("精英", "精英教育"),
        ("博弈", "博弈论"),
    ] {
        if basis.contains(needle) && !keywords.iter().any(|item| item == keyword) {
            keywords.push(keyword.to_owned());
        }
    }
    keywords.truncate(5);
    keywords
}

fn is_literature_lookup_message(message: &str) -> bool {
    contains_any(
        &message.to_lowercase(),
        &[
            "文献",
            "参考文献",
            "资料",
            "材料",
            "上网",
            "联网",
            "搜索",
            "搜一下",
            "找一下",
            "找一些",
            "找找",
            "查一下",
            "检索",
            "openalex",
            "open alex",
        ],
    )
}

fn extract_numbered_options(reply: &str) -> Vec<CandidatePath> {
    let mut options = Vec::new();
    for line in reply.lines().map(str::trim) {
        let Some((index, rest)) = line.split_once(['.', '、']) else {
            continue;
        };
        if !matches!(
            index.trim(),
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
        ) {
            continue;
        }
        let title = rest
            .split(['：', ':', '。'])
            .next()
            .unwrap_or_default()
            .trim();
        if title.chars().count() >= 2 {
            options.push(CandidatePath {
                index: index.trim().to_owned(),
                title: title.chars().take(60).collect(),
                ..CandidatePath::default()
            });
        }
    }
    options
}

fn reply_contains_thinking_options(reply: &str) -> bool {
    contains_any(reply, &["候选", "方向", "路径", "核心问题", "可用材料"])
}

fn extract_questions(reply: &str) -> Vec<String> {
    reply
        .split_inclusive(['？', '?'])
        .map(str::trim)
        .filter(|part| part.ends_with('？') || part.ends_with('?'))
        .map(|part| {
            part.rsplit(['\n', '。'])
                .next()
                .unwrap_or(part)
                .trim_start_matches(|character: char| {
                    character.is_ascii_digit() || ".、：: -".contains(character)
                })
                .trim()
                .to_owned()
        })
        .filter(|question| {
            !contains_any(
                question,
                &[
                    "核心问题",
                    "这一路的核心",
                    "研究问题",
                    "解释：",
                    "最适合收束成",
                ],
            )
        })
        .collect()
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

fn deserialize_stage_or_default<'de, D>(deserializer: D) -> Result<WritingStage, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<WritingStage>::deserialize(deserializer).map(Option::unwrap_or_default)
}

#[derive(Clone, Debug, Default)]
pub struct RouteInput {
    pub message: String,
    pub current_skill: Option<String>,
    pub awaiting_slots: Vec<String>,
    pub collected_slots: bool,
    pub writing_context: Map<String, Value>,
    pub route_needs_socratic: bool,
}

impl RouteInput {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Self::default()
        }
    }

    pub fn with_current_skill(mut self, skill: impl Into<String>) -> Self {
        self.current_skill = Some(skill.into());
        self
    }

    pub fn with_awaiting_slots<I, S>(mut self, slots: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.awaiting_slots = slots.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_collected_slots(mut self, collected_slots: bool) -> Self {
        self.collected_slots = collected_slots;
        self
    }

    pub fn with_writing_context(mut self, context: Value) -> Self {
        self.writing_context = context.as_object().cloned().unwrap_or_default();
        self
    }

    pub fn with_route_needs_socratic(mut self, needs_socratic: bool) -> Self {
        self.route_needs_socratic = needs_socratic;
        self
    }

    pub fn context_value(&self, key: &str) -> Option<&Value> {
        self.writing_context.get(key)
    }

    pub fn context_has(&self, key: &str) -> bool {
        self.context_value(key)
            .is_some_and(|value| !value.is_null() && value != "")
    }
}
