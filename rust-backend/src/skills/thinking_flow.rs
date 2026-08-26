use serde::{Deserialize, Serialize};

use crate::domain::{CandidatePath, FlowStage, WritingContext};

const SUMMARY_REQUEST_MARKERS: &[&str] = &["总结一下", "面批前摘要", "生成摘要", "可以总结"];
const RELATIONSHIP_TOPIC_MARKERS: &[&str] = &["搭子", "朋友", "友谊", "好友"];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    MotivationProbe,
    CandidatePaths,
    ChoiceReflection,
    EvidenceCheck,
    RefinedAdvice,
    SummaryReady,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FlowDecision {
    pub stage: FlowStage,
    pub prompt_kind: PromptKind,
    pub candidate_paths: Vec<CandidatePath>,
    pub missing_evidence: bool,
    pub ready_for_summary: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ThinkingFlowController;

impl ThinkingFlowController {
    pub fn new() -> Self {
        Self
    }

    pub fn advance(&self, context: &WritingContext, message: &str) -> FlowDecision {
        if context.motivation.is_none()
            && context.observed_scene.is_none()
            && context.evidence.is_empty()
        {
            return decision(
                FlowStage::MotivationProbe,
                PromptKind::MotivationProbe,
                Vec::new(),
            );
        }

        let has_direction = context.selected_direction.is_some() || context.selected_path.is_some();
        let has_reason = context.choice_reason.is_some();
        let has_evidence = !context.evidence.is_empty();
        let prerequisites_complete = has_direction && has_reason && has_evidence;
        if asks_for_summary(message) && prerequisites_complete {
            return FlowDecision {
                stage: FlowStage::SummaryReady,
                prompt_kind: PromptKind::SummaryReady,
                candidate_paths: context.candidate_paths.clone(),
                missing_evidence: false,
                ready_for_summary: true,
            };
        }

        if has_direction {
            if !has_reason {
                return decision(
                    FlowStage::ChoiceReflection,
                    PromptKind::ChoiceReflection,
                    context.candidate_paths.clone(),
                );
            }
            if !has_evidence {
                return FlowDecision {
                    stage: FlowStage::EvidenceCheck,
                    prompt_kind: PromptKind::EvidenceCheck,
                    candidate_paths: context.candidate_paths.clone(),
                    missing_evidence: true,
                    ready_for_summary: false,
                };
            }
            return decision(
                FlowStage::RefinedAdvice,
                PromptKind::RefinedAdvice,
                context.candidate_paths.clone(),
            );
        }

        let candidate_paths = if context.candidate_paths.is_empty() {
            build_candidate_paths(context)
        } else {
            context.candidate_paths.clone()
        };
        decision(
            FlowStage::CandidatePaths,
            PromptKind::CandidatePaths,
            candidate_paths,
        )
    }
}

fn decision(
    stage: FlowStage,
    prompt_kind: PromptKind,
    candidate_paths: Vec<CandidatePath>,
) -> FlowDecision {
    FlowDecision {
        stage,
        prompt_kind,
        candidate_paths,
        missing_evidence: false,
        ready_for_summary: false,
    }
}

fn asks_for_summary(message: &str) -> bool {
    SUMMARY_REQUEST_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
}

fn build_candidate_paths(context: &WritingContext) -> Vec<CandidatePath> {
    let topic = [
        context.topic.as_deref(),
        context.initial_idea.as_deref(),
        context.motivation.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");

    if RELATIONSHIP_TOPIC_MARKERS
        .iter()
        .any(|marker| topic.contains(marker))
    {
        return vec![
            candidate(
                "1",
                "边界区分方向",
                "学生如何区分两种关系，边界体现在哪些互动里？",
                "适合用关系例子、访谈和概念定义。",
                "不要只列概念，要比较场景、情感投入和责任期待。",
            ),
            candidate(
                "2",
                "功能替代方向",
                "轻关系在替代传统关系的哪些功能，还是一种独立形式？",
                "适合比较不同场景中的陪伴、信息交换和情绪支持。",
                "需要锁定一两个具体功能，避免泛谈社交变化。",
            ),
            candidate(
                "3",
                "关系转化方向",
                "什么条件下轻关系会变得更深，什么条件下保持原状？",
                "适合比较发生转化和没有转化的经历或访谈。",
                "要解释转化条件，不能只讲有趣故事。",
            ),
        ];
    }

    vec![
        candidate(
            "1",
            "比较对象方向",
            "把两个相近对象放在一起比较，它们真正差在哪里？",
            "适合有两个概念、群体或场景可以对照的选题。",
            "比较维度不能太多，最好先选二到三个。",
        ),
        candidate(
            "2",
            "机制解释方向",
            "这个现象为什么发生，中间有哪些可观察环节？",
            "适合有具体经历、案例或访谈材料的选题。",
            "不要只写原因列表，要连成因果链。",
        ),
        candidate(
            "3",
            "条件边界方向",
            "这个现象在什么条件下更明显，什么条件下不成立？",
            "适合做不同场景或不同人群的比较。",
            "需要准备反例，否则判断会太满。",
        ),
    ]
}

fn candidate(
    index: &str,
    title: &str,
    core_question: &str,
    material_type: &str,
    risk: &str,
) -> CandidatePath {
    CandidatePath {
        index: index.to_owned(),
        title: title.to_owned(),
        core_question: core_question.to_owned(),
        material_type: material_type.to_owned(),
        risk: risk.to_owned(),
        ..CandidatePath::default()
    }
}
