use serde_json::json;
use writing_coach_server::{
    domain::WritingContext,
    skills::MaterialSearchService,
    tools::{KnowledgeBundle, SearchHit},
};

#[test]
fn material_plan_uses_topic_context_english_terms_and_strict_years() {
    let context = WritingContext::from_legacy_json(json!({
        "topic":"小组合作分工不均",
        "selected_path":"默认补位方向",
        "search_keywords":["小组合作", "搭便车"]
    }))
    .unwrap();
    let plan = MaterialSearchService::new(5).build_plan("联网搜索2025-2026的资料", &context);

    assert_eq!(plan.year_from, Some(2025));
    assert_eq!(plan.year_to, Some(2026));
    assert_eq!(plan.strict_year_label.as_deref(), Some("2025-2026"));
    assert_eq!(plan.query_terms[0], "social loafing group work free rider");
    assert!(plan.web_query.contains("小组合作 分工不均 搭便车"));
}

#[test]
fn material_reply_is_deterministic_partitioned_and_source_grounded() {
    let service = MaterialSearchService::new(5);
    let context = WritingContext::from_legacy_json(json!({"topic":"小组合作"})).unwrap();
    let plan = service.build_plan("找一些小组合作资料", &context);
    let bundle = KnowledgeBundle {
        hits: vec![
            SearchHit {
                source: "corpus/course/group.md".to_owned(),
                title: "小组合作".to_owned(),
                text: "小组合作中的隐形乘客、搭便车与评分规则。".to_owned(),
                provider: "course_corpus".to_owned(),
                ..SearchHit::default()
            },
            SearchHit {
                source: "openalex:1".to_owned(),
                title: "Social Loafing in Student Teams".to_owned(),
                provider: "openalex".to_owned(),
                year: Some(2024),
                authors: vec!["A. Author".to_owned()],
                url: Some("https://example.test/paper".to_owned()),
                ..SearchHit::default()
            },
            SearchHit {
                source: "https://example.test/page".to_owned(),
                title: "小组合作学习中的搭便车问题".to_owned(),
                text: "治理线索".to_owned(),
                provider: "web".to_owned(),
                url: Some("https://example.test/page".to_owned()),
                ..SearchHit::default()
            },
        ],
        ..KnowledgeBundle::default()
    };

    let reply = service.build_reply(&plan, &bundle, true);
    assert!(reply.contains("【课程材料】"));
    assert!(reply.contains("【联网文献】"));
    assert!(reply.contains("[Social Loafing in Student Teams](https://example.test/paper)"));
    assert!(reply.contains("[小组合作学习中的搭便车问题](https://example.test/page)"));
    assert!(reply.contains("【怎么用】"));
    assert!(reply.contains("【下一步筛选任务】"));
    assert!(!reply.contains("corpus/course/group.md"));
}

#[test]
fn disabled_online_search_is_stated_without_fake_results() {
    let service = MaterialSearchService::new(5);
    let context = WritingContext::default();
    let plan = service.build_plan("找资料", &context);
    let reply = service.build_reply(&plan, &KnowledgeBundle::default(), false);

    assert!(reply.contains("这次只查了本地课程语料"));
    assert!(!reply.contains("模拟查询结果"));
}
