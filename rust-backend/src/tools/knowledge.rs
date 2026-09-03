use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::{Client, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::domain::SessionId;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub source: String,
    pub title: String,
    pub heading: String,
    pub text: String,
    pub score: i32,
    pub provider: String,
    pub year: Option<i32>,
    pub authors: Vec<String>,
    pub url: Option<String>,
    pub doi: Option<String>,
    pub cited_by_count: Option<i64>,
    pub metadata: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRequest {
    pub query_terms: Vec<String>,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub topic_context: Option<String>,
    pub max_results: Option<usize>,
    pub session_id: Option<SessionId>,
    pub target_skill_id: Option<String>,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query_terms: vec![query.into()],
            year_from: None,
            year_to: None,
            topic_context: None,
            max_results: None,
            session_id: None,
            target_skill_id: None,
        }
    }

    pub fn from_terms(query_terms: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            query_terms: query_terms.into_iter().map(Into::into).collect(),
            year_from: None,
            year_to: None,
            topic_context: None,
            max_results: None,
            session_id: None,
            target_skill_id: None,
        }
    }

    pub fn with_year_range(mut self, year_from: Option<i32>, year_to: Option<i32>) -> Self {
        self.year_from = year_from;
        self.year_to = year_to;
        self
    }

    pub fn with_max_results(mut self, max_results: usize) -> Self {
        self.max_results = Some(max_results);
        self
    }

    pub fn with_session_id(mut self, session_id: SessionId) -> Self {
        self.session_id = Some(session_id);
        self
    }

    pub fn with_target_skill_id(mut self, target_skill_id: impl Into<String>) -> Self {
        self.target_skill_id = Some(target_skill_id.into());
        self
    }

    pub fn limit_or(&self, default: usize) -> usize {
        self.max_results.unwrap_or(default)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFailure {
    pub provider: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("knowledge search cancelled")]
    Cancelled,
    #[error("local knowledge search failed: {0}")]
    Local(String),
    #[error("{provider} provider failed: {message}")]
    Provider { provider: String, message: String },
    #[error("{tool} providers exhausted")]
    ProvidersExhausted {
        tool: String,
        failures: Vec<ProviderFailure>,
    },
}

impl ToolError {
    pub(crate) fn provider(provider: &str, message: impl Into<String>) -> Self {
        Self::Provider {
            provider: provider.to_owned(),
            message: message.into(),
        }
    }

    pub(crate) fn into_failures(self, fallback_name: &str) -> Vec<ProviderFailure> {
        match self {
            Self::Provider { provider, message } => {
                vec![normalize_provider_failure(&provider, &message)]
            }
            Self::ProvidersExhausted { failures, .. } => failures
                .into_iter()
                .map(|failure| normalize_provider_failure(&failure.provider, &failure.message))
                .collect(),
            Self::Cancelled => vec![normalize_provider_failure(fallback_name, "cancelled")],
            Self::Local(_) => vec![normalize_provider_failure(fallback_name, "local_error")],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HttpTimeouts {
    pub connect: Duration,
    pub read: Duration,
}

impl HttpTimeouts {
    pub fn new(connect: Duration, read: Duration) -> Self {
        Self { connect, read }
    }
}

impl Default for HttpTimeouts {
    fn default() -> Self {
        Self::new(Duration::from_secs(3), Duration::from_secs(10))
    }
}

#[async_trait]
pub trait KnowledgeTool: Send + Sync {
    fn name(&self) -> &'static str;

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError>;
}

#[derive(Clone, Debug)]
pub struct PlannedSearch {
    pub tool: String,
    pub request: SearchRequest,
}

impl PlannedSearch {
    pub fn new(tool: impl Into<String>, request: SearchRequest) -> Self {
        Self {
            tool: tool.into(),
            request,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct KnowledgePlan {
    pub searches: Vec<PlannedSearch>,
}

impl KnowledgePlan {
    pub fn new(searches: impl IntoIterator<Item = PlannedSearch>) -> Self {
        Self {
            searches: searches.into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KnowledgeMetadata {
    pub provider_failures: Vec<ProviderFailure>,
    pub cancelled: bool,
}

#[derive(Clone, Debug, Default)]
pub struct KnowledgeBundle {
    pub hits: Vec<SearchHit>,
    pub metadata: KnowledgeMetadata,
}

#[derive(Clone)]
pub struct KnowledgeCoordinator {
    tools: BTreeMap<&'static str, Arc<dyn KnowledgeTool>>,
}

impl KnowledgeCoordinator {
    pub fn new(tools: impl IntoIterator<Item = Arc<dyn KnowledgeTool>>) -> Self {
        Self {
            tools: tools.into_iter().map(|tool| (tool.name(), tool)).collect(),
        }
    }

    pub fn with_additional_tools(
        &self,
        tools: impl IntoIterator<Item = Arc<dyn KnowledgeTool>>,
    ) -> Self {
        let mut combined = self.tools.clone();
        combined.extend(tools.into_iter().map(|tool| (tool.name(), tool)));
        Self { tools: combined }
    }

    pub fn has_tool(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub async fn execute(&self, plan: KnowledgePlan, cancel: CancellationToken) -> KnowledgeBundle {
        let mut bundle = KnowledgeBundle::default();
        for search in plan.searches {
            if cancel.is_cancelled() {
                bundle.metadata.cancelled = true;
                break;
            }
            let Some(tool) = self.tools.get(search.tool.as_str()) else {
                bundle
                    .metadata
                    .provider_failures
                    .push(normalize_provider_failure(&search.tool, "not configured"));
                continue;
            };
            match tool.search(search.request, cancel.clone()).await {
                Ok(hits) => {
                    for hit in &hits {
                        collect_hit_failures(hit, &mut bundle.metadata.provider_failures);
                    }
                    bundle.hits.extend(hits);
                }
                Err(ToolError::Cancelled) => {
                    bundle.metadata.cancelled = true;
                    break;
                }
                Err(error) => bundle
                    .metadata
                    .provider_failures
                    .extend(error.into_failures(tool.name())),
            }
        }
        bundle
    }
}

fn collect_hit_failures(hit: &SearchHit, failures: &mut Vec<ProviderFailure>) {
    let Some(items) = hit
        .metadata
        .get("provider_failures")
        .and_then(Value::as_array)
    else {
        return;
    };
    for item in items {
        if let Ok(failure) = serde_json::from_value::<ProviderFailure>(item.clone()) {
            let failure = normalize_provider_failure(&failure.provider, &failure.message);
            if !failures.contains(&failure) {
                failures.push(failure);
            }
        }
    }
}

fn normalize_provider_failure(provider: &str, message: &str) -> ProviderFailure {
    let mut provider = provider
        .chars()
        .take(48)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if provider.trim_matches('_').is_empty() {
        provider = "provider".to_owned();
    }
    let normalized = message
        .chars()
        .take(256)
        .map(|character| character.to_ascii_lowercase())
        .collect::<String>();
    let message = if matches!(
        normalized.as_str(),
        "cancelled"
            | "timeout"
            | "not_configured"
            | "http_error"
            | "local_error"
            | "provider_error"
    ) {
        normalized.as_str()
    } else if normalized.contains("cancel") {
        "cancelled"
    } else if normalized.contains("timeout") || normalized.contains("timed out") {
        "timeout"
    } else if normalized.contains("not configured") || normalized.contains("unconfigured") {
        "not_configured"
    } else if normalized.contains("http status") {
        "http_error"
    } else if normalized == "local_error" {
        "local_error"
    } else {
        "provider_error"
    };
    ProviderFailure {
        provider,
        message: message.to_owned(),
    }
}

pub(crate) fn check_cancel(cancel: &CancellationToken) -> Result<(), ToolError> {
    if cancel.is_cancelled() {
        Err(ToolError::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) fn attach_failures(hits: &mut [SearchHit], failures: &[ProviderFailure]) {
    if failures.is_empty() {
        return;
    }
    let failures = failures
        .iter()
        .map(|failure| normalize_provider_failure(&failure.provider, &failure.message))
        .collect::<Vec<_>>();
    let value = serde_json::to_value(failures).expect("provider failures serialize");
    for hit in hits {
        hit.metadata
            .insert("provider_failures".to_owned(), value.clone());
    }
}

pub(crate) fn http_client(provider: &str, timeouts: HttpTimeouts) -> Result<Client, ToolError> {
    Client::builder()
        // Provider URLs are explicit application configuration. Ignoring ambient
        // machine proxies keeps loopback/private deployments deterministic and
        // avoids silently sending course queries to an unrelated system proxy.
        .no_proxy()
        .connect_timeout(timeouts.connect)
        .timeout(timeouts.read)
        .user_agent("writing-agent-rust/0.1")
        .build()
        .map_err(|_| ToolError::provider(provider, "could not build HTTP client"))
}

pub(crate) async fn get_json(
    client: &Client,
    provider: &str,
    url: String,
    params: &[(&str, String)],
    headers: HeaderMap,
    cancel: &CancellationToken,
) -> Result<Value, ToolError> {
    check_cancel(cancel)?;
    let request = client.get(url).query(params).headers(headers);
    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(ToolError::Cancelled),
        response = request.send() => response.map_err(|error| safe_request_error(provider, &error))?,
    };
    let status = response.status();
    if !status.is_success() {
        return Err(ToolError::provider(
            provider,
            format!("HTTP status {}", status.as_u16()),
        ));
    }
    tokio::select! {
        _ = cancel.cancelled() => Err(ToolError::Cancelled),
        payload = response.json::<Value>() => payload.map_err(|_| ToolError::provider(provider, "invalid JSON response")),
    }
}

fn safe_request_error(provider: &str, error: &reqwest::Error) -> ToolError {
    let message = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    ToolError::provider(provider, message)
}
