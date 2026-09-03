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
    pub required_action: String,
    pub missing_slot: Option<String>,
    pub allowed_response_kind: String,
    pub selected_option: Option<String>,
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
        if asks_for_summary(message) {
            return FlowDecision {
                stage: FlowStage::SummaryReady,
                prompt_kind: PromptKind::SummaryReady,
                required_action: "build_summary".to_owned(),
                missing_slot: None,
                allowed_response_kind: "summary".to_owned(),
                selected_option: None,
                candidate_paths: context.candidate_paths.clone(),
                missing_evidence: false,
                ready_for_summary: true,
            };
        }

        if let Some(selected) =
            selected_option(message).filter(|_| !context.candidate_paths.is_empty())
        {
            return decision_with_contract(
                FlowStage::ChoiceReflection,
                PromptKind::ChoiceReflection,
                "reflect_on_choice",
                Some("choice_reason"),
                "question",
                Some(selected),
                context.candidate_paths.clone(),
            );
        }

        match current_stage(context) {
            FlowStage::ChoiceReflection if has_choice_reason(message) => {
                return decision_with_contract(
                    FlowStage::EvidenceCheck,
                    PromptKind::EvidenceCheck,
                    "check_evidence",
                    Some("evidence_or_counterexample"),
                    "question",
                    None,
                    context.candidate_paths.clone(),
                );
            }
            FlowStage::ChoiceReflection => {
                return decision_with_contract(
                    FlowStage::ChoiceReflection,
                    PromptKind::ChoiceReflection,
                    "ask_choice_reason",
                    Some("choice_reason"),
                    "question",
                    None,
                    context.candidate_paths.clone(),
                );
            }
            FlowStage::EvidenceCheck if has_evidence_or_counterexample(message) => {
                return decision_with_contract(
                    FlowStage::RefinedAdvice,
                    PromptKind::RefinedAdvice,
                    "give_refined_advice",
                    None,
                    "advice",
                    None,
                    context.candidate_paths.clone(),
                );
            }
            FlowStage::EvidenceCheck => {
                return FlowDecision {
                    stage: FlowStage::EvidenceCheck,
                    prompt_kind: PromptKind::EvidenceCheck,
                    required_action: "ask_evidence".to_owned(),
                    missing_slot: Some("evidence_or_counterexample".to_owned()),
                    allowed_response_kind: "question".to_owned(),
                    selected_option: None,
                    candidate_paths: context.candidate_paths.clone(),
                    missing_evidence: true,
                    ready_for_summary: false,
                };
            }
            FlowStage::RefinedAdvice => {
                return decision_with_contract(
                    FlowStage::RefinedAdvice,
                    PromptKind::RefinedAdvice,
                    "give_refined_advice",
                    None,
                    "advice",
                    None,
                    context.candidate_paths.clone(),
                );
            }
            _ => {}
        }

        if has_enough_for_candidates(context, message) {
            return decision_with_contract(
                FlowStage::CandidatePaths,
                PromptKind::CandidatePaths,
                "offer_candidate_paths",
                Some("selected_path"),
                "options",
                None,
                build_candidate_paths(context),
            );
        }

        decision_with_contract(
            FlowStage::MotivationProbe,
            PromptKind::MotivationProbe,
            "probe_motivation",
            Some("motivation_or_scene"),
            "question",
            None,
            Vec::new(),
        )
    }
}

fn decision_with_contract(
    stage: FlowStage,
    prompt_kind: PromptKind,
    required_action: &str,
    missing_slot: Option<&str>,
    allowed_response_kind: &str,
    selected_option: Option<String>,
    candidate_paths: Vec<CandidatePath>,
) -> FlowDecision {
    let missing_evidence = stage == FlowStage::EvidenceCheck && missing_slot.is_some();
    FlowDecision {
        stage,
        prompt_kind,
        required_action: required_action.to_owned(),
        missing_slot: missing_slot.map(str::to_owned),
        allowed_response_kind: allowed_response_kind.to_owned(),
        selected_option,
        candidate_paths,
        missing_evidence,
        ready_for_summary: false,
    }
}

fn asks_for_summary(message: &str) -> bool {
    SUMMARY_REQUEST_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
}

fn current_stage(context: &WritingContext) -> FlowStage {
    context
        .flow_stage
        .as_ref()
        .or(context.thinking_stage.as_ref())
        .filter(|stage| {
            matches!(
                stage,
                FlowStage::MotivationProbe
                    | FlowStage::CandidatePaths
                    | FlowStage::ChoiceReflection
                    | FlowStage::EvidenceCheck
                    | FlowStage::RefinedAdvice
                    | FlowStage::SummaryReady
            )
        })
        .cloned()
        .unwrap_or(FlowStage::MotivationProbe)
}

fn selected_option(text: &str) -> Option<String> {
    let mut value = text.trim().to_lowercase();
    for suffix in ["可以", "了", "吧"] {
        if let Some(stripped) = value.strip_suffix(suffix) {
            value = stripped.trim().to_owned();
        }
    }
    for prefix in ["我选择", "我想选", "我选", "选择", "想选", "就", "要", "选"] {
        if let Some(stripped) = value.strip_prefix(prefix) {
            value = stripped.trim().to_owned();
            break;
        }
    }
    if let Some(stripped) = value
        .strip_prefix("方向")
        .or_else(|| value.strip_prefix("第"))
    {
        value = stripped.trim().to_owned();
    }
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

fn has_choice_reason(text: &str) -> bool {
    selected_option(text).is_none()
        && [
            "因为",
            "我选",
            "选择",
            "没选",
            "不选",
            "更适合",
            "材料",
            "感受",
        ]
        .iter()
        .any(|marker| text.contains(marker))
}

fn has_evidence_or_counterexample(text: &str) -> bool {
    [
        "证据",
        "例子",
        "案例",
        "访谈",
        "材料",
        "数据",
        "经历",
        "观察",
        "反例",
        "反方",
        "反驳",
        "相反",
        "不一定",
        "但是也可能",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn build_candidate_paths(context: &WritingContext) -> Vec<CandidatePath> {
    let task = context
        .extra
        .get("thinking_task")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("选题");
    if task.contains("文献综述") {
        return vec![
            candidate(
                "1",
                "概念脉络",
                "核心概念在不同研究里如何被界定、区分和使用？",
                "适合用定义型文献、综述和课程概念。",
                "容易变成概念罗列，需要比较不同定义的差异。",
            ),
            candidate(
                "2",
                "理论脉络",
                "哪些理论解释了这个现象，它们各自能解释到哪里？",
                "适合用经典理论、高被引论文和课程理论框架。",
                "理论太多会散，需要选一个主入口。",
            ),
            candidate(
                "3",
                "方法脉络",
                "现有研究分别用访谈、问卷、文本分析回答了什么？",
                "适合比较实证研究的方法和样本。",
                "不能只列方法，要说明方法限制了什么结论。",
            ),
        ];
    }
    if task.contains("修改") {
        return vec![
            candidate(
                "1",
                "核心论点优先",
                "文章到底要让读者接受哪一个可争辩判断？",
                "适合已有初稿但中心判断不清的情况。",
                "只改句子不改判断，文章会继续散。",
            ),
            candidate(
                "2",
                "证据链优先",
                "每个例子后是否解释了它证明什么？",
                "适合材料多但论证跳跃的情况。",
                "容易堆案例，需要补分析句。",
            ),
            candidate(
                "3",
                "结构功能优先",
                "每段是否承担了清楚且不重复的功能？",
                "适合段落重复、顺序混乱或过渡弱的初稿。",
                "重排结构前要先保住核心论点。",
            ),
        ];
    }
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
                "大学生如何区分搭子和朋友，边界体现在哪些互动里？",
                "适合用自己的关系例子、身边访谈、社交平台讨论和已有文献里的概念定义。",
                "不要只写“搭子是什么、朋友是什么”，要比较活动场景、情感投入和责任期待。",
            ),
            candidate(
                "2",
                "功能替代方向",
                "搭子是在替代朋友的某些功能，还是一种独立的轻关系？",
                "适合比较饭搭子、学习搭子、运动搭子等不同场景，以及朋友在这些场景里的角色。",
                "容易写成泛泛的青年社交变化，需要锁定一两个具体功能，比如陪伴、信息交换或情绪支持。",
            ),
            candidate(
                "3",
                "关系转化方向",
                "什么条件下搭子会变成朋友，什么条件下只停留在搭子？",
                "适合访谈有搭子经历的同学，或分析“搭子变朋友/没有变朋友”的帖子。",
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
