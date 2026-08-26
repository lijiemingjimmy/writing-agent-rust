use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use reqwest::{Client, header::HeaderMap};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    HttpTimeouts, KnowledgeTool, SearchHit, SearchRequest, ToolError,
    knowledge::{attach_failures, check_cancel, get_json, http_client},
};

#[derive(Clone)]
pub struct ScholarlyConfig {
    pub providers: Option<Vec<String>>,
    pub semantic_scholar_base_url: String,
    pub semantic_scholar_api_key: Option<String>,
    pub crossref_base_url: String,
    pub openalex_base_url: String,
    pub openalex_api_key: Option<String>,
    pub openalex_mailto: Option<String>,
    pub max_results: usize,
    pub timeouts: HttpTimeouts,
}

pub struct SemanticScholar {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    max_results: usize,
}

pub struct Crossref {
    client: Client,
    base_url: String,
    mailto: Option<String>,
    max_results: usize,
}

pub struct OpenAlex {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    mailto: Option<String>,
    max_results: usize,
}

pub struct ScholarlySearch {
    providers: Vec<Arc<dyn KnowledgeTool>>,
}

impl SemanticScholar {
    pub fn new(config: &ScholarlyConfig) -> Result<Self, ToolError> {
        Ok(Self {
            client: http_client("semantic_scholar", config.timeouts)?,
            base_url: config
                .semantic_scholar_base_url
                .trim_end_matches('/')
                .to_owned(),
            api_key: config.semantic_scholar_api_key.clone(),
            max_results: config.max_results,
        })
    }

    async fn search_one(
        &self,
        query: &str,
        request: &SearchRequest,
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        let fields = "title,year,authors,venue,externalIds,url,citationCount,abstract";
        let mut params = vec![
            ("query", query.to_owned()),
            ("limit", request.limit_or(self.max_results).to_string()),
            ("fields", fields.to_owned()),
        ];
        if request.year_from.is_some() || request.year_to.is_some() {
            let start = request.year_from.unwrap_or(1900);
            let end = request.year_to.unwrap_or(chrono_year());
            params.push(("year", format!("{start}-{end}")));
        }
        let mut headers = HeaderMap::new();
        if let Some(api_key) = &self.api_key {
            headers.insert(
                "x-api-key",
                api_key
                    .parse()
                    .map_err(|_| ToolError::provider(self.name(), "invalid API key header"))?,
            );
        }
        let payload = get_json(
            &self.client,
            self.name(),
            format!("{}/paper/search", self.base_url),
            &params,
            headers,
            cancel,
        )
        .await?;
        Ok(payload
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(parse_semantic_paper)
            .collect())
    }
}

#[async_trait]
impl KnowledgeTool for SemanticScholar {
    fn name(&self) -> &'static str {
        "semantic_scholar"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        merge_queries(
            self,
            &request,
            &cancel,
            self.max_results,
            |provider, query, request, cancel| {
                Box::pin(provider.search_one(query, request, cancel))
            },
        )
        .await
    }
}

impl Crossref {
    pub fn new(config: &ScholarlyConfig) -> Result<Self, ToolError> {
        Ok(Self {
            client: http_client("crossref", config.timeouts)?,
            base_url: config.crossref_base_url.trim_end_matches('/').to_owned(),
            mailto: config.openalex_mailto.clone(),
            max_results: config.max_results,
        })
    }

    async fn search_one(
        &self,
        query: &str,
        request: &SearchRequest,
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        let mut params = vec![
            ("query.bibliographic", query.to_owned()),
            ("rows", request.limit_or(self.max_results).to_string()),
            ("sort", "relevance".to_owned()),
        ];
        if let Some(start) = request.year_from {
            let mut filter = format!("from-pub-date:{start}-01-01");
            if let Some(end) = request.year_to {
                filter.push_str(&format!(",until-pub-date:{end}-12-31"));
            }
            params.push(("filter", filter));
        } else if let Some(end) = request.year_to {
            params.push(("filter", format!("until-pub-date:{end}-12-31")));
        }
        if let Some(mailto) = &self.mailto {
            params.push(("mailto", mailto.clone()));
        }
        let payload = get_json(
            &self.client,
            self.name(),
            format!("{}/works", self.base_url),
            &params,
            HeaderMap::new(),
            cancel,
        )
        .await?;
        Ok(payload
            .pointer("/message/items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(parse_crossref_work)
            .collect())
    }
}

#[async_trait]
impl KnowledgeTool for Crossref {
    fn name(&self) -> &'static str {
        "crossref"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        merge_queries(
            self,
            &request,
            &cancel,
            self.max_results,
            |provider, query, request, cancel| {
                Box::pin(provider.search_one(query, request, cancel))
            },
        )
        .await
    }
}

impl OpenAlex {
    pub fn new(config: &ScholarlyConfig) -> Result<Self, ToolError> {
        Ok(Self {
            client: http_client("openalex", config.timeouts)?,
            base_url: config.openalex_base_url.trim_end_matches('/').to_owned(),
            api_key: config.openalex_api_key.clone(),
            mailto: config.openalex_mailto.clone(),
            max_results: config.max_results,
        })
    }

    async fn search_one(
        &self,
        query: &str,
        request: &SearchRequest,
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        let mut params = vec![
            ("search", query.to_owned()),
            ("per-page", request.limit_or(self.max_results).to_string()),
            ("sort", "relevance_score:desc".to_owned()),
        ];
        let mut filters = Vec::new();
        if let Some(start) = request.year_from {
            filters.push(format!("from_publication_date:{start}-01-01"));
        }
        if let Some(end) = request.year_to {
            filters.push(format!("to_publication_date:{end}-12-31"));
        }
        if !filters.is_empty() {
            params.push(("filter", filters.join(",")));
        }
        if let Some(api_key) = &self.api_key {
            params.push(("api_key", api_key.clone()));
        }
        if let Some(mailto) = &self.mailto {
            params.push(("mailto", mailto.clone()));
        }
        let payload = get_json(
            &self.client,
            self.name(),
            format!("{}/works", self.base_url),
            &params,
            HeaderMap::new(),
            cancel,
        )
        .await?;
        Ok(payload
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(parse_openalex_work)
            .collect())
    }
}

#[async_trait]
impl KnowledgeTool for OpenAlex {
    fn name(&self) -> &'static str {
        "openalex"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
        merge_queries(
            self,
            &request,
            &cancel,
            self.max_results,
            |provider, query, request, cancel| {
                Box::pin(provider.search_one(query, request, cancel))
            },
        )
        .await
    }
}

impl ScholarlySearch {
    pub fn new(config: ScholarlyConfig) -> Result<Self, ToolError> {
        let provider_names = config.providers.clone().unwrap_or_else(|| {
            vec![
                "semantic_scholar".to_owned(),
                "crossref".to_owned(),
                "openalex".to_owned(),
            ]
        });
        let mut providers: Vec<Arc<dyn KnowledgeTool>> = Vec::new();
        for provider_name in provider_names {
            match provider_name.trim().to_ascii_lowercase().as_str() {
                "semantic_scholar" | "semantic-scholar" => {
                    providers.push(Arc::new(SemanticScholar::new(&config)?));
                }
                "crossref" => providers.push(Arc::new(Crossref::new(&config)?)),
                "openalex" => providers.push(Arc::new(OpenAlex::new(&config)?)),
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
impl KnowledgeTool for ScholarlySearch {
    fn name(&self) -> &'static str {
        "scholarly"
    }

    async fn search(
        &self,
        request: SearchRequest,
        cancel: CancellationToken,
    ) -> Result<Vec<SearchHit>, ToolError> {
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

type SearchFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Vec<SearchHit>, ToolError>> + Send + 'a>,
>;

async fn merge_queries<'a, T, F>(
    provider: &'a T,
    request: &'a SearchRequest,
    cancel: &'a CancellationToken,
    default_limit: usize,
    search_one: F,
) -> Result<Vec<SearchHit>, ToolError>
where
    T: KnowledgeTool + Sync,
    F: Fn(&'a T, &'a str, &'a SearchRequest, &'a CancellationToken) -> SearchFuture<'a>,
{
    let limit = request.limit_or(default_limit);
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    for query in request.query_terms.iter().take(3) {
        check_cancel(cancel)?;
        for hit in search_one(provider, query, request, cancel).await? {
            if !in_year_range(hit.year, request) {
                continue;
            }
            let key = hit
                .doi
                .clone()
                .or_else(|| hit.url.clone())
                .unwrap_or_else(|| hit.title.clone());
            if !seen.insert(key) {
                continue;
            }
            hits.push(hit);
            if hits.len() >= limit {
                return Ok(hits);
            }
        }
    }
    Ok(hits)
}

fn in_year_range(year: Option<i32>, request: &SearchRequest) -> bool {
    if request.year_from.is_none() && request.year_to.is_none() {
        return true;
    }
    let Some(year) = year else {
        return false;
    };
    request.year_from.is_none_or(|start| year >= start)
        && request.year_to.is_none_or(|end| year <= end)
}

fn parse_semantic_paper(item: &Value) -> SearchHit {
    let doi = item
        .pointer("/externalIds/DOI")
        .and_then(Value::as_str)
        .map(|doi| {
            if doi.starts_with("http") {
                doi.to_owned()
            } else {
                format!("https://doi.org/{doi}")
            }
        });
    SearchHit {
        source: string(item.get("venue")),
        title: string_or(item.get("title"), "Untitled"),
        text: string(item.get("abstract")),
        provider: "semantic_scholar".to_owned(),
        year: integer(item.get("year")),
        authors: authors(item.get("authors"), "/name"),
        url: item.get("url").and_then(Value::as_str).map(str::to_owned),
        doi,
        cited_by_count: item.get("citationCount").and_then(Value::as_i64),
        ..SearchHit::default()
    }
}

fn parse_crossref_work(item: &Value) -> SearchHit {
    let raw_doi = item.get("DOI").and_then(Value::as_str);
    let doi = raw_doi.map(|doi| format!("https://doi.org/{doi}"));
    let authors = item
        .get("author")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(3)
        .filter_map(|author| {
            let name = [string(author.get("given")), string(author.get("family"))]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (!name.is_empty()).then_some(name)
        })
        .collect();
    let abstract_text = item
        .get("abstract")
        .and_then(Value::as_str)
        .map(strip_html)
        .unwrap_or_default();
    SearchHit {
        source: first_string(item.get("container-title")),
        title: first_string_or(item.get("title"), "Untitled"),
        text: abstract_text,
        provider: "crossref".to_owned(),
        year: item
            .pointer("/issued/date-parts/0/0")
            .and_then(Value::as_i64)
            .and_then(|year| i32::try_from(year).ok()),
        authors,
        url: item
            .get("URL")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| doi.clone()),
        doi,
        cited_by_count: item.get("is-referenced-by-count").and_then(Value::as_i64),
        ..SearchHit::default()
    }
}

fn parse_openalex_work(item: &Value) -> SearchHit {
    let source = item
        .pointer("/primary_location/source/display_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    SearchHit {
        source,
        title: string_or(item.get("display_name"), "Untitled"),
        text: abstract_from_inverted_index(item.get("abstract_inverted_index")),
        provider: "openalex".to_owned(),
        year: integer(item.get("publication_year")),
        authors: authors(item.get("authorships"), "/author/display_name"),
        url: item
            .pointer("/primary_location/landing_page_url")
            .and_then(Value::as_str)
            .or_else(|| item.get("doi").and_then(Value::as_str))
            .or_else(|| item.get("id").and_then(Value::as_str))
            .map(str::to_owned),
        doi: item.get("doi").and_then(Value::as_str).map(str::to_owned),
        cited_by_count: item.get("cited_by_count").and_then(Value::as_i64),
        metadata: item
            .get("id")
            .and_then(Value::as_str)
            .map(|id| {
                [("openalex_id".to_owned(), Value::String(id.to_owned()))]
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default(),
        ..SearchHit::default()
    }
}

fn authors(value: Option<&Value>, pointer: &str) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(3)
        .filter_map(|author| author.pointer(pointer).and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

fn abstract_from_inverted_index(value: Option<&Value>) -> String {
    let Some(index) = value.and_then(Value::as_object) else {
        return String::new();
    };
    let mut words = index
        .iter()
        .flat_map(|(word, positions)| {
            positions
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .map(move |position| (position, word.as_str()))
        })
        .collect::<Vec<_>>();
    words.sort_by_key(|(position, _)| *position);
    words
        .into_iter()
        .map(|(_, word)| word)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(600)
        .collect()
}

fn strip_html(input: &str) -> String {
    let mut output = String::new();
    let mut inside_tag = false;
    for character in input.chars() {
        match character {
            '<' => inside_tag = true,
            '>' => {
                inside_tag = false;
                output.push(' ');
            }
            _ if !inside_tag => output.push(character),
            _ => {}
        }
    }
    output
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(600)
        .collect()
}

fn first_string(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn first_string_or(value: Option<&Value>, fallback: &str) -> String {
    let value = first_string(value);
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

fn string(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn string_or(value: Option<&Value>, fallback: &str) -> String {
    value.and_then(Value::as_str).unwrap_or(fallback).to_owned()
}

fn integer(value: Option<&Value>) -> Option<i32> {
    value
        .and_then(Value::as_i64)
        .and_then(|number| i32::try_from(number).ok())
}

fn chrono_year() -> i32 {
    let unix_days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| (duration.as_secs() / 86_400) as i64)
        .unwrap_or_default();
    utc_year_from_unix_days(unix_days)
}

fn utc_year_from_unix_days(unix_days: i64) -> i32 {
    // Howard Hinnant's civil-from-days conversion, with 1970-01-01 as day zero.
    let shifted = unix_days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    i32::try_from(year).unwrap_or(1970)
}

#[cfg(test)]
mod tests {
    use super::utc_year_from_unix_days;

    #[test]
    fn open_ended_year_filter_uses_the_real_gregorian_year() {
        // Mutation caught: use a fixed future upper bound for Semantic Scholar year ranges.
        assert_eq!(utc_year_from_unix_days(0), 1970);
        assert_eq!(utc_year_from_unix_days(10_957), 2000);
        assert_eq!(utc_year_from_unix_days(20_454), 2026);
    }
}
