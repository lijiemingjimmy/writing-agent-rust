#![recursion_limit = "256"]

#[allow(dead_code)]
mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{OriginalUri, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use writing_coach_server::{
    corpus::{
        chunking::chunk_document,
        markdown::{MarkdownKnowledgeTool, search_markdown},
        session_documents::search_session_documents,
    },
    skills::SkillRegistry,
    store::sessions::{DocumentRepository, SessionRepository},
    tools::{
        HttpTimeouts, KnowledgeCoordinator, KnowledgePlan, KnowledgeTool, PlannedSearch, SearchHit,
        SearchRequest, ToolError,
        scholarly::{ScholarlyConfig, ScholarlySearch},
        web::{WebConfig, WebSearch},
    },
};

struct PoisonedProviderFailure;

#[async_trait]
impl KnowledgeTool for PoisonedProviderFailure {
    fn name(&self) -> &'static str {
        "poisoned_error"
    }

    async fn search(
        &self,
        _request: SearchRequest,
        _cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        Err(ToolError::Provider {
            provider: "bad\nprovider/../../private".repeat(8),
            message: format!(
                "/Users/student/private/key body=SYSTEM_OVERRIDE {} request timed out",
                "x".repeat(10_000)
            ),
        })
    }
}

#[tokio::test]
async fn session_document_search_returns_only_the_relevant_traceable_chunk() {
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    writing_coach_server::store::sqlite::migrate(&pool)
        .await
        .unwrap();
    let session = SessionRepository::new(pool.clone())
        .create(Some("student-1"))
        .await
        .unwrap();
    let text = "# 研究背景\n普通的小组合作说明。\n\n# 责任边界\n青铜雨伞假说认为责任边界模糊。";
    let chunks = chunk_document("访谈记录.md", text);
    let documents = DocumentRepository::new(pool);
    let document = documents
        .add_with_chunks(
            session.id,
            "访谈记录.md",
            "text/markdown",
            text,
            None,
            &chunks,
        )
        .await
        .unwrap();

    let hits = search_session_documents(&documents, session.id, "青铜雨伞 责任边界", 5)
        .await
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].heading, "责任边界");
    assert!(hits[0].text.contains("青铜雨伞假说"));
    assert_eq!(hits[0].metadata["document_id"], document.id.to_legacy_hex());
    assert!(hits[0].metadata["chunk_id"].as_str().is_some());
}

struct PoisonedHitFailure;

#[async_trait]
impl KnowledgeTool for PoisonedHitFailure {
    fn name(&self) -> &'static str {
        "poisoned_hit"
    }

    async fn search(
        &self,
        _request: SearchRequest,
        _cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        Ok(vec![SearchHit {
            source: "safe".to_owned(),
            title: "safe".to_owned(),
            text: "safe".to_owned(),
            provider: "fixture".to_owned(),
            metadata: serde_json::Map::from_iter([(
                "provider_failures".to_owned(),
                json!([{
                    "provider": "metadata\r\n/provider",
                    "message": "secret response body and /tmp/private.db"
                }]),
            )]),
            ..SearchHit::default()
        }])
    }
}

#[tokio::test]
async fn coordinator_normalizes_all_injected_provider_failures() {
    let tools: Vec<Arc<dyn KnowledgeTool>> = vec![
        Arc::new(PoisonedProviderFailure),
        Arc::new(PoisonedHitFailure),
    ];
    let coordinator = KnowledgeCoordinator::new(tools);
    let plan = KnowledgePlan::new([
        PlannedSearch::new("poisoned_error", SearchRequest::new("query")),
        PlannedSearch::new("poisoned_hit", SearchRequest::new("query")),
    ]);

    let bundle = coordinator.execute(plan, CancellationToken::new()).await;

    assert_eq!(bundle.metadata.provider_failures.len(), 2);
    for failure in &bundle.metadata.provider_failures {
        assert!(failure.provider.len() <= 48);
        assert!(
            failure.provider.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            )
        );
        assert_eq!(failure.message, "provider_error");
    }
}

#[test]
fn coordinator_reports_only_actually_installed_tool_capabilities() {
    let empty = KnowledgeCoordinator::new(Vec::new());
    assert!(!empty.has_tool("web"));
    let web: Vec<Arc<dyn KnowledgeTool>> = vec![Arc::new(PoisonedHitFailure)];
    let coordinator = KnowledgeCoordinator::new(web);
    assert!(coordinator.has_tool("poisoned_hit"));
    assert!(!coordinator.has_tool("web"));
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_markdown_tool_rejects_untrusted_scope_ids_and_post_load_symlink_escape() {
    // Break caught: public request paths or a symlink created after registry validation can make
    // the canonical tool read a file outside the configured Skill project root.
    use std::os::unix::fs::symlink;

    let root = temporary_dir("trusted-corpus-scope");
    fs::create_dir_all(root.join("skills")).unwrap();
    fs::create_dir_all(root.join("corpus")).unwrap();
    write(
        &root.join("skills/ppt.yaml"),
        "id: ppt_qa\nname: PPT\ndescription: course\ntrigger_keywords: [PPT]\nrequired_slots: []\ncorpus_paths: [corpus/*.md]\n",
    );
    write(
        &root.join("corpus/safe.md"),
        "# 安全课件\n研究问题要可论证。",
    );
    let outside = root.parent().unwrap().join(format!(
        "outside-secret-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    write(&outside, "# secret\n研究问题的私密指令");
    let registry = SkillRegistry::load(&root.join("skills")).unwrap();
    let tool = MarkdownKnowledgeTool::new(registry);

    for untrusted in ["../outside-secret", outside.to_str().unwrap()] {
        let hits = tool
            .search(
                SearchRequest::new("研究问题").with_target_skill_id(untrusted),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(hits.is_empty(), "scope={untrusted}");
    }

    let safe = tool
        .search(
            SearchRequest::new("研究问题").with_target_skill_id("ppt_qa"),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(safe.len(), 1);
    symlink(&outside, root.join("corpus/escape.md")).unwrap();
    let error = tool
        .search(
            SearchRequest::new("研究问题").with_target_skill_id("ppt_qa"),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "local knowledge search failed: course corpus search failed"
    );
    fs::remove_file(outside).unwrap();
    fs::remove_dir_all(root).unwrap();
}

fn temporary_dir(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("writing-coach-{label}-{unique}"));
    fs::create_dir_all(&root).expect("temporary directory is created");
    root
}

fn write(path: &Path, text: &str) {
    fs::write(path, text).expect("fixture is written");
}

#[test]
fn markdown_search_preserves_heading_title_and_metadata_scoring_rules() {
    // Mutation caught: score heading/title matches as body-only matches, or index frontmatter keys.
    let root = temporary_dir("markdown-parity");
    let corpus = root.join("corpus.md");
    write(
        &corpus,
        r#"---
title: 课程研究
owner: metadata-only-value
---
# 研究问题
这一节讲观察和提问。
# 方法
研究问题需要材料。
"#,
    );

    let hits = search_markdown("研究问题", std::slice::from_ref(&corpus), 5).unwrap();

    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].heading, "研究问题");
    assert_eq!(hits[0].title, "课程研究");
    assert!(hits[0].score > hits[1].score);
    assert!(
        search_markdown("metadata-only-value", &[corpus], 5)
            .unwrap()
            .is_empty()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn markdown_search_uses_chinese_ngrams_latin_tokens_stable_globs_and_top_k() {
    // Mutations caught: exact-Chinese-only tokenization, case-sensitive query tokens, unstable glob sort,
    // or truncating before score/order sorting.
    let root = temporary_dir("markdown-order");
    write(
        &root.join("b.md"),
        "# B\n这里讨论搭子社交与 social loafing。\n",
    );
    write(
        &root.join("a.md"),
        "# A\n这里也讨论搭子社交与 social loafing。\n",
    );
    let patterns = [root.join("b*.md"), root.join("a*.md")];
    let single_glob = [root.join("*.md")];

    let chinese = search_markdown("青年搭子社交关系", &patterns, 5).unwrap();
    let latin = search_markdown("Social Loafing", &patterns, 1).unwrap();
    let single_glob_hits = search_markdown("social loafing", &single_glob, 5).unwrap();

    assert_eq!(chinese.len(), 2);
    assert!(chinese[0].source.ends_with("b.md"));
    assert!(chinese[1].source.ends_with("a.md"));
    assert_eq!(latin.len(), 1);
    assert!(latin[0].source.ends_with("b.md"));
    assert!(single_glob_hits[0].source.ends_with("a.md"));
    assert!(single_glob_hits[1].source.ends_with("b.md"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn markdown_cache_invalidates_when_modification_time_changes() {
    // Mutation caught: cache only by canonical path and serve stale parsed chunks after a file edit.
    let root = temporary_dir("markdown-cache");
    let corpus = root.join("cached.md");
    write(&corpus, "# 旧版\n阿尔法材料\n");
    assert_eq!(
        search_markdown("阿尔法", std::slice::from_ref(&corpus), 5)
            .unwrap()
            .len(),
        1
    );

    std::thread::sleep(Duration::from_millis(10));
    write(&corpus, "# 新版\n贝塔材料已更新\n");

    assert!(
        search_markdown("阿尔法", std::slice::from_ref(&corpus), 5)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        search_markdown("贝塔", &[corpus], 5).unwrap()[0].heading,
        "新版"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn session_document_search_reads_only_existing_parsed_text() {
    // Mutation caught: search raw_path/metadata instead of documents.parsed_text, or include NULL text.
    let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
    support::create_existing_schema(&pool).await;
    let sessions = SessionRepository::new(pool.clone());
    let session = sessions.create(Some("student-1")).await.unwrap();
    let documents = DocumentRepository::new(pool);
    documents
        .add(
            session.id,
            "notes.md",
            "text/markdown",
            Some("/raw/观察机制.md"),
            Some("访谈材料显示，观察机制会影响合作。"),
            None,
        )
        .await
        .unwrap();
    documents
        .add(
            session.id,
            "unparsed.pdf",
            "application/pdf",
            Some("/raw/观察机制.pdf"),
            None,
            None,
        )
        .await
        .unwrap();

    let hits = search_session_documents(&documents, session.id, "观察机制", 5)
        .await
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source, "notes.md");
    assert_eq!(hits[0].provider, "session_document");
}

#[derive(Clone, Copy)]
enum MockMode {
    ScholarlyFallback,
    ScholarlyYearFiltering,
    WebFallback,
    SearxSuccess,
    BingSuccess,
    ScholarlyMany,
    ScholarlyFallbackMulti,
    CancelAfterSemantic,
}

#[derive(Clone)]
struct MockState {
    mode: MockMode,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    cancel: Option<CancellationToken>,
}

#[derive(Clone, Debug)]
struct RecordedRequest {
    path_and_query: String,
    bing_key: Option<String>,
    brave_key: Option<String>,
}

struct MockServer {
    base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: JoinHandle<()>,
}

impl MockServer {
    async fn spawn(mode: MockMode, cancel: Option<CancellationToken>) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = MockState {
            mode,
            requests: requests.clone(),
            cancel,
        };
        let app = Router::new()
            .route("/semantic/paper/search", get(mock_handler))
            .route("/crossref/works", get(mock_handler))
            .route("/openalex/works", get(mock_handler))
            .route("/searx/search", get(mock_handler))
            .route("/bing", get(mock_handler))
            .route("/brave", get(mock_handler))
            .with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base_url: format!("http://{address}"),
            requests,
            task,
        }
    }

    fn recorded(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn mock_handler(
    State(state): State<MockState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let path = uri.path().to_owned();
    state.requests.lock().unwrap().push(RecordedRequest {
        path_and_query: uri.to_string(),
        bing_key: header(&headers, "ocp-apim-subscription-key"),
        brave_key: header(&headers, "x-subscription-token"),
    });

    match (state.mode, path.as_str()) {
        (MockMode::ScholarlyFallback, "/semantic/paper/search") => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "upstream unavailable"})),
        )
            .into_response(),
        (MockMode::ScholarlyFallbackMulti, "/semantic/paper/search") => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "upstream unavailable"})),
        )
            .into_response(),
        (MockMode::CancelAfterSemantic, "/semantic/paper/search") => {
            state.cancel.as_ref().unwrap().cancel();
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "cancel now"})),
            )
                .into_response()
        }
        (MockMode::ScholarlyFallback | MockMode::CancelAfterSemantic, "/crossref/works") => {
            (StatusCode::OK, Json(crossref_payload())).into_response()
        }
        (MockMode::ScholarlyFallbackMulti, "/crossref/works") => {
            (StatusCode::OK, Json(crossref_multi_payload())).into_response()
        }
        (MockMode::ScholarlyYearFiltering, "/semantic/paper/search") => {
            (StatusCode::OK, Json(semantic_payload())).into_response()
        }
        (MockMode::ScholarlyYearFiltering, "/crossref/works") => {
            (StatusCode::OK, Json(crossref_payload())).into_response()
        }
        (MockMode::ScholarlyYearFiltering, "/openalex/works") => {
            (StatusCode::OK, Json(openalex_payload())).into_response()
        }
        (MockMode::ScholarlyMany, "/semantic/paper/search") => {
            (StatusCode::OK, Json(semantic_many_payload())).into_response()
        }
        (MockMode::WebFallback, "/searx/search") => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "not ready"})),
        )
            .into_response(),
        (MockMode::WebFallback, "/bing") => (StatusCode::OK, Json(bing_payload())).into_response(),
        (MockMode::WebFallback, "/brave") => {
            (StatusCode::OK, Json(brave_payload())).into_response()
        }
        (MockMode::SearxSuccess, "/searx/search") => {
            (StatusCode::OK, Json(searx_payload())).into_response()
        }
        (MockMode::BingSuccess, "/bing") => {
            (StatusCode::OK, Json(bing_success_payload())).into_response()
        }
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unexpected path"})),
        )
            .into_response(),
    }
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn semantic_payload() -> Value {
    json!({
        "total": 3,
        "offset": 0,
        "next": 3,
        "data": [
            semantic_paper("S-OLD", "Old ties paper", 2022),
            semantic_paper("S-IN", "Recent ties paper", 2024),
            {
                "paperId": "S-NO-YEAR", "title": "Unknown year paper", "year": null,
                "authors": [], "venue": null, "externalIds": {},
                "url": "https://example.test/unknown", "citationCount": 0,
                "abstract": null, "publicationTypes": [], "publicationDate": null,
                "journal": null
            }
        ]
    })
}

fn semantic_paper(id: &str, title: &str, year: i32) -> Value {
    json!({
        "paperId": id, "title": title, "year": year,
        "authors": [{"authorId": "A-1", "name": "Test Author"}],
        "venue": "Test Journal", "externalIds": {"DOI": format!("10.1000/{id}"), "CorpusId": 7},
        "url": format!("https://example.test/{id}"), "citationCount": 12,
        "abstract": "A complete semantic scholar fixture.",
        "publicationTypes": ["JournalArticle"], "publicationDate": format!("{year}-02-03"),
        "journal": {"name": "Test Journal", "pages": "1-9", "volume": "2"}
    })
}

fn semantic_many_payload() -> Value {
    json!({
        "total": 8,
        "offset": 0,
        "next": 8,
        "data": (0..8)
            .map(|index| semantic_paper(
                &format!("S-{index}"),
                &format!("Semantic result {index}"),
                2024,
            ))
            .collect::<Vec<_>>()
    })
}

fn crossref_payload() -> Value {
    json!({
        "status": "ok", "message-type": "work-list", "message-version": "1.0.0",
        "message": {
            "facets": {}, "total-results": 1, "items-per-page": 5,
            "query": {"start-index": 0, "search-terms": null},
            "items": [{
                "indexed": {"date-parts": [[2026, 1, 1]], "date-time": "2026-01-01T00:00:00Z", "timestamp": 1},
                "reference-count": 8, "publisher": "Test Publisher", "license": [],
                "content-domain": {"domain": [], "crossmark-restriction": false},
                "short-container-title": ["TJS"], "published-print": {"date-parts": [[2024, 1, 1]]},
                "DOI": "10.1000/crossref", "type": "journal-article", "created": {"date-parts": [[2024, 1, 1]], "date-time": "2024-01-01T00:00:00Z", "timestamp": 1},
                "page": "1-9", "source": "Crossref", "is-referenced-by-count": 9,
                "title": ["Crossref social ties result"], "prefix": "10.1000", "volume": "1",
                "author": [{"ORCID": "https://orcid.org/0000-0000", "authenticated-orcid": false, "given": "Lin", "family": "Test", "sequence": "first", "affiliation": []}],
                "member": "1", "container-title": ["Test Journal"], "original-title": [],
                "language": "en", "link": [], "deposited": {"date-parts": [[2024, 1, 2]], "date-time": "2024-01-02T00:00:00Z", "timestamp": 2},
                "score": 12.0, "resource": {"primary": {"URL": "https://example.test/crossref"}},
                "subtitle": [], "short-title": [], "issued": {"date-parts": [[2024, 1, 1]]},
                "references-count": 8, "URL": "https://example.test/crossref",
                "relation": {}, "ISSN": ["0000-0000"], "issn-type": [{"value": "0000-0000", "type": "print"}],
                "subject": ["Sociology"], "published": {"date-parts": [[2024, 1, 1]]},
                "abstract": "<jats:p>Complete Crossref fixture.</jats:p>"
            }]
        }
    })
}

fn crossref_multi_payload() -> Value {
    let mut payload = crossref_payload();
    let first = payload["message"]["items"][0].clone();
    let mut second = first.clone();
    second["DOI"] = json!("10.1000/crossref-second");
    second["URL"] = json!("https://example.test/crossref-second");
    second["title"] = json!(["Second Crossref social ties result"]);
    payload["message"]["total-results"] = json!(2);
    payload["message"]["items"] = json!([first, second]);
    payload
}

fn openalex_payload() -> Value {
    json!({
        "meta": {"count": 2, "db_response_time_ms": 1, "page": 1, "per_page": 5, "groups_count": null, "cost_usd": 0.0001},
        "results": [openalex_work("W-OLD", 2021), openalex_work("W-IN", 2025)],
        "group_by": []
    })
}

fn openalex_work(id: &str, year: i32) -> Value {
    json!({
        "id": format!("https://openalex.org/{id}"), "doi": format!("https://doi.org/10.1000/{id}"),
        "title": format!("OpenAlex {id}"), "display_name": format!("OpenAlex {id}"),
        "publication_year": year, "publication_date": format!("{year}-01-01"), "ids": {"openalex": format!("https://openalex.org/{id}")},
        "language": "en", "primary_location": {"is_oa": true, "landing_page_url": format!("https://example.test/{id}"), "pdf_url": null, "source": {"id": "https://openalex.org/S1", "display_name": "OpenAlex Journal"}},
        "type": "article", "indexed_in": ["crossref"], "open_access": {"is_oa": true, "oa_status": "gold", "oa_url": null, "any_repository_has_fulltext": false},
        "authorships": [{"author_position": "first", "author": {"id": "https://openalex.org/A1", "display_name": "Alex Author"}, "institutions": [], "countries": [], "is_corresponding": true, "raw_author_name": "Alex Author", "raw_affiliation_strings": [], "affiliations": []}],
        "institutions": [], "countries_distinct_count": 0, "institutions_distinct_count": 0,
        "corresponding_author_ids": ["https://openalex.org/A1"], "corresponding_institution_ids": [],
        "apc_list": null, "apc_paid": null, "fwci": 1.0, "has_fulltext": false, "cited_by_count": 4,
        "citation_normalized_percentile": null, "cited_by_percentile_year": null, "biblio": {"volume": "1", "issue": "1", "first_page": "1", "last_page": "9"},
        "is_retracted": false, "is_paratext": false, "primary_topic": null, "topics": [], "keywords": [], "concepts": [], "mesh": [],
        "locations_count": 1, "locations": [], "best_oa_location": null, "sustainable_development_goals": [], "awards": [],
        "funders": [], "has_content": {"pdf": false, "grobid_xml": false},
        "content_urls": null, "referenced_works_count": 0, "referenced_works": [], "related_works": [],
        "abstract_inverted_index": {"Complete": [0], "OpenAlex": [1], "fixture": [2]},
        "counts_by_year": [], "updated_date": "2026-01-01T00:00:00Z", "created_date": "2024-01-01"
    })
}

fn bing_payload() -> Value {
    json!({
        "_type": "SearchResponse",
        "queryContext": {"originalQuery": "social ties"},
        "webPages": {"webSearchUrl": "https://bing.test/search", "totalEstimatedMatches": 0, "value": []},
        "rankingResponse": {"mainline": {"items": []}}
    })
}

fn bing_success_payload() -> Value {
    json!({
        "_type": "SearchResponse",
        "queryContext": {"originalQuery": "social ties", "alterationDisplayQuery": null},
        "webPages": {
            "webSearchUrl": "https://bing.test/search", "totalEstimatedMatches": 1,
            "value": [{
                "id": "https://bing.test/api/v7/#WebPages.0", "name": "Bing social ties result",
                "url": "https://example.test/bing", "isFamilyFriendly": true,
                "displayUrl": "https://example.test/bing", "snippet": "A complete Bing fixture.",
                "dateLastCrawled": "2026-08-24T00:00:00.0000000Z", "cachedPageUrl": null,
                "language": "en", "isNavigational": false, "siteName": "Example University"
            }]},
        "rankingResponse": {"mainline": {"items": [{"answerType": "WebPages", "resultIndex": 0, "value": {"id": "https://bing.test/api/v7/#WebPages.0"}}]}}
    })
}

fn brave_payload() -> Value {
    json!({
        "type": "search",
        "query": {"original": "social ties", "show_strict_warning": false, "is_navigational": false, "is_news_breaking": false, "spellcheck_off": false, "country": "us", "bad_results": false, "should_fallback": false, "postal_code": "", "city": "", "header_country": "", "more_results_available": false, "state": ""},
        "mixed": {"type": "mixed", "main": [], "top": [], "side": []},
        "web": {"type": "search", "family_friendly": true, "results": [{
            "title": "Brave social ties result", "url": "https://example.test/brave",
            "is_source_local": false, "is_source_both": false, "description": "A complete Brave fixture.",
            "profile": {"name": "Example", "long_name": "Example Source", "url": "https://example.test", "img": ""},
            "language": "en", "family_friendly": true, "type": "search_result", "subtype": "generic", "is_live": false,
            "meta_url": {"scheme": "https", "netloc": "example.test", "hostname": "example.test", "favicon": "", "path": "brave"},
            "thumbnail": null, "age": "August 1, 2026"
        }]}
    })
}

fn searx_payload() -> Value {
    json!({
        "query": "social ties", "number_of_results": 1,
        "results": [{
            "url": "https://example.test/searx", "title": "Searx result", "content": "A result.",
            "publishedDate": null, "thumbnail": null, "engine": "bing", "template": "default.html",
            "parsed_url": ["https", "example.test", "/searx", "", "", ""], "img_src": "", "priority": "", "engines": ["bing"],
            "positions": [1], "score": 1.0, "category": "general"
        }],
        "answers": [], "corrections": [], "infoboxes": [], "suggestions": [], "unresponsive_engines": []
    })
}

fn scholarly_config(server: &MockServer) -> ScholarlyConfig {
    ScholarlyConfig {
        providers: None,
        semantic_scholar_base_url: format!("{}/semantic", server.base_url),
        semantic_scholar_api_key: Some("semantic-secret".to_owned()),
        crossref_base_url: format!("{}/crossref", server.base_url),
        openalex_base_url: format!("{}/openalex", server.base_url),
        openalex_api_key: Some("openalex-secret".to_owned()),
        openalex_mailto: Some("test@example.test".to_owned()),
        max_results: 5,
        timeouts: HttpTimeouts::new(Duration::from_secs(1), Duration::from_secs(2)),
    }
}

fn web_config(server: &MockServer) -> WebConfig {
    WebConfig {
        providers: None,
        searxng_base_url: Some(format!("{}/searx", server.base_url)),
        bing_base_url: format!("{}/bing", server.base_url),
        bing_api_key: Some("bing-secret".to_owned()),
        brave_base_url: format!("{}/brave", server.base_url),
        brave_api_key: Some("brave-secret".to_owned()),
        max_results: 5,
        timeouts: HttpTimeouts::new(Duration::from_secs(1), Duration::from_secs(2)),
    }
}

#[tokio::test]
async fn scholarly_provider_override_supports_reverse_order_alias_subset_and_unknown_skipping() {
    // Mutations caught: always use the default chain, reject semantic-scholar alias, or stop on an unknown name.
    let reversed_server = MockServer::spawn(MockMode::ScholarlyYearFiltering, None).await;
    let mut reversed_config = scholarly_config(&reversed_server);
    reversed_config.providers = Some(vec![
        "openalex".to_owned(),
        "crossref".to_owned(),
        "semantic_scholar".to_owned(),
    ]);
    let reversed = ScholarlySearch::new(reversed_config).unwrap();

    let reversed_hits = reversed
        .search(SearchRequest::new("social ties"), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(reversed_hits[0].provider, "openalex");
    assert_eq!(reversed_server.recorded().len(), 1);
    assert!(
        reversed_server.recorded()[0]
            .path_and_query
            .starts_with("/openalex/works?")
    );

    let subset_server = MockServer::spawn(MockMode::ScholarlyYearFiltering, None).await;
    let mut subset_config = scholarly_config(&subset_server);
    subset_config.providers = Some(vec![
        "unknown-provider".to_owned(),
        "semantic-scholar".to_owned(),
    ]);
    let subset = ScholarlySearch::new(subset_config).unwrap();

    let subset_hits = subset
        .search(
            SearchRequest::new("social ties").with_year_range(Some(2024), Some(2025)),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(subset_hits[0].provider, "semantic_scholar");
    assert_eq!(subset_server.recorded().len(), 1);
    assert!(
        subset_server.recorded()[0]
            .path_and_query
            .starts_with("/semantic/paper/search?")
    );
}

#[tokio::test]
async fn scholarly_aggregation_uses_configured_default_limit_below_and_above_five() {
    // Mutation caught: aggregate provider hits with a hardcoded limit of five.
    for expected_limit in [2, 7] {
        let server = MockServer::spawn(MockMode::ScholarlyMany, None).await;
        let mut config = scholarly_config(&server);
        config.providers = Some(vec!["semantic_scholar".to_owned()]);
        config.max_results = expected_limit;
        let search = ScholarlySearch::new(config).unwrap();

        let hits = search
            .search(SearchRequest::new("social ties"), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(hits.len(), expected_limit);
    }
}

#[tokio::test]
async fn web_provider_override_supports_reverse_order_alias_subset_and_unknown_skipping() {
    // Mutations caught: always use the default chain, reject searx alias, or stop on an unknown name.
    let reversed_server = MockServer::spawn(MockMode::WebFallback, None).await;
    let mut reversed_config = web_config(&reversed_server);
    reversed_config.providers = Some(vec![
        "brave".to_owned(),
        "bing".to_owned(),
        "searxng".to_owned(),
    ]);
    let reversed = WebSearch::new(reversed_config).unwrap();

    let reversed_hits = reversed
        .search(SearchRequest::new("social ties"), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(reversed_hits[0].provider, "brave");
    assert_eq!(reversed_server.recorded().len(), 1);
    assert!(
        reversed_server.recorded()[0]
            .path_and_query
            .starts_with("/brave?")
    );

    let subset_server = MockServer::spawn(MockMode::SearxSuccess, None).await;
    let mut subset_config = web_config(&subset_server);
    subset_config.providers = Some(vec!["unknown-provider".to_owned(), "searx".to_owned()]);
    let subset = WebSearch::new(subset_config).unwrap();

    let subset_hits = subset
        .search(SearchRequest::new("social ties"), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(subset_hits[0].provider, "searxng");
    assert_eq!(subset_server.recorded().len(), 1);
    assert!(
        subset_server.recorded()[0]
            .path_and_query
            .starts_with("/searx/search?")
    );
}

#[tokio::test]
async fn scholarly_search_falls_back_and_coordinator_records_safe_provider_failure() {
    // Mutations caught: stop after Semantic Scholar errors, swap provider order, or discard failure metadata.
    let server = MockServer::spawn(MockMode::ScholarlyFallback, None).await;
    let tool: Arc<dyn KnowledgeTool> =
        Arc::new(ScholarlySearch::new(scholarly_config(&server)).unwrap());
    let coordinator = KnowledgeCoordinator::new([tool]);
    let request = SearchRequest::new("搭子社交")
        .with_year_range(Some(2024), Some(2025))
        .with_max_results(5);
    let plan = KnowledgePlan::new([PlannedSearch::new("scholarly", request)]);

    let bundle = coordinator.execute(plan, CancellationToken::new()).await;

    assert_eq!(bundle.hits.len(), 1);
    assert_eq!(bundle.hits[0].provider, "crossref");
    assert_eq!(bundle.metadata.provider_failures.len(), 1);
    assert_eq!(
        bundle.metadata.provider_failures[0].provider,
        "semantic_scholar"
    );
    assert!(
        !bundle.metadata.provider_failures[0]
            .message
            .contains("semantic-secret")
    );
    let requests = server.recorded();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .path_and_query
            .starts_with("/semantic/paper/search?")
    );
    assert!(requests[0].path_and_query.contains("year=2024-2025"));
    assert!(requests[1].path_and_query.starts_with("/crossref/works?"));
    assert!(
        requests[1]
            .path_and_query
            .contains("from-pub-date%3A2024-01-01%2Cuntil-pub-date%3A2025-12-31")
    );
}

#[tokio::test]
async fn coordinator_records_a_fallback_failure_once_for_multiple_hits() {
    // Mutation caught: copy one fallback failure onto every hit and collect every copy.
    let server = MockServer::spawn(MockMode::ScholarlyFallbackMulti, None).await;
    let tool: Arc<dyn KnowledgeTool> =
        Arc::new(ScholarlySearch::new(scholarly_config(&server)).unwrap());
    let coordinator = KnowledgeCoordinator::new([tool]);
    let plan = KnowledgePlan::new([PlannedSearch::new(
        "scholarly",
        SearchRequest::new("搭子社交"),
    )]);

    let bundle = coordinator.execute(plan, CancellationToken::new()).await;

    assert_eq!(bundle.hits.len(), 2);
    assert_eq!(bundle.metadata.provider_failures.len(), 1);
    assert_eq!(
        bundle.metadata.provider_failures[0].provider,
        "semantic_scholar"
    );
}

#[tokio::test]
async fn scholarly_providers_shape_year_queries_and_filter_response_years() {
    // Mutations caught: omit provider-specific year syntax or retain old/unknown-year records.
    let server = MockServer::spawn(MockMode::ScholarlyYearFiltering, None).await;
    let config = scholarly_config(&server);
    let request = SearchRequest::new("social ties")
        .with_year_range(Some(2024), Some(2025))
        .with_max_results(5);
    let cancel = CancellationToken::new();

    let semantic = writing_coach_server::tools::scholarly::SemanticScholar::new(&config).unwrap();
    let crossref = writing_coach_server::tools::scholarly::Crossref::new(&config).unwrap();
    let openalex = writing_coach_server::tools::scholarly::OpenAlex::new(&config).unwrap();
    let semantic_hits = semantic
        .search(request.clone(), cancel.clone())
        .await
        .unwrap();
    let crossref_hits = crossref
        .search(request.clone(), cancel.clone())
        .await
        .unwrap();
    let openalex_hits = openalex.search(request, cancel).await.unwrap();

    assert_eq!(
        semantic_hits.iter().map(|hit| hit.year).collect::<Vec<_>>(),
        [Some(2024)]
    );
    assert_eq!(
        crossref_hits.iter().map(|hit| hit.year).collect::<Vec<_>>(),
        [Some(2024)]
    );
    assert_eq!(
        openalex_hits.iter().map(|hit| hit.year).collect::<Vec<_>>(),
        [Some(2025)]
    );
    let requests = server.recorded();
    assert!(requests[0].path_and_query.contains("year=2024-2025"));
    assert!(
        requests[1]
            .path_and_query
            .contains("query.bibliographic=social+ties")
    );
    assert!(
        requests[2]
            .path_and_query
            .contains("from_publication_date%3A2024-01-01%2Cto_publication_date%3A2025-12-31")
    );
    assert!(
        requests[2]
            .path_and_query
            .contains("sort=relevance_score%3Adesc")
    );
}

#[tokio::test]
async fn web_search_uses_searxng_bing_brave_fallback_order_and_auth_headers() {
    // Mutations caught: reorder/stop the fallback chain, treat an empty provider as terminal,
    // or put the wrong provider credential/header on a request.
    let server = MockServer::spawn(MockMode::WebFallback, None).await;
    let search = WebSearch::new(web_config(&server)).unwrap();

    let hits = search
        .search(SearchRequest::new("social ties"), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].provider, "brave");
    assert_eq!(hits[0].source, "Example");
    let requests = server.recorded();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].path_and_query.starts_with("/searx/search?"));
    assert!(requests[0].path_and_query.contains("format=json"));
    assert!(requests[0].path_and_query.contains("language=zh-CN"));
    assert!(requests[1].path_and_query.starts_with("/bing?"));
    assert_eq!(requests[1].bing_key.as_deref(), Some("bing-secret"));
    assert!(requests[2].path_and_query.starts_with("/brave?"));
    assert_eq!(requests[2].brave_key.as_deref(), Some("brave-secret"));
    let failures = hits[0].metadata["provider_failures"].as_array().unwrap();
    assert_eq!(failures[0]["provider"], "searxng");
}

#[tokio::test]
async fn searxng_search_parses_complete_json_result_shape() {
    // Mutation caught: use Bing/Brave field names for the SearXNG result parser.
    let server = MockServer::spawn(MockMode::SearxSuccess, None).await;
    let search = WebSearch::new(web_config(&server)).unwrap();

    let hits = search
        .search(SearchRequest::new("social ties"), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].provider, "searxng");
    assert_eq!(hits[0].title, "Searx result");
    assert_eq!(hits[0].source, "bing");
    assert_eq!(hits[0].text, "A result.");
}

#[tokio::test]
async fn bing_search_parses_complete_web_pages_result_shape() {
    // Mutation caught: use SearXNG/Brave field names for the Bing result parser.
    let server = MockServer::spawn(MockMode::BingSuccess, None).await;
    let search = WebSearch::new(web_config(&server)).unwrap();

    let hits = search
        .search(SearchRequest::new("social ties"), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].provider, "bing");
    assert_eq!(hits[0].title, "Bing social ties result");
    assert_eq!(hits[0].source, "Example University");
    assert_eq!(hits[0].text, "A complete Bing fixture.");
}

#[tokio::test]
async fn cancellation_after_provider_error_prevents_the_fallback_request() {
    // Mutation caught: only check cancellation once before the chain and still issue Crossref fallback.
    let cancel = CancellationToken::new();
    let server = MockServer::spawn(MockMode::CancelAfterSemantic, Some(cancel.clone())).await;
    let search = ScholarlySearch::new(scholarly_config(&server)).unwrap();

    let error = search
        .search(SearchRequest::new("搭子社交"), cancel)
        .await
        .unwrap_err();

    assert!(matches!(error, ToolError::Cancelled));
    assert_eq!(server.recorded().len(), 1);
}

#[tokio::test]
async fn cancelled_web_search_makes_no_external_request() {
    // Mutation caught: provider does not check an already-cancelled token before sending HTTP.
    let server = MockServer::spawn(MockMode::WebFallback, None).await;
    let search = WebSearch::new(web_config(&server)).unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();

    let error = search
        .search(SearchRequest::new("social ties"), cancel)
        .await
        .unwrap_err();

    assert!(matches!(error, ToolError::Cancelled));
    assert!(server.recorded().is_empty());
}
