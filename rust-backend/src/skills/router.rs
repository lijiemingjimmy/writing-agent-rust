use crate::{
    domain::{RiskLevel, RouteDecision, RouteInput, WritingStage},
    skills::{SkillDefinition, SkillRegistry},
};

pub struct SkillRouter {
    registry: SkillRegistry,
}

impl SkillRouter {
    pub fn new(registry: SkillRegistry) -> Self {
        Self { registry }
    }

    pub fn route(&self, input: &RouteInput) -> RouteDecision {
        let original = input.message.trim().to_lowercase();
        let switch_payload = explicit_switch_payload(&original);
        let normalized = switch_payload
            .unwrap_or(original.as_str())
            .trim()
            .to_owned();
        let target = self
            .select(&normalized, input, switch_payload.is_some())
            .map(|skill| skill.id.clone());
        build_decision(&normalized, input, target)
    }

    fn select<'a>(
        &'a self,
        text: &str,
        input: &RouteInput,
        explicit_switch: bool,
    ) -> Option<&'a SkillDefinition> {
        if !explicit_switch
            && let Some(current) = input
                .current_skill
                .as_deref()
                .and_then(|id| self.registry.get(id))
        {
            return Some(current);
        }

        if (input.current_skill.is_none() || explicit_switch)
            && input.awaiting_slots.is_empty()
            && let Some(skill_id) = command_target(text)
        {
            return self.registry.get(skill_id);
        }

        if !explicit_switch
            && let Some(current) = input
                .current_skill
                .as_deref()
                .and_then(|id| self.registry.get(id))
        {
            if !input.awaiting_slots.is_empty() {
                return Some(current);
            }
            if current.id == "socratic_review"
                && (is_socratic_followup(text)
                    || is_selection_followup(text)
                    || is_socratic_thinking_request(text))
            {
                return Some(current);
            }
        }

        if let Some(skill) = self.strong_trigger(text) {
            return Some(skill);
        }
        if input.current_skill.is_some()
            && matches!(text, "1" | "2" | "3" | "4")
            && input.collected_slots
        {
            return input
                .current_skill
                .as_deref()
                .and_then(|id| self.registry.get(id));
        }

        if rejects_material_search(text) {
            if is_topic_refinement_request(text) {
                return self.registry.get("socratic_review");
            }
            if let Some(current_skill) = input.current_skill.as_deref()
                && current_skill != "material_search"
            {
                return self.registry.get(current_skill);
            }
            return None;
        }

        if let Some(socratic) = self.registry.get("socratic_review") {
            if matches!(
                input.current_skill.as_deref(),
                Some("socratic_review" | "novelty_eval" | "material_search")
            ) && !is_material_search_request(text)
                && (is_material_source_answer(text) || is_topic_refinement_request(text))
            {
                return Some(socratic);
            }
            if input.current_skill.as_deref() == Some("socratic_review")
                && (is_socratic_followup(text)
                    || is_selection_followup(text)
                    || is_socratic_thinking_request(text))
            {
                return Some(socratic);
            }
            if input.current_skill.as_deref() == Some("novelty_eval")
                && (input.route_needs_socratic || has_candidate_paths(input))
                && (is_socratic_followup(text) || is_selection_followup(text))
            {
                return Some(socratic);
            }
            if is_broad_topic_problem(text)
                || is_topic_refinement_request(text)
                || (!is_direct_evaluation_or_theory_request(text)
                    && is_socratic_thinking_request(text))
            {
                return Some(socratic);
            }
        }

        if let Some(alias) = self.alias_match(text) {
            return Some(alias);
        }
        if let Some(current_skill) = input
            .current_skill
            .as_deref()
            .and_then(|id| self.registry.get(id))
            && (keyword_score(text, current_skill) > 0
                || is_short_followup(text)
                || is_selection_followup(text))
        {
            return Some(current_skill);
        }
        self.best_keyword_match(text)
    }

    fn strong_trigger<'a>(&'a self, text: &str) -> Option<&'a SkillDefinition> {
        if has_any(text, &["ppt", "课件", "老师讲过", "这节课"]) {
            return self.registry.get("ppt_qa");
        }
        if is_literature_reading_request(text) {
            return self.registry.get("literature_reading");
        }
        if is_material_search_request(text) {
            return self
                .registry
                .get("material_search")
                .or_else(|| self.registry.get("novelty_eval"));
        }
        for (matches, skill_id) in [
            (is_ai_boundary_request(text), "ai_use_boundary_qa"),
            (is_academic_norm_request(text), "academic_norm_check"),
            (is_course_policy_request(text), "course_policy_qa"),
            (
                is_explicit_method_request(text),
                "method_feasibility_checker",
            ),
            (is_draft_diagnosis_request(text), "draft_diagnosis"),
            (is_theory_fit_request(text), "theory_fit_checker"),
            (
                is_research_question_request(text),
                "research_question_evaluator",
            ),
        ] {
            if matches {
                return self.registry.get(skill_id);
            }
        }
        None
    }

    fn alias_match(&self, text: &str) -> Option<&SkillDefinition> {
        self.registry.all().find(|skill| {
            skill
                .mode_aliases
                .iter()
                .any(|alias| alias.to_lowercase() == text)
        })
    }

    fn best_keyword_match(&self, text: &str) -> Option<&SkillDefinition> {
        self.registry
            .all()
            .filter_map(|skill| {
                let score = keyword_score(text, skill);
                (score > 0).then_some((score, skill))
            })
            .max_by_key(|(score, _)| *score)
            .map(|(_, skill)| skill)
    }
}

fn explicit_switch_payload(message: &str) -> Option<&str> {
    ["切换分支", "切换到"].into_iter().find_map(|marker| {
        message
            .split_once(marker)
            .map(|(_, payload)| payload.trim_start_matches(['：', ':', '，', ',', '。', ' ']))
    })
}

fn build_decision(text: &str, input: &RouteInput, target_skill: Option<String>) -> RouteDecision {
    let stage = stage_for(text, input, target_skill.as_deref());
    let intent = intent_for(text, target_skill.as_deref());
    let risk = risk_for(&stage, &intent, text, input);
    let required_context = required_context_for(&stage, &intent, input);
    let needs_socratic = risk == RiskLevel::NeedsSocratic || !required_context.is_empty();
    let reason = reason_for(&risk, &intent);
    let confidence = (0.55
        + f32::from(target_skill.is_some()) * 0.15
        + f32::from(!stage.is_unknown()) * 0.15
        + f32::from(intent != "continue") * 0.10
        + f32::from(text.chars().count() >= 6) * 0.05)
        .clamp(0.0, 1.0);
    RouteDecision {
        stage,
        intent,
        risk: risk.clone(),
        target_skill,
        confidence,
        reason,
        required_context,
        needs_socratic,
    }
}

fn stage_for(text: &str, input: &RouteInput, target: Option<&str>) -> WritingStage {
    match target {
        Some("research_question_evaluator") => WritingStage::ResearchQuestion,
        Some("theory_fit_checker") => WritingStage::Theory,
        Some("method_feasibility_checker") => WritingStage::Method,
        Some("draft_diagnosis") | Some("writing_feedback") => WritingStage::DraftArgument,
        Some("course_policy_qa") | Some("ai_use_boundary_qa") => WritingStage::CoursePolicy,
        Some("academic_norm_check") => WritingStage::AcademicNorm,
        Some("material_search") | Some("literature_reading") => WritingStage::Literature,
        Some("socratic_review") | Some("novelty_eval") => WritingStage::Topic,
        _ if has_any(text, &["文献", "资料", "搜索", "联网"]) => WritingStage::Literature,
        _ if is_explicit_method_request(text) => WritingStage::Method,
        _ if has_any(text, &["理论", "概念框架", "框架", "硬套"]) => WritingStage::Theory,
        _ if has_any(
            text,
            &[
                "初稿",
                "修改稿",
                "段落",
                "结构",
                "论证",
                "逻辑",
                "反方",
                "证据",
            ],
        ) =>
        {
            WritingStage::DraftArgument
        }
        _ if input.context_has("research_question")
            || has_any(text, &["研究问题", "这个问题", "好不好"]) =>
        {
            WritingStage::ResearchQuestion
        }
        _ => input
            .context_value("stage")
            .and_then(|value| value.as_str())
            .map(stage_from_context)
            .unwrap_or_default(),
    }
}

fn stage_from_context(stage: &str) -> WritingStage {
    match stage {
        "topic" => WritingStage::Topic,
        "literature" => WritingStage::Literature,
        "research_question" => WritingStage::ResearchQuestion,
        "theory" => WritingStage::Theory,
        "method" => WritingStage::Method,
        "draft_argument" => WritingStage::DraftArgument,
        "course_policy" => WritingStage::CoursePolicy,
        "academic_norm" => WritingStage::AcademicNorm,
        other => WritingStage::Unknown(other.to_owned()),
    }
}

fn intent_for(text: &str, target: Option<&str>) -> String {
    if target == Some("material_search") {
        return "material_search".into();
    }
    if has_any(text, &["好不好", "怎么样", "能不能写", "可不可行", "评估"]) {
        return "evaluate".into();
    }
    if has_any(text, &["怎么改", "修改", "诊断", "哪里有问题"]) {
        return "diagnose".into();
    }
    if has_any(text, &["找", "搜索", "联网", "文献", "资料", "引用"]) {
        return "search".into();
    }
    if matches!(text, "1" | "2" | "3" | "4" | "5" | "6")
        || has_any(
            text,
            &["选哪个", "选择", "第二个", "第三个", "就这个", "这个"],
        )
    {
        return "select".into();
    }
    if has_any(text, &["不知道", "没思路", "没灵感", "想写", "想研究"]) {
        return "clarify".into();
    }
    if has_any(text, &["能不能用", "规则", "要求", "隐私", "ai率", "格式"]) {
        return "policy_qa".into();
    }
    "continue".into()
}

fn risk_for(stage: &WritingStage, intent: &str, text: &str, input: &RouteInput) -> RiskLevel {
    if matches!(
        stage,
        WritingStage::CoursePolicy | WritingStage::AcademicNorm
    ) || matches!(intent, "search" | "material_search" | "policy_qa")
    {
        return RiskLevel::DirectOk;
    }
    if matches!(stage, WritingStage::Topic | WritingStage::ResearchQuestion)
        && !input.context_has("motivation")
    {
        return RiskLevel::NeedsSocratic;
    }
    if *stage == WritingStage::Theory && !input.context_has("research_question") {
        return RiskLevel::NeedsContext;
    }
    if *stage == WritingStage::Method
        && !input.context_has("research_question")
        && !has_any(text, &["小组合作", "搭子", "ai"])
    {
        return RiskLevel::NeedsContext;
    }
    if has_any(text, &["帮我写", "直接写", "替我写", "完整"]) {
        return RiskLevel::GhostwritingRisk;
    }
    RiskLevel::DirectOk
}

fn required_context_for(stage: &WritingStage, intent: &str, input: &RouteInput) -> Vec<String> {
    let mut required = Vec::new();
    match stage {
        WritingStage::Topic => {
            for key in ["motivation", "evidence_items"] {
                if !input.context_has(key) {
                    required.push(key.to_owned());
                }
            }
        }
        WritingStage::ResearchQuestion => {
            if !input.context_has("topic") {
                required.push("topic".to_owned());
            }
            if !input.context_has("evidence_items") {
                required.push("material_source".to_owned());
            }
        }
        WritingStage::Theory | WritingStage::Method if !input.context_has("research_question") => {
            required.push("research_question".to_owned());
        }
        _ => {}
    }
    if intent == "select" && !input.context_has("choice_reason") {
        required.push("choice_reason".to_owned());
    }
    required
}

fn reason_for(risk: &RiskLevel, intent: &str) -> String {
    match risk {
        RiskLevel::NeedsSocratic => {
            "学生处在选题或研究问题推进阶段，必须先补动机、观察和材料，不应直接给最终答案。".into()
        }
        RiskLevel::NeedsContext => "当前问题需要先确认研究问题或材料边界，再给结构化建议。".into(),
        RiskLevel::GhostwritingRisk => "请求可能滑向代写，需要转成诊断和修改任务。".into(),
        RiskLevel::DirectOk if matches!(intent, "search" | "material_search") => {
            "学生明确要求查资料，应进入材料检索并区分课程材料、学术库和网页线索。".into()
        }
        RiskLevel::DirectOk => "可以在现有 skill 流程内继续推进。".into(),
    }
}

fn command_target(text: &str) -> Option<&'static str> {
    match text {
        "1" | "/write" => Some("writing_feedback"),
        "2" | "/eval" => Some("novelty_eval"),
        "3" | "/ppt" => Some("ppt_qa"),
        "4" | "/review" => Some("peer_review_training"),
        "/lit" => Some("literature_reading"),
        "/think" => Some("socratic_review"),
        "/search" => Some("material_search"),
        _ => None,
    }
}

fn keyword_score(text: &str, skill: &SkillDefinition) -> usize {
    skill
        .trigger_keywords
        .iter()
        .filter(|keyword| !keyword.is_empty() && text.contains(&keyword.to_lowercase()))
        .count()
}

fn has_candidate_paths(input: &RouteInput) -> bool {
    input.context_has("candidate_paths")
}
fn has_any(text: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| text.contains(term))
}
fn is_short_followup(text: &str) -> bool {
    matches!(
        text,
        "是啊"
            | "对"
            | "对的"
            | "都要"
            | "直接回答"
            | "你不能直接回答吗"
            | "继续"
            | "我现在不太清楚"
            | "不太清楚"
            | "我不太清楚"
            | "不清楚"
            | "还没想清楚"
            | "没想清楚"
            | "没思路"
            | "没灵感"
    )
}
fn is_selection_followup(text: &str) -> bool {
    matches!(
        text,
        "就这个"
            | "这个"
            | "这个题目"
            | "选这个"
            | "就它"
            | "可以"
            | "行"
            | "1"
            | "2"
            | "3"
            | "4"
            | "5"
            | "6"
            | "一"
            | "二"
            | "三"
            | "四"
            | "五"
            | "六"
    ) || has_any(
        text,
        &[
            "第一个",
            "第一",
            "第1",
            "第二个",
            "第二",
            "第2",
            "第三个",
            "第三",
            "第3",
            "第四个",
            "第四",
            "第4",
            "第五个",
            "第五",
            "第5",
            "第六个",
            "第六",
            "第6",
        ],
    )
}
fn is_literature_reading_request(text: &str) -> bool {
    has_any(
        text,
        &[
            "帮我读",
            "读一下",
            "读这篇",
            "看这篇",
            "文献阅读",
            "论文阅读",
        ],
    ) && has_any(
        text,
        &["这篇文献", "这篇论文", "这篇 paper", "摘要", "abstract"],
    )
}
fn is_material_search_request(text: &str) -> bool {
    !rejects_material_search(text)
        && !has_any(text, &["参考文献格式", "引用格式", "文献格式"])
        && (has_any(
            text,
            &[
                "上网",
                "联网",
                "搜索",
                "搜一下",
                "搜文献",
                "找资料",
                "查一下",
                "检索",
                "openalex",
                "open alex",
                "参考文献",
                "有什么文献",
                "哪些文献",
                "有没有文献",
                "推荐文献",
                "列文献",
            ],
        ) || (text.contains("文献")
            && has_any(
                text,
                &["找", "搜", "查", "有没有", "有什么", "推荐", "列", "链接"],
            )))
}

fn rejects_material_search(text: &str) -> bool {
    has_any(
        text,
        &[
            "别给我文献",
            "不要文献",
            "不用文献",
            "不是要文献",
            "不是让你给文献",
            "我不是让你给文献",
            "谁让你给我文献",
            "谁让你找文献",
            "谁让你搜文献",
            "别找文献",
            "不要给我文献",
            "不是问文献",
            "不是要你看文献",
            "不是让你找文献",
            "不是让你搜文献",
            "不是要找资料",
            "不是让你找资料",
            "我问你细化选题",
            "我不是让你细化选题",
        ],
    )
}

fn is_material_source_answer(text: &str) -> bool {
    !is_material_search_request(text)
        && has_any(
            text,
            &[
                "网上的文献",
                "网络文献",
                "已有文献",
                "网上资料",
                "论文",
                "文献",
            ],
        )
        && (text.chars().count() <= 24
            || has_any(
                text,
                &["就是", "主要是", "应该是", "可能是", "我想用", "用", "看"],
            ))
}
fn is_topic_refinement_request(text: &str) -> bool {
    !is_material_search_request(text)
        && has_any(
            text,
            &[
                "细化选题",
                "怎么细化",
                "选题思路",
                "选题方向",
                "具体方向",
                "具体切口",
                "怎么拆",
                "怎么展开",
            ],
        )
}
fn is_socratic_thinking_request(text: &str) -> bool {
    !has_any(text, &["ppt", "课件", "概念", "是什么意思", "怎么理解"])
        && !is_material_search_request(text)
        && has_any(
            text,
            &[
                "没想清楚",
                "想清楚",
                "追问",
                "面批",
                "思路",
                "动机",
                "选哪个",
                "怎么选",
                "文献综述",
                "修改方案",
                "不知道怎么选题",
                "细化",
                "切口",
                "没灵感",
                "没思路",
                "想写",
                "想研究",
                "为什么",
                "导致",
            ],
        )
        && has_any(
            text,
            &[
                "选题",
                "主题",
                "题目",
                "论文",
                "文章",
                "研究",
                "论点",
                "论证",
                "逻辑",
                "证据",
                "反方",
                "细化",
                "切口",
                "修改",
                "初稿",
                "提纲",
                "小组合作",
                "合作",
                "分工",
            ],
        )
}
fn is_socratic_followup(text: &str) -> bool {
    has_any(
        text,
        &[
            "因为",
            "我观察到",
            "材料",
            "访谈",
            "例子",
            "案例",
            "证据",
            "我选",
            "选择",
            "没选",
            "不选",
            "更适合",
            "反例",
            "反方",
            "可以总结",
            "总结一下",
            "换成",
            "改成",
            "不是",
            "当然是",
            "不好意思",
            "不敢催",
            "不干活",
            "拖了进度",
            "替他",
            "默认选项",
            "好写",
            "细化",
            "切口",
        ],
    )
}
fn is_broad_topic_problem(text: &str) -> bool {
    !is_material_search_request(text)
        && !has_any(
            text,
            &[
                "评估",
                "新颖",
                "创新",
                "可写性",
                "课程匹配",
                "理论",
                "理论入口",
                "理论框架",
                "没有理论",
                "可能的理论",
            ],
        )
        && has_any(
            text,
            &[
                "我有选题问题",
                "有选题问题",
                "选题问题",
                "不知道怎么选题",
                "选题卡住",
                "我想讨论",
                "想讨论",
                "我想研究",
                "想研究",
                "我想写",
                "想写",
            ],
        )
        && has_any(
            text,
            &[
                "选题",
                "小组合作",
                "小组分工",
                "分工不均",
                "搭子",
                "朋友",
                "逻辑",
            ],
        )
}
fn is_research_question_request(text: &str) -> bool {
    has_any(
        text,
        &[
            "研究问题",
            "问题意识",
            "这个问题好不好",
            "这个题好不好",
            "能不能研究",
            "研究问题怎么样",
            "问题怎么改",
        ],
    )
}
fn is_theory_fit_request(text: &str) -> bool {
    !has_any(text, &["没有理论", "给我一些可能的理论", "理论入口"])
        && has_any(
            text,
            &[
                "理论框架",
                "概念框架",
                "硬套",
                "堆理论",
                "理论适配",
                "理论是不是",
            ],
        )
}
fn is_explicit_method_request(text: &str) -> bool {
    has_any(text, &["反向因果", "变量", "题项"])
        || (has_any(text, &["问卷", "访谈", "样本", "研究方法", "因果", "数据"])
            && has_any(
                text,
                &[
                    "怎么", "如何", "设计", "可行", "对象", "多少", "分析", "处理",
                ],
            ))
}
fn is_draft_diagnosis_request(text: &str) -> bool {
    has_any(text, &["初稿", "修改稿", "草稿", "段落"])
        && has_any(
            text,
            &[
                "诊断",
                "修改",
                "怎么改",
                "哪里有问题",
                "逻辑",
                "结构",
                "论证",
            ],
        )
}
fn is_course_policy_request(text: &str) -> bool {
    has_any(
        text,
        &[
            "课程规则",
            "作业要求",
            "字数",
            "截止",
            "评分标准",
            "格式要求",
        ],
    )
}
fn is_ai_boundary_request(text: &str) -> bool {
    has_any(
        text,
        &[
            "ai率",
            "ai 率",
            "ai使用",
            "用ai",
            "用 ai",
            "chatgpt",
            "生成式ai",
            "生成式 ai",
            "检测",
            "降低ai痕迹",
            "隐私",
            "记录对话",
            "学术诚信",
        ],
    )
}
fn is_academic_norm_request(text: &str) -> bool {
    has_any(
        text,
        &[
            "学术规范",
            "引用",
            "参考文献格式",
            "doi",
            "来源可靠",
            "文献真假",
            "观点是不是原文",
            "抄袭",
            "查重",
        ],
    )
}

fn is_direct_evaluation_or_theory_request(text: &str) -> bool {
    has_any(
        text,
        &[
            "评估",
            "新颖",
            "创新",
            "可写性",
            "课程匹配",
            "有没有意思",
            "值不值得写",
            "理论",
            "理论入口",
            "理论框架",
            "没有理论",
            "可能的理论",
        ],
    )
}
