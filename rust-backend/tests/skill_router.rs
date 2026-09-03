use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::json;
use writing_coach_server::{
    domain::RouteInput,
    skills::{SkillRegistry, SkillRouter},
};

fn skills_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rust-backend has a project root")
        .join("skills")
}

fn load_router() -> SkillRouter {
    SkillRouter::new(SkillRegistry::load(&skills_root()).expect("real skills corpus loads"))
}

fn temporary_skills_dir() -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("writing-coach-skills-{unique}"));
    fs::create_dir_all(root.join("skills")).expect("temporary skills directory is created");
    root.join("skills")
}

fn write_skill(skills_root: &Path, name: &str, yaml: &str) {
    fs::write(skills_root.join(name), yaml).expect("temporary YAML is written");
}

const MINIMAL_SKILL: &str = r#"
id: example
name: Example
description: Example skill
mode_aliases: [example]
trigger_keywords: [example]
required_slots: []
slot_questions: {}
corpus_paths: []
"#;

#[test]
fn loads_the_real_yaml_corpus_without_losing_unknown_fields() {
    let registry = SkillRegistry::load(&skills_root()).expect("real skills corpus loads");

    assert_eq!(registry.all().len(), 14);
    let ppt = registry.get("ppt_qa").expect("ppt skill is registered");
    assert_eq!(ppt.extra.get("stay_in_mode"), Some(&json!(true)));
    assert!(ppt.extra.contains_key("answer_policy"));
}

#[test]
fn rejects_duplicate_skill_ids() {
    let skills_root = temporary_skills_dir();
    write_skill(&skills_root, "one.yaml", MINIMAL_SKILL);
    write_skill(&skills_root, "two.yaml", MINIMAL_SKILL);

    let result = SkillRegistry::load(&skills_root);

    assert!(result.is_err());
    let _ = fs::remove_dir_all(skills_root.parent().expect("temporary project root"));
}

#[test]
fn rejects_empty_triggers_and_missing_slot_questions() {
    let skills_root = temporary_skills_dir();
    write_skill(
        &skills_root,
        "invalid.yaml",
        r#"
id: invalid
name: Invalid
description: Invalid skill
mode_aliases: []
trigger_keywords: []
required_slots: [question]
slot_questions: {}
corpus_paths: []
"#,
    );

    let result = SkillRegistry::load(&skills_root);

    assert!(result.is_err());
    let _ = fs::remove_dir_all(skills_root.parent().expect("temporary project root"));
}

#[test]
fn rejects_corpus_paths_that_escape_the_project_root() {
    let skills_root = temporary_skills_dir();
    write_skill(
        &skills_root,
        "invalid.yaml",
        r#"
id: invalid
name: Invalid
description: Invalid skill
mode_aliases: [invalid]
trigger_keywords: [invalid]
required_slots: []
slot_questions: {}
corpus_paths: [../outside.md]
"#,
    );

    let result = SkillRegistry::load(&skills_root);

    assert!(result.is_err());
    let _ = fs::remove_dir_all(skills_root.parent().expect("temporary project root"));
}

#[test]
fn routes_topic_uncertainty_to_socratic_review() {
    let decision = load_router().route(&RouteInput::new("我有几个选题方向，不知道该选哪个"));

    assert_eq!(decision.target_skill.as_deref(), Some("socratic_review"));
    assert!(decision.needs_socratic);
}

#[test]
fn active_skill_is_sticky_without_an_explicit_switch() {
    let input = RouteInput::new("PPT 里扎根理论是什么意思？")
        .with_current_skill("socratic_review")
        .with_writing_context(json!({"stage": "topic", "motivation": "课堂观察"}));
    let decision = load_router().route(&input);

    assert_eq!(decision.target_skill.as_deref(), Some("socratic_review"));
}

#[test]
fn explicit_switch_can_replace_the_active_skill() {
    let input = RouteInput::new("切换分支：/ppt")
        .with_current_skill("socratic_review")
        .with_writing_context(json!({"stage": "topic", "motivation": "课堂观察"}));
    let decision = load_router().route(&input);

    assert_eq!(decision.target_skill.as_deref(), Some("ppt_qa"));
}

#[test]
fn routes_draft_diagnosis_request() {
    let decision = load_router().route(&RouteInput::new("这段初稿的论证逻辑哪里有问题，怎么改？"));

    assert_eq!(decision.target_skill.as_deref(), Some("draft_diagnosis"));
    assert!(!decision.needs_socratic);
}

#[test]
fn routes_literature_reading_request() {
    let decision = load_router().route(&RouteInput::new("帮我读这篇论文的摘要"));

    assert_eq!(decision.target_skill.as_deref(), Some("literature_reading"));
}

#[test]
fn routes_material_search_request() {
    let decision = load_router().route(&RouteInput::new("帮我搜索有关小组合作的文献"));

    assert_eq!(decision.target_skill.as_deref(), Some("material_search"));
}

#[test]
fn routes_theory_fit_request() {
    let decision = load_router().route(&RouteInput::new("这个理论框架是不是在硬套？"));

    assert_eq!(decision.target_skill.as_deref(), Some("theory_fit_checker"));
    assert!(decision.needs_socratic);
}

#[test]
fn routes_method_feasibility_request() {
    let decision = load_router().route(&RouteInput::new("问卷样本要多少才可行？"));

    assert_eq!(
        decision.target_skill.as_deref(),
        Some("method_feasibility_checker")
    );
    assert!(decision.needs_socratic);
}

#[test]
fn routes_ai_policy_risk_request() {
    let decision = load_router().route(&RouteInput::new("用AI写作会不会影响学术诚信？"));

    assert_eq!(decision.target_skill.as_deref(), Some("ai_use_boundary_qa"));
    assert!(!decision.needs_socratic);
}

#[test]
fn routes_contextual_follow_up_after_socratic_review() {
    let input = RouteInput::new("就这个")
        .with_current_skill("socratic_review")
        .with_writing_context(json!({"stage": "topic", "motivation": "小组观察"}));
    let decision = load_router().route(&input);

    assert_eq!(decision.target_skill.as_deref(), Some("socratic_review"));
    assert!(decision.needs_socratic);
}

#[test]
fn numeric_command_is_a_socratic_selection_when_context_is_active() {
    let router = load_router();
    let decision = router.route(
        &RouteInput::new("2")
            .with_current_skill("socratic_review")
            .with_collected_slots(true)
            .with_writing_context(json!({"candidate_paths":[{"index":"2","title":"机制解释"}]})),
    );

    assert_eq!(decision.target_skill.as_deref(), Some("socratic_review"));
    assert_eq!(decision.intent, "select");
}

#[test]
fn routes_the_complete_python_high_frequency_contract() {
    let router = load_router();
    for (message, expected) in [
        ("我不知道写啥", "novelty_eval"),
        ("我没有啥灵感", "novelty_eval"),
        (
            "这个研究问题好不好：小组合作为什么分工不均？",
            "research_question_evaluator",
        ),
        ("我这个理论是不是硬套？", "theory_fit_checker"),
        ("访谈对象怎么设计比较可行？", "method_feasibility_checker"),
        ("这段初稿哪里有问题，怎么改？", "draft_diagnosis"),
        ("作业字数和格式要求是什么？", "course_policy_qa"),
        ("AI率太高怎么办，能不能用 ChatGPT？", "ai_use_boundary_qa"),
        ("这个引用格式和 DOI 可靠吗？", "academic_norm_check"),
    ] {
        let decision = router.route(&RouteInput::new(message));
        assert_eq!(
            decision.target_skill.as_deref(),
            Some(expected),
            "{message}"
        );
    }
}

#[test]
fn high_frequency_routes_precede_generic_ppt_keyword_matching() {
    let router = load_router();
    let research = router.route(&RouteInput::new("PPT 里如何定义研究问题？"));
    assert_eq!(
        research.target_skill.as_deref(),
        Some("research_question_evaluator")
    );

    let courseware = router.route(&RouteInput::new("老师讲过 audience awareness 吗？"));
    assert_eq!(courseware.target_skill.as_deref(), Some("ppt_qa"));
}

#[test]
fn explicit_material_source_and_rejection_follow_python_branch_rules() {
    let router = load_router();
    let context = json!({"topic":"朋友和搭子", "thinking_task":"选题", "stage":"topic"});

    let source = router.route(
        &RouteInput::new("就是网上的文献")
            .with_current_skill("socratic_review")
            .with_writing_context(context.clone()),
    );
    assert_eq!(source.target_skill.as_deref(), Some("socratic_review"));

    let rejected = router.route(
        &RouteInput::new("我问你细化选题，谁让你给我文献了")
            .with_current_skill("material_search")
            .with_writing_context(context),
    );
    assert_eq!(rejected.target_skill.as_deref(), Some("material_search"));
}

#[test]
fn route_stage_matches_python_context_sensitive_socratic_contract() {
    let router = load_router();
    let decision = router.route(
        &RouteInput::new("我想整理文章结构")
            .with_current_skill("socratic_review")
            .with_writing_context(json!({"thinking_task":"论证结构", "stage":"draft_argument"})),
    );

    assert_eq!(decision.target_skill.as_deref(), Some("socratic_review"));
    assert_eq!(decision.stage.as_str(), "draft_argument");
}

#[test]
fn awaited_slot_reply_stays_in_current_skill_even_when_it_contains_a_strong_trigger() {
    // Break caught: slot text such as "PPT 里要求 1500 字" starts a new PPT task instead of
    // satisfying the writing-feedback assignment requirement.
    let router = load_router();
    let input = RouteInput::new("PPT 里要求不少于 1500 字")
        .with_current_skill("writing_feedback")
        .with_awaiting_slots(["assignment_requirement"]);

    let decision = router.route(&input);

    assert_eq!(decision.target_skill.as_deref(), Some("writing_feedback"));
}

#[test]
fn routes_missing_theory_topic_to_novelty_evaluation_instead_of_socratic_review() {
    let decision = load_router().route(&RouteInput::new(
        "我想研究搭子与朋友的关系，但是现在没有理论。",
    ));

    assert_eq!(decision.target_skill.as_deref(), Some("novelty_eval"));
}

#[test]
fn material_search_rejection_does_not_fall_back_to_material_search() {
    let decision = load_router().route(&RouteInput::new("我不是让你给文献"));

    assert_eq!(decision.target_skill, None);
}

#[test]
fn awaiting_material_search_slot_keeps_contextual_continuation_on_rejection() {
    let input = RouteInput::new("我不是让你给文献")
        .with_current_skill("material_search")
        .with_awaiting_slots(["research_topic"]);
    let decision = load_router().route(&input);

    assert_eq!(decision.target_skill.as_deref(), Some("material_search"));
}

#[test]
fn material_source_answer_stays_in_material_search_until_explicit_switch() {
    let input = RouteInput::new("我想用已有文献")
        .with_current_skill("material_search")
        .with_writing_context(json!({"stage": "topic", "motivation": "小组观察"}));
    let decision = load_router().route(&input);

    assert_eq!(decision.target_skill.as_deref(), Some("material_search"));
}

#[test]
fn clamps_route_confidence() {
    let decision = load_router().route(&RouteInput::new("/ppt"));

    assert!((0.0..=1.0).contains(&decision.confidence));
}
