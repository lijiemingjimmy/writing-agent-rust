use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{domain::SessionStateData, llm::ModelMessage};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct KnowledgeDecision {
    pub use_course_corpus: bool,
    pub use_external_search: bool,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub reason: String,
    #[serde(skip)]
    pub decider: String,
}

impl KnowledgeDecision {
    pub fn fallback(skill_id: &str, message: &str, web_enabled: bool) -> Self {
        if rejects_material_search(message) {
            return Self {
                use_course_corpus: false,
                use_external_search: false,
                query: String::new(),
                reason: "用户明确拒绝资料检索".to_owned(),
                decider: "deterministic_fallback".to_owned(),
            };
        }
        let explicit_external = is_material_search_request(message);
        let use_course_corpus = explicit_external
            || matches!(
                skill_id,
                "novelty_eval"
                    | "ppt_qa"
                    | "research_question_evaluator"
                    | "theory_fit_checker"
                    | "method_feasibility_checker"
                    | "course_policy_qa"
                    | "academic_norm_check"
            );
        Self {
            use_course_corpus,
            use_external_search: explicit_external && web_enabled,
            query: message.trim().to_owned(),
            reason: "模型决策不可用，使用与 Python 一致的确定性回退".to_owned(),
            decider: "deterministic_fallback".to_owned(),
        }
    }

    pub fn parse(raw: &str, web_enabled: bool) -> Option<Self> {
        let cleaned = raw
            .trim()
            .strip_prefix("```json")
            .or_else(|| raw.trim().strip_prefix("```"))
            .unwrap_or(raw.trim())
            .trim_end_matches("```")
            .trim();
        let value: Value = serde_json::from_str(cleaned).ok()?;
        Some(Self {
            use_course_corpus: value.get("use_course_corpus")?.as_bool()?,
            use_external_search: value.get("use_external_search")?.as_bool()? && web_enabled,
            query: value
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned(),
            reason: value
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned(),
            decider: "llm".to_owned(),
        })
    }
}

pub fn build_knowledge_decision_prompt(
    skill_id: &str,
    message: &str,
    state: &SessionStateData,
) -> Vec<ModelMessage> {
    let summary = state
        .writing_context
        .context_summary
        .as_deref()
        .unwrap_or("暂无");
    vec![
        ModelMessage::system(
            "你只判断本轮回答是否需要检索资料，不负责回答学生问题。不要为了关键词自动查库。只输出一个 JSON 对象，不要 Markdown。字段必须为 use_course_corpus:boolean、use_external_search:boolean、query:string、reason:string。课程概念、课件、规范或需要理论依据时才查课程语料；只有学生明确要求查找、检索、联网、文献或资料时才允许外部检索；苏格拉底式澄清通常不检索。",
        ),
        ModelMessage::user(format!(
            "[UNTRUSTED_INPUT]\nskill_id={skill_id}\ncontext_summary={summary}\nlatest_message={message}\n[/UNTRUSTED_INPUT]"
        )),
    ]
}

fn is_material_search_request(message: &str) -> bool {
    !rejects_material_search(message)
        && ([
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
            "给我文献",
            "推荐文献",
            "列文献",
        ]
        .iter()
        .any(|term| message.to_lowercase().contains(term))
            || (message.contains("文献")
                && ["找", "搜", "查", "有没有", "有什么", "推荐", "列", "链接"]
                    .iter()
                    .any(|term| message.contains(term))))
}

fn rejects_material_search(message: &str) -> bool {
    [
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
    ]
    .iter()
    .any(|term| message.contains(term))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_json_and_enforces_web_consent() {
        let parsed = KnowledgeDecision::parse(
            "```json\n{\"use_course_corpus\":false,\"use_external_search\":true,\"query\":\"x\",\"reason\":\"explicit\"}\n```",
            false,
        )
        .unwrap();
        assert!(!parsed.use_course_corpus);
        assert!(!parsed.use_external_search);
        assert_eq!(parsed.decider, "llm");
    }

    #[test]
    fn fallback_does_not_search_merely_because_socratic_text_mentions_materials() {
        let decision = KnowledgeDecision::fallback(
            "socratic_review",
            "因为我观察到小组作业有人不做事，材料可以访谈同学",
            true,
        );
        assert!(!decision.use_course_corpus);
        assert!(!decision.use_external_search);
    }
}
