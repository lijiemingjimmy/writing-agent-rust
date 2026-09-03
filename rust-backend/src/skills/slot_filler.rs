use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::{domain::SessionStateData, skills::SkillDefinition};

const MODE_COMMANDS: &[&str] = &[
    "1", "2", "3", "4", "/write", "/eval", "/ppt", "/review", "/think", "/search",
];

#[derive(Clone, Debug, Default)]
pub struct SlotFiller;

impl SlotFiller {
    pub fn new() -> Self {
        Self
    }

    pub fn fill(
        &self,
        skill: &SkillDefinition,
        state: &SessionStateData,
        message: &str,
    ) -> Map<String, Value> {
        let mut collected = state
            .extra
            .get("collected_slots")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let awaiting = state
            .extra
            .get("awaiting_slots")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        let normalized = message.trim().to_lowercase();

        if MODE_COMMANDS.contains(&normalized.as_str()) {
            if skill.id == "novelty_eval" && !collected.is_empty() {
                fill_novelty_eval(&mut collected, message);
            }
            return collected;
        }
        if let Some(slot) = awaiting.first() {
            collected.insert((*slot).to_owned(), string(message.trim()));
            return collected;
        }

        match skill.id.as_str() {
            "writing_feedback" => fill_writing_feedback(&mut collected, message),
            "novelty_eval" => fill_novelty_eval(&mut collected, message),
            "ppt_qa" => insert(&mut collected, "question", message.trim()),
            "peer_review_training" => fill_peer_review(&mut collected, message),
            "literature_reading" => fill_literature_reading(&mut collected, message),
            "socratic_review" => fill_socratic_review(&mut collected, message, state),
            _ => {}
        }
        collected
    }

    pub fn missing_slots(
        &self,
        skill: &SkillDefinition,
        collected: &Map<String, Value>,
    ) -> Vec<String> {
        skill
            .required_slots
            .iter()
            .filter(|slot| !has_value(collected, slot))
            .cloned()
            .collect()
    }

    pub fn next_question(&self, skill: &SkillDefinition, missing: &[String]) -> String {
        missing
            .first()
            .and_then(|slot| {
                skill
                    .slot_questions
                    .as_ref()
                    .and_then(|questions| questions.get(slot))
                    .cloned()
                    .or_else(|| Some(format!("请补充 {slot}。")))
            })
            .unwrap_or_default()
    }
}

fn fill_writing_feedback(collected: &mut Map<String, Value>, message: &str) {
    if matches_regex(message, r"(作业要求|题目|要求)[:：是为\s]") {
        insert(collected, "assignment_requirement", message.trim());
    }
    if contains_any(message, &["核心观点", "中心论点", "我想表达", "我想说明"]) {
        insert(collected, "core_argument", message.trim());
    }
    if contains_any(message, &["逻辑", "结构", "表达", "润色", "论证", "证据"]) {
        insert(collected, "feedback_goal", message.trim());
    }
    if message.trim().chars().count() >= 80 {
        collected
            .entry("draft_text".to_owned())
            .or_insert_with(|| string(message.trim()));
    }
}

fn fill_novelty_eval(collected: &mut Map<String, Value>, message: &str) {
    let stripped = message.trim();
    if let Some(selected) = selected_option(stripped) {
        insert(collected, "followup_goal", stripped);
        insert(collected, "selected_option", &selected);
        return;
    }
    if is_inspiration_request(stripped) {
        if has_value(collected, "target_text") {
            insert(collected, "followup_goal", stripped);
        } else {
            insert(collected, "target_text", stripped);
        }
        collected
            .entry("evaluation_goal".to_owned())
            .or_insert_with(|| string("选题灵感与方向发散"));
        return;
    }

    let literature = contains_any(
        &stripped.to_lowercase(),
        &[
            "文献",
            "参考文献",
            "论文",
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
            "literature",
            "citation",
        ],
    );
    let evaluation = contains_any(
        stripped,
        &[
            "新颖性",
            "创新",
            "可写性",
            "课程匹配",
            "论证潜力",
            "评估",
            "研究",
            "选题",
            "理论",
            "理论入口",
            "理论框架",
            "可能的理论",
            "没有理论",
            "文献",
            "参考文献",
            "论文",
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
            "主题",
            "灵感",
            "怎么写",
            "写什么",
            "不知道",
            "细化",
            "方向",
            "都要",
        ],
    ) || contains_any(
        &stripped.to_lowercase(),
        &["openalex", "open alex", "literature", "citation"],
    );
    let topic_signal = has_topic_signal_for_novelty(stripped);
    if evaluation {
        if has_value(collected, "evaluation_goal") {
            insert(collected, "followup_goal", stripped);
        } else {
            insert(collected, "evaluation_goal", stripped);
        }
    }
    if literature
        && !has_value(collected, "target_text")
        && (topic_signal || stripped.chars().count() >= 6)
    {
        insert(collected, "target_text", stripped);
    }
    if !literature && (stripped.chars().count() >= 12 || topic_signal) {
        insert(collected, "target_text", stripped);
    }
}

fn fill_peer_review(collected: &mut Map<String, Value>, message: &str) {
    if contains_any(
        message,
        &["互评", "反馈", "评价", "结构", "逻辑", "证据", "表达"],
    ) {
        collected
            .entry("review_goal".to_owned())
            .or_insert_with(|| string(message.trim()));
    }
    if message.trim().chars().count() >= 40 {
        collected
            .entry("target_text".to_owned())
            .or_insert_with(|| string(message.trim()));
    }
}

fn fill_literature_reading(collected: &mut Map<String, Value>, message: &str) {
    let stripped = message.trim();
    if !stripped.is_empty()
        && (contains_any(
            &stripped.to_lowercase(),
            &[
                "标题", "题名", "摘要", "abstract", "本文", "这篇", "paper", "论文", "文献",
            ],
        ) || stripped.chars().count() >= 40)
    {
        insert(collected, "source_text", stripped);
    }
}

fn fill_socratic_review(
    collected: &mut Map<String, Value>,
    message: &str,
    state: &SessionStateData,
) {
    let stripped = message.trim();
    if stripped.is_empty() {
        return;
    }
    let selected = selected_option(stripped);
    let has_candidates = !state.writing_context.candidate_paths.is_empty()
        || collected
            .get("candidate_paths")
            .and_then(Value::as_array)
            .is_some_and(|paths| !paths.is_empty());
    let task = thinking_task(stripped).or_else(|| {
        collected
            .get("target_text")
            .and_then(Value::as_str)
            .map(|text| thinking_task(text).unwrap_or_else(|| "选题".to_owned()))
    });
    if let Some(task) = task {
        insert(collected, "thinking_task", &task);
    } else if !has_value(collected, "thinking_task") && selected.is_some() && has_candidates {
        insert(collected, "thinking_task", "选题");
    }
    if let Some(selected) = selected.filter(|_| has_candidates) {
        insert(collected, "selected_option", &selected);
        insert(collected, "followup_goal", stripped);
        if !has_value(collected, "initial_idea")
            && let Some(target) = collected
                .get("target_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        {
            insert(collected, "initial_idea", &target);
        }
    } else if !has_value(collected, "initial_idea")
        || (is_vague_initial_idea(collected.get("initial_idea")) && has_topic_signal(stripped))
    {
        insert(collected, "initial_idea", stripped);
    } else if contains_any(stripped, &["因为", "选择", "我选", "没选", "不选"]) {
        insert(collected, "choice_reason", stripped);
    } else {
        insert(collected, "followup_goal", stripped);
    }
}

pub(crate) fn selected_option(text: &str) -> Option<String> {
    let mut lowered = text.trim().to_lowercase();
    while let Some(value) = ["了", "吧"]
        .iter()
        .find_map(|suffix| lowered.strip_suffix(suffix))
    {
        lowered = value.trim().to_owned();
    }
    for (phrase, value) in [
        ("1", "1"),
        ("2", "2"),
        ("3", "3"),
        ("4", "4"),
        ("5", "5"),
        ("6", "6"),
        ("第一个", "1"),
        ("第一", "1"),
        ("第1个", "1"),
        ("第1", "1"),
        ("第二个", "2"),
        ("第二", "2"),
        ("第2个", "2"),
        ("第2", "2"),
        ("第三个", "3"),
        ("第三", "3"),
        ("第3个", "3"),
        ("第3", "3"),
        ("第四个", "4"),
        ("第四", "4"),
        ("第4个", "4"),
        ("第4", "4"),
        ("第五个", "5"),
        ("第五", "5"),
        ("第5个", "5"),
        ("第5", "5"),
        ("第六个", "6"),
        ("第六", "6"),
        ("第6个", "6"),
        ("第6", "6"),
        ("方向一", "1"),
        ("方向二", "2"),
        ("方向三", "3"),
        ("方向四", "4"),
        ("方向五", "5"),
        ("方向六", "6"),
    ] {
        if lowered == phrase || (phrase.chars().count() > 1 && lowered.contains(phrase)) {
            return Some(value.to_owned());
        }
    }
    selection_regex()
        .captures(&lowered)
        .and_then(|capture| capture.get(1).or_else(|| capture.get(2)))
        .map(|value| match value.as_str() {
            "一" => "1",
            "二" => "2",
            "三" => "3",
            "四" => "4",
            "五" => "5",
            "六" => "6",
            digit => digit,
        })
        .map(str::to_owned)
}

fn selection_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?:我)?(?:选|选择|就|要|想选)\s*(?:第|方向)?\s*(?:([1-6])|([一二三四五六]))\s*(?:个|号|方向|题目)?")
            .expect("selection regex")
    })
}

fn matches_regex(text: &str, pattern: &str) -> bool {
    Regex::new(pattern).expect("constant regex").is_match(text)
}

fn thinking_task(text: &str) -> Option<String> {
    for (markers, task) in [
        (&["文献综述", "综述"][..], "文献综述"),
        (&["修改方案", "怎么改", "先改", "修改优先"][..], "修改方案"),
        (
            &["论证", "结构", "论点", "证据", "反方", "逻辑"][..],
            "论证结构",
        ),
        (
            &[
                "选题",
                "题目",
                "想写",
                "研究",
                "方向",
                "为什么值得写",
                "小组合作",
                "分工",
                "搭子",
                "朋友",
            ][..],
            "选题",
        ),
    ] {
        if contains_any(text, markers) {
            return Some(task.to_owned());
        }
    }
    None
}

fn is_inspiration_request(text: &str) -> bool {
    contains_any(
        text,
        &[
            "没灵感",
            "没有灵感",
            "没啥灵感",
            "没有啥灵感",
            "没什么灵感",
            "没有什么灵感",
            "没想法",
            "没有想法",
            "不知道写啥",
            "不知道写什么",
            "不知道选什么",
            "不知道选题",
            "没思路",
            "没有思路",
            "想不到题",
            "不知道从哪开始",
        ],
    )
}

fn has_topic_signal_for_novelty(text: &str) -> bool {
    contains_any(
        text,
        &[
            "想写",
            "要写",
            "准备写",
            "有关",
            "关于",
            "搭子",
            "朋友",
            "主题",
            "选题",
        ],
    )
}

fn has_topic_signal(text: &str) -> bool {
    contains_any(
        text,
        &[
            "想研究",
            "研究有关",
            "研究关于",
            "小组合作",
            "分工",
            "搭子",
            "朋友",
            "AI",
            "人工智能",
        ],
    )
}

fn is_vague_initial_idea(value: Option<&Value>) -> bool {
    value.and_then(Value::as_str).is_some_and(|text| {
        let text = text.trim();
        matches!(text, "我想写一个逻辑" | "想写一个逻辑" | "逻辑")
            || (text.chars().count() <= 12 && text.contains("逻辑"))
    })
}

fn has_value(values: &Map<String, Value>, key: &str) -> bool {
    values.get(key).is_some_and(|value| match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(value) => !value.is_empty(),
        _ => true,
    })
}

fn insert(values: &mut Map<String, Value>, key: &str, value: &str) {
    values.insert(key.to_owned(), string(value));
}

fn string(value: &str) -> Value {
    Value::String(value.to_owned())
}

fn contains_any(text: &str, markers: &[&str]) -> bool {
    markers.iter().any(|marker| text.contains(marker))
}
