use std::{
    env, fs,
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};

use serde::Deserialize;

use crate::AppError;

#[derive(Clone, Debug, Deserialize)]
pub struct AppConfig {
    pub bind_addr: SocketAddr,
    pub database_url: String,
    pub skill_root: String,
    pub corpus_root: String,
    #[serde(default)]
    pub local_corpus_root: Option<String>,
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,
    pub model: ModelConfig,
    pub run_defaults: RunDefaults,
    #[serde(default)]
    pub knowledge: KnowledgeConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default = "default_context_max_chars")]
    pub conversation_context_max_chars: usize,
    #[serde(default = "default_context_recent_chars")]
    pub conversation_context_recent_chars: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    pub teacher_access_token: Option<String>,
    pub student_token_pepper: String,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            teacher_access_token: None,
            student_token_pepper: "development-only-change-me".to_owned(),
        }
    }
}

fn default_context_max_chars() -> usize {
    24_000
}

fn default_context_recent_chars() -> usize {
    12_000
}

#[derive(Clone, Debug, Deserialize)]
pub struct ModelConfig {
    pub provider: String,
    pub endpoint: String,
    pub name: String,
    pub api_key_env: String,
    pub context_length: u32,
    pub max_output_tokens: u32,
    pub reasoning_mode: String,
    pub input_price_microusd_per_million: u64,
    pub output_price_microusd_per_million: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunDefaults {
    pub max_input_tokens: u32,
    pub max_output_tokens: u32,
    pub max_cost_microusd: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KnowledgeConfig {
    pub scholarly: Option<ScholarlyProviderConfig>,
    pub web: Option<WebProviderConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScholarlyProviderConfig {
    pub providers: Option<Vec<String>>,
    pub semantic_scholar_base_url: Option<String>,
    pub semantic_scholar_api_key_env: Option<String>,
    pub crossref_base_url: Option<String>,
    pub openalex_base_url: Option<String>,
    pub openalex_api_key_env: Option<String>,
    pub openalex_mailto: Option<String>,
    pub max_results: Option<usize>,
    pub connect_timeout_seconds: Option<u64>,
    pub read_timeout_seconds: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebProviderConfig {
    pub providers: Option<Vec<String>>,
    pub searxng_base_url: Option<String>,
    pub bing_base_url: Option<String>,
    pub bing_api_key_env: Option<String>,
    pub brave_base_url: Option<String>,
    pub brave_api_key_env: Option<String>,
    pub max_results: Option<usize>,
    pub connect_timeout_seconds: Option<u64>,
    pub read_timeout_seconds: Option<u64>,
}

impl AppConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let source = fs::read_to_string(path)?;
        Self::from_toml(&source)
    }

    pub fn from_toml(source: &str) -> Result<Self, AppError> {
        let config: Self = toml::from_str(source)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), AppError> {
        crate::llm::ModelSettingsStore::new_with_run_defaults(
            self.model.clone(),
            self.run_defaults.clone(),
        )
        .map_err(|error| AppError::InvalidConfig(error.to_string()))?;

        self.validate_cors_origins()?;
        self.validate_knowledge()?;
        self.validate_security()?;
        if self
            .local_corpus_root
            .as_deref()
            .is_some_and(|root| root.trim().is_empty())
        {
            return Err(AppError::InvalidConfig(
                "local_corpus_root must not be empty".to_owned(),
            ));
        }
        if self
            .local_corpus_root
            .as_deref()
            .is_some_and(|root| !Path::new(root).is_absolute())
        {
            return Err(AppError::InvalidConfig(
                "local_corpus_root must be an absolute path".to_owned(),
            ));
        }
        Ok(())
    }

    pub fn build_knowledge_coordinator(
        &self,
    ) -> Result<(crate::tools::KnowledgeCoordinator, bool), AppError> {
        use crate::tools::{
            KnowledgeTool,
            scholarly::{ScholarlyConfig, ScholarlySearch},
            web::{WebConfig, WebSearch},
        };

        self.validate_knowledge()?;
        let mut tools: Vec<Arc<dyn KnowledgeTool>> = Vec::new();
        if let Some(config) = &self.knowledge.scholarly {
            let timeouts =
                provider_timeouts(config.connect_timeout_seconds, config.read_timeout_seconds);
            let search = ScholarlySearch::new(ScholarlyConfig {
                providers: config.providers.clone(),
                semantic_scholar_base_url: config
                    .semantic_scholar_base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.semanticscholar.org/graph/v1".to_owned()),
                semantic_scholar_api_key: resolve_optional_secret(
                    config.semantic_scholar_api_key_env.as_deref(),
                )?,
                crossref_base_url: config
                    .crossref_base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.crossref.org".to_owned()),
                openalex_base_url: config
                    .openalex_base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openalex.org".to_owned()),
                openalex_api_key: resolve_optional_secret(config.openalex_api_key_env.as_deref())?,
                openalex_mailto: config.openalex_mailto.clone(),
                max_results: config.max_results.unwrap_or(5),
                timeouts,
            })
            .map_err(|_| {
                AppError::InvalidConfig("invalid scholarly provider configuration".to_owned())
            })?;
            if search.is_configured() {
                tools.push(Arc::new(search));
            }
        }
        if let Some(config) = &self.knowledge.web {
            let timeouts =
                provider_timeouts(config.connect_timeout_seconds, config.read_timeout_seconds);
            let search = WebSearch::new(WebConfig {
                providers: config.providers.clone(),
                searxng_base_url: config.searxng_base_url.clone(),
                bing_base_url: config
                    .bing_base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.bing.microsoft.com/v7.0/search".to_owned()),
                bing_api_key: resolve_optional_secret(config.bing_api_key_env.as_deref())?,
                brave_base_url: config
                    .brave_base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.search.brave.com/res/v1/web/search".to_owned()),
                brave_api_key: resolve_optional_secret(config.brave_api_key_env.as_deref())?,
                max_results: config.max_results.unwrap_or(5),
                timeouts,
            })
            .map_err(|_| {
                AppError::InvalidConfig("invalid web provider configuration".to_owned())
            })?;
            if search.is_configured() {
                tools.push(Arc::new(search));
            }
        }
        let coordinator = crate::tools::KnowledgeCoordinator::new(tools);
        let web_enabled = coordinator.has_tool("web");
        Ok((coordinator, web_enabled))
    }

    fn validate_knowledge(&self) -> Result<(), AppError> {
        if let Some(config) = &self.knowledge.web {
            validate_provider_names(
                config.providers.as_deref(),
                &["searxng", "searx", "bing", "brave"],
            )?;
            for value in [
                config.searxng_base_url.as_deref(),
                config.bing_base_url.as_deref(),
                config.brave_base_url.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                validate_provider_url(value)?;
            }
            validate_provider_limits(
                config.max_results,
                config.connect_timeout_seconds,
                config.read_timeout_seconds,
            )?;
            validate_env_names([
                config.bing_api_key_env.as_deref(),
                config.brave_api_key_env.as_deref(),
            ])?;
        }
        if let Some(config) = &self.knowledge.scholarly {
            validate_provider_names(
                config.providers.as_deref(),
                &[
                    "semantic_scholar",
                    "semantic-scholar",
                    "crossref",
                    "openalex",
                ],
            )?;
            for value in [
                config.semantic_scholar_base_url.as_deref(),
                config.crossref_base_url.as_deref(),
                config.openalex_base_url.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                validate_provider_url(value)?;
            }
            validate_provider_limits(
                config.max_results,
                config.connect_timeout_seconds,
                config.read_timeout_seconds,
            )?;
            validate_env_names([
                config.semantic_scholar_api_key_env.as_deref(),
                config.openalex_api_key_env.as_deref(),
            ])?;
        }
        Ok(())
    }

    fn validate_cors_origins(&self) -> Result<(), AppError> {
        for value in &self.cors_allowed_origins {
            let uri = value
                .parse::<axum::http::Uri>()
                .map_err(|_| AppError::InvalidConfig("invalid CORS allowed origin".to_owned()))?;
            let valid_scheme = matches!(uri.scheme_str(), Some("http" | "https"));
            let authority = uri.authority().map(|value| value.as_str());
            let valid_authority = authority.is_some_and(|value| {
                !value.is_empty() && !value.contains('@') && !value.chars().any(char::is_whitespace)
            });
            let origin_only = uri
                .path_and_query()
                .is_none_or(|value| value.path() == "/" && value.query().is_none())
                && value
                    .split_once("://")
                    .is_some_and(|(_, authority)| !authority.contains('/'));
            if !valid_scheme || !valid_authority || !origin_only {
                return Err(AppError::InvalidConfig(
                    "invalid CORS allowed origin".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn validate_security(&self) -> Result<(), AppError> {
        if self
            .security
            .teacher_access_token
            .as_deref()
            .is_some_and(str::is_empty)
            || self.security.student_token_pepper.is_empty()
        {
            return Err(AppError::InvalidConfig(
                "security credentials must not be empty".to_owned(),
            ));
        }
        if self.conversation_context_max_chars < 4_000
            || self.conversation_context_recent_chars < 2_000
            || self.conversation_context_recent_chars > self.conversation_context_max_chars
        {
            return Err(AppError::InvalidConfig(
                "invalid conversation context character budgets".to_owned(),
            ));
        }
        Ok(())
    }
}

fn validate_provider_names(
    configured: Option<&[String]>,
    allowed: &[&str],
) -> Result<(), AppError> {
    if configured.is_some_and(|providers| {
        providers.is_empty()
            || providers
                .iter()
                .any(|provider| !allowed.contains(&provider.as_str()))
    }) {
        return Err(AppError::InvalidConfig(
            "unknown knowledge provider".to_owned(),
        ));
    }
    Ok(())
}

fn provider_timeouts(connect: Option<u64>, read: Option<u64>) -> crate::tools::HttpTimeouts {
    crate::tools::HttpTimeouts::new(
        Duration::from_secs(connect.unwrap_or(3)),
        Duration::from_secs(read.unwrap_or(10)),
    )
}

fn validate_provider_limits(
    max_results: Option<usize>,
    connect: Option<u64>,
    read: Option<u64>,
) -> Result<(), AppError> {
    if max_results.is_some_and(|value| !(1..=10).contains(&value))
        || connect.is_some_and(|value| value == 0 || value > 60)
        || read.is_some_and(|value| value == 0 || value > 120)
    {
        return Err(AppError::InvalidConfig(
            "invalid knowledge provider limits".to_owned(),
        ));
    }
    Ok(())
}

fn validate_env_names<'a>(
    names: impl IntoIterator<Item = Option<&'a str>>,
) -> Result<(), AppError> {
    if names.into_iter().flatten().any(|name| {
        name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    }) {
        return Err(AppError::InvalidConfig(
            "invalid knowledge credential environment name".to_owned(),
        ));
    }
    Ok(())
}

fn validate_provider_url(value: &str) -> Result<(), AppError> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| AppError::InvalidConfig("invalid knowledge provider URL".to_owned()))?;
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if !matches!(url.scheme(), "http" | "https")
        || (url.scheme() == "http" && !loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::InvalidConfig(
            "invalid knowledge provider URL".to_owned(),
        ));
    }
    Ok(())
}

fn resolve_optional_secret(name: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(name) = name else {
        return Ok(None);
    };
    match env::var(name) {
        Ok(value) => Ok((!value.trim().is_empty()).then_some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(AppError::InvalidConfig(
            "knowledge credential environment is not valid UTF-8".to_owned(),
        )),
    }
}
