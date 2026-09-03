use serde_json::json;
use writing_coach_server::{
    domain::{FlowStage, SessionStateData, WritingContext, WritingStage},
    skills::{GroundingGuard, GuardPolicy, PromptKind, ThinkingFlowController},
    tools::{KnowledgeBundle, SearchHit},
};

#[test]
fn unknown_legacy_fields_survive_round_trip() {
    let input = json!({
        "task_type": "topic_selection",
        "future_top_level": {"enabled": true},
        "writing_context": {
            "topic": "搭子社交",
            "stage": "topic",
            "future_field": 42
        }
    });

    let state = SessionStateData::from_legacy_json(input.clone()).unwrap();

    assert_eq!(state.writing_context.topic.as_deref(), Some("搭子社交"));
    assert_eq!(state.writing_context.stage, WritingStage::Topic);
    assert_eq!(state.writing_context.extra["future_field"], json!(42));
    assert_eq!(state.extra["future_top_level"], json!({"enabled": true}));
    assert_eq!(serde_json::to_value(state).unwrap(), input);
}

#[test]
fn unknown_stage_strings_round_trip_instead_of_failing() {
    let input = json!({"writing_context": {"stage": "future_peer_review"}});

    let state = SessionStateData::from_legacy_json(input.clone()).unwrap();

    assert_eq!(
        state.writing_context.stage,
        WritingStage::Unknown("future_peer_review".to_owned())
    );
    assert_eq!(serde_json::to_value(state).unwrap(), input);
}

#[test]
fn explicit_legacy_unknown_stage_value_is_not_mistaken_for_an_absent_stage() {
    let input = json!({"writing_context": {"stage": "unknown"}});

    let state = SessionStateData::from_legacy_json(input.clone()).unwrap();

    assert_eq!(
        state.writing_context.stage,
        WritingStage::Unknown("unknown".to_owned())
    );
    assert_eq!(serde_json::to_value(state).unwrap(), input);
}

#[test]
fn programmatically_assigned_known_stage_is_serialized() {
    let mut context = WritingContext::default();
    context.stage = WritingStage::Theory;
    let state = SessionStateData {
        writing_context: context,
        ..SessionStateData::default()
    };

    assert_eq!(
        serde_json::to_value(state).unwrap(),
        json!({"writing_context": {"stage": "theory"}})
    );
}

#[test]
fn null_legacy_collections_normalize_to_empty_collections() {
    let context = WritingContext::from_legacy_json(json!({
        "candidate_paths": null,
        "evidence_items": null,
        "counterexamples": null
    }))
    .unwrap();

    assert!(context.candidate_paths.is_empty());
    assert!(context.evidence.is_empty());
    assert!(context.counterexamples.is_empty());
}

#[test]
fn topic_rules_extract_explicit_writing_and_research_statements() {
    for (message, expected) in [
        ("我想写搭子社交。", "搭子社交"),
        (
            "准备研究宿舍夜话如何影响同伴关系！",
            "宿舍夜话如何影响同伴关系",
        ),
    ] {
        let mut context = WritingContext::default();

        context.apply_user_message(message);

        assert_eq!(context.topic.as_deref(), Some(expected));
    }
}

#[test]
fn topic_change_clears_all_dependent_argument_fields() {
    let mut context = WritingContext::from_legacy_json(json!({
        "topic": "旧主题",
        "stage": "topic",
        "selected_direction": "旧方向",
        "selected_path_id": "2",
        "selected_path": "旧路径",
        "choice_reason": "旧理由",
        "core_claim": "旧论点",
        "evidence_items": ["旧证据"],
        "counterexamples": ["旧反例"]
    }))
    .unwrap();

    let update = context.apply_user_message("我想换成研究小组合作");

    assert!(update.topic_changed);
    assert_eq!(context.topic.as_deref(), Some("小组合作"));
    assert!(context.selected_direction.is_none());
    assert!(context.selected_path_id.is_none());
    assert!(context.selected_path.is_none());
    assert!(context.choice_reason.is_none());
    assert!(context.core_claim.is_none());
    assert!(context.evidence.is_empty());
    assert!(context.counterexamples.is_empty());
}

#[test]
fn motivation_scene_claim_and_evidence_rules_capture_general_user_language() {
    let mut context = WritingContext::default();

    let motivation = context.apply_user_message("因为我发现同学会找饭搭子，却不一定当朋友。");
    assert!(motivation.motivation_captured);
    assert_eq!(
        context.motivation.as_deref(),
        Some("我发现同学会找饭搭子，却不一定当朋友")
    );

    let scene = context.apply_user_message("某次课程作业里，有人拖了进度，组长最后替他做了。");
    assert!(scene.scene_captured);
    assert!(
        context
            .observed_scene
            .as_deref()
            .unwrap()
            .contains("课程作业")
    );

    let claim = context.apply_user_message("我的核心观点是关系成本会让组员默认补位。");
    assert!(claim.claim_captured);
    assert_eq!(
        context.core_claim.as_deref(),
        Some("关系成本会让组员默认补位")
    );

    let evidence = context.apply_user_message("访谈里三位同学都提到过类似经历。");
    assert!(evidence.evidence_added);
    assert_eq!(context.evidence.len(), 2);
}

#[test]
fn weak_writability_motivation_is_normalized_like_legacy_context() {
    let mut context = WritingContext::default();

    let update = context.apply_user_message("我觉得这个题好写，材料也容易找。");

    assert!(update.motivation_captured);
    assert_eq!(
        context.motivation.as_deref(),
        Some("觉得题目好写/材料容易找")
    );
}

#[test]
fn evidence_rejection_rules_do_not_treat_search_refusals_as_evidence() {
    let mut context = WritingContext::default();

    let update = context.apply_user_message("不要文献，我不是让你找文献。");

    assert!(!update.evidence_added);
    assert!(context.evidence.is_empty());
}

#[test]
fn counterexample_clause_is_not_also_positive_evidence() {
    let mut context = WritingContext::from_legacy_json(json!({
        "motivation": "我观察到一个值得解释的现象",
        "candidate_paths": [{"index":"1", "title":"机制解释方向"}],
        "selected_direction": "机制解释方向",
        "choice_reason": "这个方向能解释互动过程"
    }))
    .unwrap();

    let update = context.apply_user_message("但是也可能有一个案例不成立");
    let decision = ThinkingFlowController::new().advance(&context, "可以总结一下");

    assert!(!update.evidence_added);
    assert!(context.evidence.is_empty());
    assert_eq!(context.counterexamples.len(), 1);
    assert_eq!(decision.stage, FlowStage::SummaryReady);
    assert!(decision.ready_for_summary);
}

#[test]
fn numbered_choice_rules_accept_arabic_and_chinese_direction_phrases() {
    for (message, expected) in [
        ("我选择2号方向", "2"),
        ("第三个吧", "3"),
        ("第3个", "3"),
        ("3号方向", "3"),
        ("方向三", "3"),
        ("方向一", "1"),
    ] {
        let mut context = context_with_candidates();

        let update = context.apply_user_message(message);

        assert_eq!(update.selected_path_id.as_deref(), Some(expected));
        assert_eq!(context.selected_path_id.as_deref(), Some(expected));
        assert!(context.selected_direction.is_some());
        assert_eq!(
            context
                .selected_path_detail
                .as_ref()
                .map(|path| path.index.as_str()),
            Some(expected)
        );
    }
}

#[test]
fn numbered_choice_rules_accept_embedded_ordinals_but_not_arbitrary_digits() {
    for (message, expected) in [("我觉得第二个更好", "2"), ("我想试试第3个方向", "3")]
    {
        let mut context = context_with_candidates();

        let update = context.apply_user_message(message);

        assert_eq!(update.selected_path_id.as_deref(), Some(expected));
        assert_eq!(context.selected_path_id.as_deref(), Some(expected));
    }

    let mut context = context_with_candidates();
    let update = context.apply_user_message("我访谈了2位同学");
    assert!(update.selected_path_id.is_none());
    assert!(context.selected_path_id.is_none());
}

#[test]
fn direction_one_fixed_phrase_does_not_select_first_candidate() {
    let mut context = context_with_candidates();

    let update = context.apply_user_message("这个方向一旦确定再说");

    assert!(update.selected_path_id.is_none());
    assert!(context.selected_path_id.is_none());
    assert!(context.selected_direction.is_none());
}

#[test]
fn second_person_phrase_does_not_select_second_candidate() {
    let mut context = context_with_candidates();

    let update = context.apply_user_message("我还访谈了第二个人");

    assert!(update.selected_path_id.is_none());
    assert!(context.selected_path_id.is_none());
    assert!(context.selected_direction.is_none());
}

#[test]
fn an_explicit_summary_request_matches_the_python_summary_contract() {
    let mut context = WritingContext::default();
    let controller = ThinkingFlowController::new();

    context.apply_user_message("总结一下");
    let decision = controller.advance(&context, "总结一下");

    assert_eq!(decision.stage, FlowStage::SummaryReady);
    assert_eq!(decision.prompt_kind, PromptKind::SummaryReady);
    assert_eq!(decision.required_action, "build_summary");
    assert_eq!(decision.allowed_response_kind, "summary");
    assert!(decision.ready_for_summary);
}

#[test]
fn thinking_flow_exposes_the_full_python_decision_contract() {
    let mut context = WritingContext::from_legacy_json(json!({
        "candidate_paths": [{"index":"2", "title":"理论脉络"}],
        "thinking_task": "文献综述"
    }))
    .unwrap();
    let controller = ThinkingFlowController::new();

    let choice = controller.advance(&context, "第二个");
    assert_eq!(choice.stage, FlowStage::ChoiceReflection);
    assert_eq!(choice.required_action, "reflect_on_choice");
    assert_eq!(choice.missing_slot.as_deref(), Some("choice_reason"));
    assert_eq!(choice.allowed_response_kind, "question");
    assert_eq!(choice.selected_option.as_deref(), Some("2"));

    context.flow_stage = Some(FlowStage::ChoiceReflection);
    let reason = controller.advance(&context, "因为它最能解释我的材料");
    assert_eq!(reason.stage, FlowStage::EvidenceCheck);
    assert_eq!(reason.required_action, "check_evidence");
    assert_eq!(
        reason.missing_slot.as_deref(),
        Some("evidence_or_counterexample")
    );
}

#[test]
fn literature_and_revision_tasks_receive_task_specific_candidate_paths() {
    let controller = ThinkingFlowController::new();
    let literature = WritingContext::from_legacy_json(json!({
        "thinking_task":"文献综述", "initial_idea":"我想做搭子研究综述", "motivation":"梳理研究脉络"
    }))
    .unwrap();
    let literature_decision = controller.advance(&literature, "因为现有定义不一致");
    assert_eq!(literature_decision.candidate_paths[0].title, "概念脉络");
    assert_eq!(literature_decision.candidate_paths[1].title, "理论脉络");
    assert_eq!(literature_decision.candidate_paths[2].title, "方法脉络");

    let revision = WritingContext::from_legacy_json(json!({
        "thinking_task":"修改方案", "initial_idea":"修改初稿", "motivation":"论证不够清楚"
    }))
    .unwrap();
    let revision_decision = controller.advance(&revision, "因为文章结构很散");
    assert_eq!(revision_decision.candidate_paths[0].title, "核心论点优先");
    assert_eq!(revision_decision.candidate_paths[1].title, "证据链优先");
    assert_eq!(revision_decision.candidate_paths[2].title, "结构功能优先");
}

#[test]
fn unsupported_claim_transitions_to_evidence_check() {
    let context = WritingContext::from_legacy_json(json!({
        "stage": "topic",
        "topic": "宿舍社交",
        "motivation": "我观察到宿舍夜话会影响关系",
        "candidate_paths": [{"index":"1", "title":"互动机制方向"}],
        "selected_direction": "互动机制方向",
        "selected_path_id": "1",
        "choice_reason": "因为能解释互动过程",
        "flow_stage": "evidence_check",
        "core_claim": "夜话必然会增强宿舍关系"
    }))
    .unwrap();

    let decision = ThinkingFlowController::new().advance(&context, "我的论点就是这样");

    assert_eq!(decision.stage, FlowStage::EvidenceCheck);
    assert!(decision.missing_evidence);
    assert!(!decision.ready_for_summary);
}

#[test]
fn legacy_selected_direction_without_candidates_still_asks_for_choice_reason() {
    let context = WritingContext::from_legacy_json(json!({
        "topic": "宿舍社交",
        "motivation": "我观察到宿舍夜话会影响关系",
        "selected_direction": "互动机制方向",
        "flow_stage": "choice_reflection"
    }))
    .unwrap();

    let decision = ThinkingFlowController::new().advance(&context, "就这个方向");

    assert_eq!(decision.stage, FlowStage::ChoiceReflection);
    assert_eq!(decision.prompt_kind, PromptKind::ChoiceReflection);
}

#[test]
fn legacy_selected_direction_and_reason_without_candidates_still_checks_evidence() {
    let context = WritingContext::from_legacy_json(json!({
        "topic": "宿舍社交",
        "motivation": "我观察到宿舍夜话会影响关系",
        "selected_direction": "互动机制方向",
        "choice_reason": "这个方向能解释互动过程",
        "flow_stage": "evidence_check"
    }))
    .unwrap();

    let decision = ThinkingFlowController::new().advance(&context, "我先说论点");

    assert_eq!(decision.stage, FlowStage::EvidenceCheck);
    assert_eq!(decision.prompt_kind, PromptKind::EvidenceCheck);
    assert!(decision.missing_evidence);
}

#[test]
fn topic_change_is_detected_in_a_later_message_clause() {
    let mut context = WritingContext::from_legacy_json(json!({
        "topic": "搭子社交",
        "selected_direction": "功能替代方向",
        "choice_reason": "旧理由",
        "evidence_items": ["旧证据"]
    }))
    .unwrap();

    let update = context.apply_user_message("换个方向，我想研究小组合作");

    assert!(update.topic_changed);
    assert_eq!(context.topic.as_deref(), Some("小组合作"));
    assert!(context.selected_direction.is_none());
    assert!(context.choice_reason.is_none());
    assert!(context.evidence.is_empty());
}

#[test]
fn short_natural_legacy_topic_statement_is_captured() {
    let mut context = WritingContext::default();

    context.apply_user_message("小组合作的问题");

    assert_eq!(context.topic.as_deref(), Some("小组合作的问题"));
}

#[test]
fn topic_change_rebuilds_legacy_task_summary_and_preserves_unrelated_extras() {
    let mut context = WritingContext::from_legacy_json(json!({
        "topic": "搭子社交",
        "thinking_task": "文献综述",
        "context_summary": "主题：搭子社交；已选路径：功能替代方向",
        "selected_direction": "功能替代方向",
        "future_field": {"preserve": true}
    }))
    .unwrap();

    context.apply_user_message("换个方向，我想研究小组合作");
    let serialized = serde_json::to_value(&context).unwrap();

    assert_eq!(context.extra["thinking_task"], json!("选题"));
    assert_eq!(
        context.context_summary.as_deref(),
        Some("主题：小组合作；学生原始表述：换个方向，我想研究小组合作")
    );
    assert_eq!(context.extra["future_field"], json!({"preserve": true}));
    assert!(!serialized.to_string().contains("搭子社交"));
    assert!(!serialized.to_string().contains("功能替代方向"));
}

#[test]
fn six_turn_flow_reaches_summary_only_through_real_state_transitions() {
    let mut context = WritingContext::default();
    let controller = ThinkingFlowController::new();

    let decision = turn(&mut context, &controller, "我想写搭子社交");
    assert_eq!(decision.stage, FlowStage::MotivationProbe);

    let decision = turn(
        &mut context,
        &controller,
        "因为我发现同学会找饭搭子，却不一定把对方当朋友。",
    );
    assert_eq!(decision.stage, FlowStage::CandidatePaths);
    assert_eq!(decision.candidate_paths.len(), 3);
    assert_eq!(decision.candidate_paths[1].title, "功能替代方向");
    context.candidate_paths = decision.candidate_paths;

    let decision = turn(&mut context, &controller, "第二个");
    assert_eq!(decision.stage, FlowStage::ChoiceReflection);
    assert_eq!(context.selected_path_id.as_deref(), Some("2"));

    let decision = turn(
        &mut context,
        &controller,
        "因为这个问题更能解释轻关系的功能边界",
    );
    assert_eq!(decision.stage, FlowStage::EvidenceCheck);
    assert!(decision.missing_evidence);

    let decision = turn(
        &mut context,
        &controller,
        "访谈里三位同学都说，饭搭子能陪伴但不承担情绪责任。",
    );
    assert_eq!(decision.stage, FlowStage::RefinedAdvice);
    assert!(!decision.ready_for_summary);

    let decision = turn(&mut context, &controller, "可以总结一下");
    assert_eq!(decision.stage, FlowStage::SummaryReady);
    assert_eq!(decision.prompt_kind, PromptKind::SummaryReady);
    assert!(decision.ready_for_summary);
}

#[test]
fn group_work_thread_keeps_a_structured_context_and_builds_matching_paths() {
    let mut context = WritingContext::default();
    let controller = ThinkingFlowController::new();
    let turns = [
        "我想讨论这个小组合作的问题",
        "因为我发现大家分工不均匀但是仍然能正常进行",
        "就是大作业啊，非常不合理，我觉得是因为大家抹不下脸面",
        "不是，是抹不下脸面，别人不干活，不好意思说",
        "当然是有人拖了进度之后不敢催，碍于面子所以把他的那份工作也做了",
        "当然是为什么替人干活反而成了默认选项",
        "我觉得是成绩成本",
    ];

    for message in turns {
        let decision = turn(&mut context, &controller, message);
        context.candidate_paths = decision.candidate_paths;
        context.thinking_stage = Some(decision.stage.clone());
        context.flow_stage = Some(decision.stage);
    }

    let serialized = serde_json::to_value(&context).unwrap();
    assert_eq!(context.topic.as_deref(), Some("小组合作的问题"));
    assert_eq!(
        context.initial_idea.as_deref(),
        Some("我想讨论这个小组合作的问题")
    );
    assert_eq!(
        context.observed_scene.as_deref(),
        Some("有人拖了进度之后不敢催，碍于面子所以把他的那份工作也做了")
    );
    assert_eq!(
        serialized["confusion_point"],
        json!("为什么替人干活反而成了默认选项")
    );
    assert_eq!(
        serialized["suspected_mechanism"],
        json!("面子压力/关系顾虑")
    );
    assert_eq!(serialized["selected_mechanism"], json!("成绩成本"));
    assert_eq!(context.thinking_stage, Some(FlowStage::CandidatePaths));
    assert_eq!(context.candidate_paths[0].title, "沉默成本方向");
    assert_eq!(context.candidate_paths[1].title, "默认补位方向");
    assert_eq!(context.candidate_paths[2].title, "评分制度方向");
    let summary = serialized["context_summary"].as_str().unwrap();
    assert!(summary.contains("主题：小组合作的问题"));
    assert!(summary.contains("已知场景：有人拖了进度之后不敢催"));
    assert!(summary.contains("真正困惑：为什么替人干活反而成了默认选项"));
    assert!(summary.contains("学生猜测机制：面子压力/关系顾虑"));
    assert!(summary.contains("已选关键机制：成绩成本"));
}

#[test]
fn group_work_question_stays_in_probe_until_a_mechanism_or_motivation_is_known() {
    let mut context = WritingContext::default();
    let controller = ThinkingFlowController::new();

    turn(&mut context, &controller, "我想讨论这个小组合作的问题");
    let decision = turn(
        &mut context,
        &controller,
        "比如小组合作，为什么会导致分工不均匀",
    );

    assert_eq!(decision.stage, FlowStage::MotivationProbe);
    assert_eq!(decision.prompt_kind, PromptKind::MotivationProbe);
    assert!(decision.candidate_paths.is_empty());
}

#[test]
fn a_new_explicit_topic_clears_the_complete_thinking_frame() {
    let mut context = WritingContext::default();
    for message in [
        "我想讨论这个小组合作的问题",
        "当然是有人拖了进度之后不敢催，碍于面子所以把他的那份工作也做了",
        "当然是为什么替人干活反而成了默认选项",
        "我觉得是成绩成本",
    ] {
        context.apply_user_message(message);
    }

    let update = context.apply_user_message("算了，我想写搭子有关的话题");
    let serialized = serde_json::to_value(&context).unwrap();

    assert!(update.topic_changed);
    assert_eq!(context.topic.as_deref(), Some("搭子有关的话题"));
    assert_eq!(
        context.initial_idea.as_deref(),
        Some("算了，我想写搭子有关的话题")
    );
    assert!(context.observed_scene.is_none());
    assert!(serialized.get("confusion_point").is_none());
    assert!(serialized.get("suspected_mechanism").is_none());
    assert!(serialized.get("selected_mechanism").is_none());
    assert_eq!(
        serialized["context_summary"],
        json!("主题：搭子有关的话题；学生原始表述：算了，我想写搭子有关的话题")
    );
}

fn context_with_candidates() -> WritingContext {
    WritingContext::from_legacy_json(json!({
        "candidate_paths": [
            {"index":"1", "title":"比较对象方向"},
            {"index":"2", "title":"机制解释方向"},
            {"index":"3", "title":"条件边界方向"}
        ]
    }))
    .unwrap()
}

fn turn(
    context: &mut WritingContext,
    controller: &ThinkingFlowController,
    message: &str,
) -> writing_coach_server::skills::FlowDecision {
    context.apply_user_message(message);
    let decision = controller.advance(context, message);
    context.candidate_paths = decision.candidate_paths.clone();
    context.thinking_stage = Some(decision.stage.clone());
    context.flow_stage = Some(decision.stage.clone());
    context.ready_for_refined_advice = decision.stage == FlowStage::RefinedAdvice;
    decision
}

#[test]
fn guard_normalizes_url_and_doi_and_verifies_claimed_quotes() {
    let guard = GroundingGuard::new();
    let knowledge = KnowledgeBundle {
        hits: vec![SearchHit {
            source: "paper".to_owned(),
            title: "Verified Paper".to_owned(),
            text: "访谈可以呈现责任边界如何被协商。".to_owned(),
            provider: "openalex".to_owned(),
            url: Some("https://example.test/paper".to_owned()),
            doi: Some("10.1234/ABC.1".to_owned()),
            ..SearchHit::default()
        }],
        ..KnowledgeBundle::default()
    };
    let accepted = guard.validate(
        "可对照“访谈可以呈现责任边界如何被协商”（DOI:10.1234/abc.1，https://example.test/paper）。",
        &knowledge,
        GuardPolicy::default(),
    );
    assert!(accepted.allowed, "{accepted:?}");

    let rejected = guard.validate(
        "研究表明“未经支持的精确原话”，详见 https://evil.test/fake 和 DOI:10.9999/fake。",
        &knowledge,
        GuardPolicy::default(),
    );
    assert!(
        rejected
            .violations
            .contains(&"unsupported_source".to_owned())
    );
    assert!(
        rejected
            .violations
            .contains(&"unsupported_quote".to_owned())
    );
}

#[test]
fn guard_binds_suffix_quote_attribution_to_the_specifically_referenced_hit() {
    // Break caught: a real URL/DOI after a fabricated quote used to validate the reference while
    // the quote itself was treated as pedagogical; checking any unrelated hit would be the same
    // bypass under a different shape.
    let guard = GroundingGuard::new();
    let knowledge = KnowledgeBundle {
        hits: vec![
            SearchHit {
                source: "verified-paper".to_owned(),
                title: "Verified Paper".to_owned(),
                text: "访谈可以呈现责任边界如何被协商。".to_owned(),
                provider: "openalex".to_owned(),
                year: Some(2024),
                authors: vec!["张明".to_owned()],
                url: Some("https://example.test/verified".to_owned()),
                doi: Some("10.1234/verified.1".to_owned()),
                ..SearchHit::default()
            },
            SearchHit {
                source: "unrelated-paper".to_owned(),
                title: "Unrelated Paper".to_owned(),
                text: "这是一句伪造的精确原话。".to_owned(),
                provider: "openalex".to_owned(),
                year: Some(2023),
                authors: vec!["李芳".to_owned()],
                url: Some("https://example.test/unrelated".to_owned()),
                doi: Some("10.1234/unrelated.1".to_owned()),
                ..SearchHit::default()
            },
        ],
        ..KnowledgeBundle::default()
    };

    for answer in [
        "可对照“访谈可以呈现责任边界如何被协商”（https://example.test/verified）。",
        "可对照“访谈可以呈现责任边界如何被协商”（DOI: 10.1234/verified.1）。",
        "可对照“访谈可以呈现责任边界如何被协商”（张明，2024）。",
    ] {
        let result = guard.validate(answer, &knowledge, GuardPolicy::default());
        assert!(result.allowed, "answer={answer}; result={result:?}");
    }

    for answer in [
        "可对照“这是一句伪造的精确原话”（https://example.test/verified）。",
        "可对照“这是一句伪造的精确原话”（DOI: 10.1234/verified.1）。",
        "可对照“这是一句伪造的精确原话”（张明，2024）。",
    ] {
        let result = guard.validate(answer, &knowledge, GuardPolicy::default());
        assert!(
            result.violations.contains(&"unsupported_quote".to_owned()),
            "answer={answer}; result={result:?}"
        );
    }
}

#[test]
fn guard_recognizes_author_year_syntax_before_resolving_the_cited_hit() {
    // Break caught: `(Smith, 2024)` was treated as pedagogical whenever Smith was absent from the
    // hit list, because author-year attribution was recognized only after a successful lookup.
    let knowledge = KnowledgeBundle {
        hits: vec![
            SearchHit {
                source: "chinese-paper".to_owned(),
                title: "Chinese Paper".to_owned(),
                text: "访谈可以呈现责任边界如何被协商。".to_owned(),
                provider: "openalex".to_owned(),
                year: Some(2024),
                authors: vec!["张明".to_owned()],
                ..SearchHit::default()
            },
            SearchHit {
                source: "latin-paper".to_owned(),
                title: "Latin Paper".to_owned(),
                text: "Interviews reveal how responsibility boundaries are negotiated.".to_owned(),
                provider: "openalex".to_owned(),
                year: Some(2023),
                authors: vec!["Jane Doe".to_owned()],
                ..SearchHit::default()
            },
        ],
        ..KnowledgeBundle::default()
    };

    for answer in [
        "“访谈可以呈现责任边界如何被协商”（张明，2024）。",
        "\"Interviews reveal how responsibility boundaries are negotiated.\" (Doe, 2023).",
    ] {
        let result = GroundingGuard::new().validate(answer, &knowledge, GuardPolicy::default());
        assert!(result.allowed, "answer={answer}; result={result:?}");
    }

    let unresolved = GroundingGuard::new().validate(
        "“伪造原话”（Smith, 2024）。",
        &knowledge,
        GuardPolicy::default(),
    );
    assert!(
        unresolved
            .violations
            .contains(&"unsupported_quote".to_owned()),
        "{unresolved:?}"
    );
    assert!(
        unresolved
            .violations
            .contains(&"unsupported_source".to_owned()),
        "{unresolved:?}"
    );

    for pedagogical in [
        "可以追问“责任边界如何被协商？”（示例问题）。",
        "可以追问“责任边界如何被协商？”（2024）。",
    ] {
        let result =
            GroundingGuard::new().validate(pedagogical, &knowledge, GuardPolicy::default());
        assert!(result.allowed, "answer={pedagogical}; result={result:?}");
    }
}

#[test]
fn guard_binds_user_quote_attribution_to_actual_user_text() {
    // Break caught: a quote labeled as the user's original text must not be validated by an
    // unrelated retrieved hit that happens to contain the fabricated fragment.
    let knowledge = KnowledgeBundle {
        hits: vec![SearchHit {
            source: "paper".to_owned(),
            title: "Paper".to_owned(),
            text: "这是一句伪造的用户原话。".to_owned(),
            provider: "openalex".to_owned(),
            ..SearchHit::default()
        }],
        ..KnowledgeBundle::default()
    };
    let policy =
        GuardPolicy::default().with_user_texts(["我的原文是：小组责任边界不清会放大免费搭车。"]);

    let accepted = GroundingGuard::new().validate(
        "你的原句“小组责任边界不清会放大免费搭车”可以保留。",
        &knowledge,
        policy.clone(),
    );
    assert!(accepted.allowed, "{accepted:?}");

    let rejected = GroundingGuard::new().validate(
        "“这是一句伪造的用户原话”（用户原文）。",
        &knowledge,
        policy,
    );
    assert!(
        rejected
            .violations
            .contains(&"unsupported_quote".to_owned()),
        "{rejected:?}"
    );
}

#[test]
fn guard_accepts_exact_user_fragment_and_does_not_misread_a_warning_as_delivery() {
    let guard = GroundingGuard::new();
    let policy =
        GuardPolicy::default().with_user_texts(["我的原文是：小组责任边界不清会放大免费搭车。"]);
    let result = guard.validate(
        "你的原句“小组责任边界不清会放大免费搭车”可用于定位问题，但建议不可以直接提交。",
        &KnowledgeBundle::default(),
        policy,
    );
    assert!(result.allowed);
}

#[test]
fn guard_blocks_unlabeled_submit_ready_full_draft_and_unsupported_factual_claim() {
    let guard = GroundingGuard::new();
    let paragraph = "在当代大学生的小组作业中，分工与责任边界直接影响合作效率。本文从互惠规范出发，分析免费搭车形成的机制及其对团队信任的影响，并讨论可行的治理路径。";
    let draft = [paragraph; 5].join("\n\n");
    assert!(
        guard
            .validate(
                &draft,
                &KnowledgeBundle::default(),
                GuardPolicy::default().with_direct_delivery_risk(true),
            )
            .violations
            .contains(&"ghostwriting_delivery".to_owned())
    );
    assert!(
        guard
            .validate(
                "研究表明，87%的学生因此放弃合作。",
                &KnowledgeBundle::default(),
                GuardPolicy::default(),
            )
            .violations
            .contains(&"ungrounded_claim".to_owned())
    );
}

#[test]
fn guard_does_not_allow_draft_words_to_bypass_complete_draft_detection() {
    let guard = GroundingGuard::new();
    let paragraph = "在当代大学生的小组作业中，研究问题聚焦于责任边界如何影响合作。本文首先分析免费搭车的形成机制，其次讨论互惠规范的作用，最后建议通过明确分工改善团队信任。";
    let draft = [paragraph; 4].join("\n\n");

    let result = guard.validate(
        &draft,
        &KnowledgeBundle::default(),
        GuardPolicy::default().with_direct_delivery_risk(true),
    );

    assert!(
        result
            .violations
            .contains(&"ghostwriting_delivery".to_owned()),
        "{result:?}"
    );
}

#[test]
fn guard_allows_long_teaching_explanation_without_direct_writing_risk() {
    let guard = GroundingGuard::new();
    let paragraph = "课件里的研究问题是一个可以用材料和论证回答的聚焦问题。首先要区分宽泛的主题和具体问题，其次要检查手上材料是否足以回答，因此评估时会同时看对象、机制和证据。";
    let explanation = [paragraph; 4].join("\n\n");

    let result = guard.validate(
        &explanation,
        &KnowledgeBundle::default(),
        GuardPolicy::default(),
    );

    assert!(result.allowed, "{result:?}");
    assert!(
        !result
            .violations
            .contains(&"ghostwriting_delivery".to_owned())
    );
}

#[test]
fn guard_blocks_explicit_submit_ready_claim_without_inferred_user_risk() {
    let result = GroundingGuard::new().validate(
        "以下是一篇完整范文，你可以直接提交。",
        &KnowledgeBundle::default(),
        GuardPolicy::default(),
    );
    assert!(
        result
            .violations
            .contains(&"ghostwriting_delivery".to_owned())
    );
}

#[test]
fn guard_checks_quote_attribution_after_quote_and_each_factual_sentence() {
    let guard = GroundingGuard::new();
    let knowledge = KnowledgeBundle {
        hits: vec![SearchHit {
            source: "paper".to_owned(),
            title: "Verified Paper".to_owned(),
            text: "访谈可以呈现责任边界如何被协商。".to_owned(),
            provider: "openalex".to_owned(),
            ..SearchHit::default()
        }],
        ..KnowledgeBundle::default()
    };

    let supported = guard.validate(
        "“访谈可以呈现责任边界如何被协商”（课程材料）。",
        &knowledge,
        GuardPolicy::default(),
    );
    assert!(supported.allowed, "{supported:?}");

    let unsupported_quote = guard.validate(
        "“这是一句伪造的课程原话”（课程材料）。",
        &knowledge,
        GuardPolicy::default(),
    );
    assert!(
        unsupported_quote
            .violations
            .contains(&"unsupported_quote".to_owned())
    );

    let mixed = guard.validate(
        "研究表明，责任边界需要协商 [来源: Verified Paper]。数据显示，87%的学生因此放弃合作。",
        &knowledge,
        GuardPolicy::default(),
    );
    assert!(mixed.violations.contains(&"ungrounded_claim".to_owned()));
}

#[test]
fn guard_allows_diagnostic_guidance_and_pedagogical_quotes() {
    let guard = GroundingGuard::new();
    let diagnostic = [
        "【问题定位】当前段落把现象和机制放在一句里，读者不容易判断你究竟想证明什么。请先标出你已有的观察材料，再区分描述和解释。",
        "【启发建议】可以把研究问题改成“责任边界如何影响合作？”，这是提问示例，不是对原文或文献的引用。你还需要用自己的材料检验它。",
        "【下一步】请你用两句话补充一个具体场景和一条反例，然后我再帮你检查论点、证据与推理是否连接。这些步骤是修改任务，不是可提交正文。",
    ]
    .join("\n\n");

    let result = guard.validate(
        &diagnostic,
        &KnowledgeBundle::default(),
        GuardPolicy::default(),
    );

    assert!(result.allowed, "{result:?}");
    assert!(!result.violations.contains(&"unsupported_quote".to_owned()));
    assert!(
        !result
            .violations
            .contains(&"ghostwriting_delivery".to_owned())
    );
}
