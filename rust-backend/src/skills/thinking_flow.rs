use serde::{Deserialize, Serialize};

use crate::domain::{CandidatePath, FlowStage, WritingContext};

const SUMMARY_REQUEST_MARKERS: &[&str] = &["总结一下", "面批前摘要", "生成摘要", "可以总结"];
const RELATIONSHIP_TOPIC_MARKERS: &[&str] = &["搭子", "朋友", "友谊", "好友"];
const GROUP_WORK_TOPIC_MARKERS: &[&str] = &["小组合作", "小组分工", "分工不均", "大作业", "分工"];

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

        if !has_enough_for_candidates(context, message) {
            return decision(
                FlowStage::MotivationProbe,
                PromptKind::MotivationProbe,
                Vec::new(),
            );
        }

        // Rebuild while the student is still exploring: a later turn can add a mechanism that
        // makes a previously generic option set too coarse.
        let candidate_paths = build_candidate_paths(context);
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

    if GROUP_WORK_TOPIC_MARKERS
        .iter()
        .any(|marker| topic.contains(marker))
    {
        let face_saving_thread = context.confusion_point.as_deref()
            == Some("为什么替人干活反而成了默认选项")
            || context.suspected_mechanism.as_deref() == Some("面子压力/关系顾虑")
            || ["不好意思", "抹不下脸", "不敢催", "替人", "别人不干活"]
                .iter()
                .any(|marker| topic.contains(marker));
        if face_saving_thread {
            return vec![
                candidate(
                    "1",
                    "沉默成本方向",
                    "为什么组员明知有人没干活，却觉得开口催比自己补上更难？",
                    "适合访谈承担者，追问他们当时怕什么、怎么判断开口成本。",
                    "不要只说面子文化，要把面子具体化成关系尴尬、评价压力或成绩风险。",
                ),
                candidate(
                    "2",
                    "默认补位方向",
                    "一次次不好意思催，如何把谁急谁补上变成小组里的默认规则？",
                    "适合分析小组聊天记录、任务表和成员访谈，重建互动过程。",
                    "需要证明这是互动模式，不只是某个同学人好或某个同学偷懒。",
                ),
                candidate(
                    "3",
                    "评分制度方向",
                    "为什么在共同成绩或弱过程评价下，代做比公开冲突更像低成本选择？",
                    "适合比较不同评分规则的小组作业，或访谈组长和组员对成绩风险的判断。",
                    "不能只批评评分制度，要说明制度如何和面子压力一起起作用。",
                ),
            ];
        }
        return vec![
            candidate(
                "1",
                "动机解释方向",
                "学生为什么会选择少出力、多得分的搭便车策略？",
                "适合访谈搭便车者或分析社会惰化、成本收益计算。",
                "容易把问题简化成道德批评，需要解释具体心理机制。",
            ),
            candidate(
                "2",
                "互动机制方向",
                "搭便车者的推脱和其他成员的容忍如何互相强化？",
                "适合写具体小组互动案例、聊天记录或过程访谈。",
                "需要描述动态过程，不能只罗列个人心理。",
            ),
            candidate(
                "3",
                "情境条件方向",
                "什么任务类型、评分规则或人际关系下，分工不均更容易被容忍？",
                "适合比较不同课程小组、熟人组队和陌生人组队。",
                "变量会变多，需要先锁定一两个条件。",
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

fn has_enough_for_candidates(context: &WritingContext, message: &str) -> bool {
    let topic = [
        context.topic.as_deref(),
        context.initial_idea.as_deref(),
        context.motivation.as_deref(),
        context.selected_direction.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");

    if asks_for_topic_refinement(message) && !topic.is_empty() {
        return true;
    }
    if is_material_source_answer(message) && !topic.is_empty() {
        return true;
    }
    if GROUP_WORK_TOPIC_MARKERS
        .iter()
        .any(|marker| topic.contains(marker))
    {
        let mut signals = usize::from(context.observed_scene.is_some())
            + usize::from(context.confusion_point.is_some())
            + usize::from(
                context.selected_mechanism.is_some()
                    || context.suspected_mechanism.is_some()
                    || context.motivation.is_some(),
            );
        if [
            "默认选项",
            "替人干活",
            "不敢催",
            "拖了进度",
            "关系成本",
            "评价成本",
            "成绩成本",
            "评分规则",
            "承担者",
            "拖延者",
        ]
        .iter()
        .any(|marker| message.contains(marker))
        {
            signals += 1;
        }
        if ["不是", "别人不干活", "不好意思说", "抹不下脸"]
            .iter()
            .any(|marker| message.contains(marker))
        {
            signals += 1;
        }
        if context.confusion_point.as_deref() == Some("为什么替人干活反而成了默认选项")
        {
            return signals >= 3;
        }
        return signals >= 2
            && (context.selected_mechanism.is_some()
                || context.suspected_mechanism.is_some()
                || context.motivation.is_some());
    }

    if context.motivation.as_deref() == Some("觉得题目好写/材料容易找") {
        return !context.evidence.is_empty() && !is_material_source_answer(message);
    }
    context.motivation.is_some()
        || !context.evidence.is_empty()
        || ["因为", "我发现", "我观察到", "经常", "有些人", "具体经历"]
            .iter()
            .any(|marker| message.contains(marker))
}

fn asks_for_topic_refinement(message: &str) -> bool {
    [
        "细化",
        "怎么细化",
        "细化选题",
        "选题思路",
        "选题方向",
        "具体切口",
        "怎么拆",
        "怎么展开",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

fn is_material_source_answer(message: &str) -> bool {
    if ["别给我文献", "不要文献", "不用文献", "谁让你给我文献"]
        .iter()
        .any(|marker| message.contains(marker))
    {
        return false;
    }
    let has_source = [
        "网上的文献",
        "网络文献",
        "已有文献",
        "网上资料",
        "论文",
        "文献",
    ]
    .iter()
    .any(|marker| message.contains(marker));
    let has_answer_marker = ["就是", "主要是", "应该是", "可能是", "我想用", "用", "看"]
        .iter()
        .any(|marker| message.contains(marker));
    has_source && (message.chars().count() <= 24 || has_answer_marker)
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
