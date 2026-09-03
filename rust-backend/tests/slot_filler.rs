use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use writing_coach_server::{
    domain::SessionStateData,
    skills::{SkillRegistry, SlotFiller},
};

fn skills_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("project root")
        .join("skills")
}

fn fill(skill_id: &str, state: Value, message: &str) -> Map<String, Value> {
    let registry = SkillRegistry::load(&skills_root()).expect("skills load");
    let state = SessionStateData::from_legacy_json(state).expect("state parses");
    SlotFiller::new().fill(registry.get(skill_id).unwrap(), &state, message)
}

#[test]
fn awaiting_slot_consumes_the_next_message_without_length_heuristics() {
    let slots = fill(
        "novelty_eval",
        json!({"awaiting_slots":["target_text"], "collected_slots":{}}),
        "AI与写作",
    );
    assert_eq!(slots["target_text"], json!("AI与写作"));
}

#[test]
fn novelty_filler_matches_inspiration_theory_and_selection_rules() {
    let inspiration = fill("novelty_eval", json!({}), "我没有啥灵感");
    assert_eq!(inspiration["target_text"], json!("我没有啥灵感"));
    assert_eq!(inspiration["evaluation_goal"], json!("选题灵感与方向发散"));

    let theory = "我现在要研究搭子与朋友的关系但是我没有理论，你给我一些可能的理论";
    let theory_slots = fill("novelty_eval", json!({}), theory);
    assert_eq!(theory_slots["target_text"], json!(theory));
    assert_eq!(theory_slots["evaluation_goal"], json!(theory));

    let selected = fill(
        "novelty_eval",
        json!({"collected_slots":{"target_text":"朋友和搭子"}}),
        "我选择方向二",
    );
    assert_eq!(selected["selected_option"], json!("2"));
    assert_eq!(selected["followup_goal"], json!("我选择方向二"));
}

#[test]
fn socratic_filler_only_accepts_a_selection_when_candidates_exist() {
    let without = fill("socratic_review", json!({}), "第二个");
    assert!(without.get("selected_option").is_none());

    let with = fill(
        "socratic_review",
        json!({
            "collected_slots":{"target_text":"我想写朋友和搭子"},
            "writing_context":{"candidate_paths":[{"index":"2","title":"理论脉络"}]}
        }),
        "第二个",
    );
    assert_eq!(with["thinking_task"], json!("选题"));
    assert_eq!(with["selected_option"], json!("2"));
    assert_eq!(with["initial_idea"], json!("我想写朋友和搭子"));
}

#[test]
fn literature_filler_accepts_structured_source_text() {
    let message = "帮我读一下这篇文献：摘要：本文研究大学生搭子关系与朋友关系的边界。";
    let slots = fill("literature_reading", json!({}), message);
    assert_eq!(slots["source_text"], json!(message));
}
