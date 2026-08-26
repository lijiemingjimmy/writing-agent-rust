use std::sync::Arc;

use async_trait::async_trait;
use reqwest::{Client, header::HeaderMap};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    HttpTimeouts, KnowledgeTool, SearchHit, SearchRequest, ToolError,
    knowledge::{attach_failures, check_cancel, get_json, http_client},
};

#[derive(Clone)]
pub struct WebConfig {
    pub providers: Option<Vec<String>>,
    pub searxng_base_url: Option<String>,
    pub bing_base_url: String,
    pub bing_api_key: Option<String>,
    pub brave_base_url: String,
    pub brave_api_key: Option<String>,
    pub max_results: usize,
    pub timeouts: HttpTimeouts,
}

pub struct WebSearch {
    providers: Vec<Arc<dyn KnowledgeTool>>,
}

struct Searxng {
    client: Client,
    base_url: String,
    max_results: usize,
}

struct Bing {
    client: Client,
    base_url: String,
    api_key: String,
    max_results: usize,
}

struct Brave {
    client: Client,
    base_url: String,
    api_key: String,
    max_results: usize,
}

impl WebSearch {
    pub fn new(config: WebConfig) -> Result<Self, ToolError> {
        let mut providers: Vec<Arc<dyn KnowledgeTool>> = Vec::new();
        let provider_names = config
            .providers
            .clone()
            .unwrap_or_else(|| vec!["searxng".to_owned(), "bing".to_owned(), "brave".to_owned()]);
        for provider_name in provider_names {
            match provider_name.trim().to_ascii_lowercase().as_str() {
                "searxng" | "searx" => {
                    if let Some(base_url) = config
                        .searxng_base_url
                        .as_ref()
                        .filter(|url| !url.trim().is_empty())
                    {
                        providers.push(Arc::new(Searxng {
                            client: http_client("searxng", config.timeouts)?,
                            base_url: base_url.trim_end_matches('/').to_owned(),
                            max_results: config.max_results,
                        }));
                    }
                }
                "bing" => {
                    if let Some(api_key) =
                        config.bing_api_key.as_ref().filter(|key| !key.is_empty())
                    {
                        providers.push(Arc::new(Bing {
                            client: http_client("bing", config.timeouts)?,
                            base_url: config.bing_base_url.clone(),
                            api_key: api_key.clone(),
                            max_results: config.max_results,
                        }));
                    }
                }
                "brave" => {
                    if let Some(api_key) =
                        config.brave_api_key.as_ref().filter(|key| !key.is_empty())
                    {
                        providers.push(Arc::new(Brave {
                            client: http_client("brave", config.timeouts)?,
                            base_url: config.brave_base_url.clone(),
                            api_key: api_key.clone(),
                            max_results: config.max_results,
                        }));
                    }
                }
                _ => {}
            }
        }
        Ok(Self { providers })
    }

    pub fn is_configured(&self) -> bool {
        !self.providers.is_empty()
    }
}

#[async_trait]
impl KnowledgeTool for WebSearch {
    fn name(&self) -> &'static str {
        "web"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        if self.providers.is_empty() {
            return Err(ToolError::provider(self.name(), "web_search_unconfigured"));
        }
        let mut failures = Vec::new();
        for provider in &self.providers {
            check_cancel(&cancel)?;
            match provider.search(request.clone(), cancel.clone()).await {
                Ok(mut hits) if !hits.is_empty() => {
                    attach_failures(&mut hits, &failures);
                    return Ok(hits);
                }
                Ok(_) => {}
                Err(ToolError::Cancelled) => return Err(ToolError::Cancelled),
                Err(error) => failures.extend(error.into_failures(provider.name())),
            }
        }
        if failures.is_empty() {
            Ok(Vec::new())
        } else {
            Err(ToolError::ProvidersExhausted {
                tool: self.name().to_owned(),
                failures,
            })
        }
    }
}

#[async_trait]
impl KnowledgeTool for Searxng {
    fn name(&self) -> &'static str {
        "searxng"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        let query = request
            .query_terms
            .first()
            .map(String::as_str)
            .unwrap_or("");
        let limit = request.limit_or(self.max_results);
        let params = [
            ("q", query.to_owned()),
            ("format", "json".to_owned()),
            ("language", "zh-CN".to_owned()),
        ];
        let payload = get_json(
            &self.client,
            self.name(),
            format!("{}/search", self.base_url),
            &params,
            HeaderMap::new(),
            &cancel,
        )
        .await?;
        Ok(payload
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| web_hit(item, "title", "content", "engine", self.name()))
            .take(limit)
            .collect())
    }
}

#[async_trait]
impl KnowledgeTool for Bing {
    fn name(&self) -> &'static str {
        "bing"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        let query = request
            .query_terms
            .first()
            .map(String::as_str)
            .unwrap_or("");
        let limit = request.limit_or(self.max_results);
        let params = [
            ("q", query.to_owned()),
            ("count", limit.to_string()),
            ("mkt", "zh-CN".to_owned()),
        ];
        let mut headers = HeaderMap::new();
        headers.insert(
            "ocp-apim-subscription-key",
            self.api_key
                .parse()
                .map_err(|_| ToolError::provider(self.name(), "invalid API key header"))?,
        );
        let payload = get_json(
            &self.client,
            self.name(),
            self.base_url.clone(),
            &params,
            headers,
            &cancel,
        )
        .await?;
        Ok(payload
            .pointer("/webPages/value")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| web_hit(item, "name", "snippet", "siteName", self.name()))
            .take(limit)
            .collect())
    }
}

#[async_trait]
impl KnowledgeTool for Brave {
    fn name(&self) -> &'static str {
        "brave"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        let query = request
            .query_terms
            .first()
            .map(String::as_str)
            .unwrap_or("");
        let limit = request.limit_or(self.max_results);
        let params = [
            ("q", query.to_owned()),
            ("count", limit.to_string()),
            ("search_lang", "zh-hans".to_owned()),
        ];
        let mut headers = HeaderMap::new();
        headers.insert("accept", "application/json".parse().unwrap());
        headers.insert(
            "x-subscription-token",
            self.api_key
                .parse()
                .map_err(|_| ToolError::provider(self.name(), "invalid API key header"))?,
        );
        let payload = get_json(
            &self.client,
            self.name(),
            self.base_url.clone(),
            &params,
            headers,
            &cancel,
        )
        .await?;
        Ok(payload
            .pointer("/web/results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let mut hit = web_hit(item, "title", "description", "", self.name())?;
                hit.source = item
                    .pointer("/profile/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                Some(hit)
            })
            .take(limit)
            .collect())
    }
}

fn web_hit(
    item: &Value,
    title_field: &str,
    snippet_field: &str,
    source_field: &str,
    provider: &str,
) -> Option<SearchHit> {
    let title = item.get(title_field)?.as_str()?;
    let url = item.get("url")?.as_str()?;
    Some(SearchHit {
        source: item
            .get(source_field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        title: title.to_owned(),
        text: item
            .get(snippet_field)
            .or_else(|| item.get("snippet"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        provider: provider.to_owned(),
        url: Some(url.to_owned()),
        ..SearchHit::default()
    })
}
