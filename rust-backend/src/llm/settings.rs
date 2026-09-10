use std::{env, fmt, sync::RwLock};

use genai::adapter::AdapterKind;
use serde::{Deserialize, Serialize};

use crate::config::{ModelConfig, RunDefaults};
use crate::domain::PriceSnapshot;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PublicModelSettings {
    pub provider: String,
    pub endpoint: String,
    pub name: String,
    pub api_key_env: String,
    pub api_key_configured: bool,
    pub context_length: u32,
    pub max_output_tokens: u32,
    pub reasoning_mode: String,
    pub input_price_microusd_per_million: u64,
    pub output_price_microusd_per_million: u64,
    pub default_token_budget: u64,
    pub default_cost_budget_microusd: u64,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettingsUpdate {
    pub provider: Option<String>,
    pub endpoint: Option<String>,
    pub name: Option<String>,
    pub api_key: Option<String>,
    pub context_length: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub reasoning_mode: Option<String>,
    pub input_price_microusd_per_million: Option<u64>,
    pub output_price_microusd_per_million: Option<u64>,
    pub default_token_budget: Option<u64>,
    pub default_cost_budget_microusd: Option<u64>,
}

impl fmt::Debug for ModelSettingsUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelSettingsUpdate")
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint.as_ref().map(|_| "[REDACTED]"))
            .field("name", &self.name)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("context_length", &self.context_length)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("reasoning_mode", &self.reasoning_mode)
            .field(
                "input_price_microusd_per_million",
                &self.input_price_microusd_per_million,
            )
            .field(
                "output_price_microusd_per_million",
                &self.output_price_microusd_per_million,
            )
            .field("default_token_budget", &self.default_token_budget)
            .field(
                "default_cost_budget_microusd",
                &self.default_cost_budget_microusd,
            )
            .finish()
    }
}

#[derive(Clone)]
pub struct SecretValue(String);

impl SecretValue {
    fn new(value: String) -> Option<Self> {
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

#[derive(Clone)]
struct ModelSettings {
    provider: String,
    endpoint: String,
    name: String,
    api_key_env: String,
    temporary_api_key: Option<SecretValue>,
    context_length: u32,
    max_output_tokens: u32,
    reasoning_mode: String,
    input_price_microusd_per_million: u64,
    output_price_microusd_per_million: u64,
    default_token_budget: u64,
    default_cost_budget_microusd: u64,
}

pub struct ModelSettingsStore {
    settings: RwLock<ModelSettings>,
}

#[derive(Clone)]
pub struct ModelCallSettings {
    pub provider: String,
    pub endpoint: String,
    pub name: String,
    pub context_length: u32,
    pub max_output_tokens: u32,
    pub reasoning_mode: String,
    pub price: PriceSnapshot,
    pub(crate) secret: Option<SecretValue>,
}

#[derive(Clone)]
pub struct RunSettingsLease {
    pub call: ModelCallSettings,
    pub default_token_budget: u64,
    pub default_cost_budget_microusd: u64,
}

impl fmt::Debug for ModelSettingsStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.settings.read() {
            Ok(settings) => formatter
                .debug_struct("ModelSettingsStore")
                .field("provider", &settings.provider)
                .field("endpoint", &settings.endpoint)
                .field("name", &settings.name)
                .field("api_key_env", &settings.api_key_env)
                .field(
                    "api_key_configured",
                    &has_configured_key(&settings).unwrap_or(false),
                )
                .field("context_length", &settings.context_length)
                .field("max_output_tokens", &settings.max_output_tokens)
                .field("reasoning_mode", &settings.reasoning_mode)
                .field(
                    "input_price_microusd_per_million",
                    &settings.input_price_microusd_per_million,
                )
                .field(
                    "output_price_microusd_per_million",
                    &settings.output_price_microusd_per_million,
                )
                .field("default_token_budget", &settings.default_token_budget)
                .field(
                    "default_cost_budget_microusd",
                    &settings.default_cost_budget_microusd,
                )
                .finish(),
            Err(_) => formatter.write_str("ModelSettingsStore([UNAVAILABLE])"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ModelSettingsError {
    #[error("model settings are unavailable")]
    Unavailable,
    #[error("changing endpoint or provider requires a new API key")]
    EndpointKeyRequired,
    #[error("model provider is not supported")]
    UnsupportedProvider,
    #[error("model endpoint must be a valid absolute HTTP(S) URL")]
    InvalidEndpoint,
    #[error("model name must not be empty")]
    EmptyModelName,
    #[error("api_key_env must not be empty")]
    EmptyApiKeyEnvironment,
    #[error("context_length must be greater than zero")]
    ZeroContextLength,
    #[error("max_output_tokens must be greater than zero")]
    ZeroMaxOutputTokens,
    #[error("max_output_tokens must not exceed context_length")]
    OutputExceedsContext,
    #[error("reasoning_mode is not supported")]
    UnsupportedReasoningMode,
    #[error("model price exceeds the supported persistence range")]
    PriceOutOfRange,
    #[error("default token budget must be greater than zero")]
    ZeroDefaultTokenBudget,
    #[error("default cost budget must be greater than zero")]
    ZeroDefaultCostBudget,
    #[error("default budget exceeds the supported persistence range")]
    BudgetOutOfRange,
    #[error("configured API key is not valid UTF-8")]
    InvalidEnvironmentSecret,
}

impl ModelSettingsStore {
    pub fn new(config: ModelConfig) -> Result<Self, ModelSettingsError> {
        let compatibility_defaults = RunDefaults {
            max_input_tokens: config.context_length,
            max_output_tokens: 1,
            max_cost_microusd: i64::MAX as u64,
        };
        Self::new_with_run_defaults(config, compatibility_defaults)
    }

    pub fn new_with_run_defaults(
        config: ModelConfig,
        run_defaults: RunDefaults,
    ) -> Result<Self, ModelSettingsError> {
        let default_token_budget = u64::from(run_defaults.max_input_tokens)
            .checked_add(u64::from(run_defaults.max_output_tokens))
            .ok_or(ModelSettingsError::BudgetOutOfRange)?;
        let settings = ModelSettings {
            provider: config.provider,
            endpoint: config.endpoint,
            name: config.name,
            api_key_env: config.api_key_env,
            temporary_api_key: None,
            context_length: config.context_length,
            max_output_tokens: config.max_output_tokens,
            reasoning_mode: config.reasoning_mode,
            input_price_microusd_per_million: config.input_price_microusd_per_million,
            output_price_microusd_per_million: config.output_price_microusd_per_million,
            default_token_budget,
            default_cost_budget_microusd: run_defaults.max_cost_microusd,
        };
        validate(&settings)?;
        Ok(Self {
            settings: RwLock::new(settings),
        })
    }

    pub fn public(&self) -> PublicModelSettings {
        let Ok(settings) = self.settings.read() else {
            return unavailable_public_settings();
        };
        public_settings(&settings)
    }

    pub fn update(&self, update: ModelSettingsUpdate) -> Result<(), ModelSettingsError> {
        let mut current = self
            .settings
            .write()
            .map_err(|_| ModelSettingsError::Unavailable)?;
        let mut candidate = current.clone();

        // A saved credential must never silently follow a newly selected destination.
        let destination_changed = update.endpoint.as_ref().is_some_and(|endpoint| {
            normalized_endpoint(endpoint) != normalized_endpoint(&candidate.endpoint)
        }) || update.provider.as_ref().is_some_and(|provider| {
            adapter_kind_for_provider(provider) != adapter_kind_for_provider(&candidate.provider)
        });
        let has_new_key = update
            .api_key
            .as_ref()
            .is_some_and(|key| !key.trim().is_empty());
        apply_update(&mut candidate, update);
        validate(&candidate)?;
        if destination_changed && !has_new_key {
            return Err(ModelSettingsError::EndpointKeyRequired);
        }
        *current = candidate;
        Ok(())
    }

    pub fn resolve_secret(&self) -> Result<Option<SecretValue>, ModelSettingsError> {
        let settings = self
            .settings
            .read()
            .map_err(|_| ModelSettingsError::Unavailable)?;
        resolve_secret(&settings)
    }

    pub fn lease_for_call(&self) -> Result<ModelCallSettings, ModelSettingsError> {
        Ok(self.lease_for_run()?.call)
    }

    pub fn lease_for_run(&self) -> Result<RunSettingsLease, ModelSettingsError> {
        let settings = self
            .settings
            .read()
            .map_err(|_| ModelSettingsError::Unavailable)?;
        let secret = resolve_secret(&settings)?;
        Ok(RunSettingsLease {
            call: ModelCallSettings {
                provider: settings.provider.clone(),
                endpoint: normalized_endpoint(&settings.endpoint),
                name: settings.name.clone(),
                context_length: settings.context_length,
                secret,
                max_output_tokens: settings.max_output_tokens,
                reasoning_mode: settings.reasoning_mode.clone(),
                price: PriceSnapshot {
                    input_microusd_per_million: settings.input_price_microusd_per_million,
                    output_microusd_per_million: settings.output_price_microusd_per_million,
                },
            },
            default_token_budget: settings.default_token_budget,
            default_cost_budget_microusd: settings.default_cost_budget_microusd,
        })
    }
}

pub(super) fn adapter_kind_for_provider(provider: &str) -> Option<AdapterKind> {
    let normalized = provider.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "openai" | "openai_compatible" => Some(AdapterKind::OpenAI),
        "deepseek" => Some(AdapterKind::DeepSeek),
        _ => None,
    }
}

pub(super) fn endpoint_is_loopback(endpoint: &reqwest::Url) -> bool {
    endpoint.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

fn apply_update(settings: &mut ModelSettings, update: ModelSettingsUpdate) {
    if let Some(provider) = update.provider {
        settings.provider = provider;
    }
    if let Some(endpoint) = update.endpoint {
        settings.endpoint = endpoint;
    }
    if let Some(name) = update.name {
        settings.name = name;
    }
    if let Some(api_key) = update.api_key {
        settings.temporary_api_key = SecretValue::new(api_key);
    }
    if let Some(context_length) = update.context_length {
        settings.context_length = context_length;
    }
    if let Some(max_output_tokens) = update.max_output_tokens {
        settings.max_output_tokens = max_output_tokens;
    }
    if let Some(reasoning_mode) = update.reasoning_mode {
        settings.reasoning_mode = reasoning_mode;
    }
    if let Some(input_price) = update.input_price_microusd_per_million {
        settings.input_price_microusd_per_million = input_price;
    }
    if let Some(output_price) = update.output_price_microusd_per_million {
        settings.output_price_microusd_per_million = output_price;
    }
    if let Some(default_token_budget) = update.default_token_budget {
        settings.default_token_budget = default_token_budget;
    }
    if let Some(default_cost_budget_microusd) = update.default_cost_budget_microusd {
        settings.default_cost_budget_microusd = default_cost_budget_microusd;
    }
}

fn validate(settings: &ModelSettings) -> Result<(), ModelSettingsError> {
    if adapter_kind_for_provider(&settings.provider).is_none() {
        return Err(ModelSettingsError::UnsupportedProvider);
    }
    let endpoint =
        reqwest::Url::parse(&settings.endpoint).map_err(|_| ModelSettingsError::InvalidEndpoint)?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.cannot_be_a_base()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || (endpoint.scheme() == "http" && !endpoint_is_loopback(&endpoint))
        || endpoint.fragment().is_some()
        || !endpoint
            .query_pairs()
            .all(|(key, _)| key.eq_ignore_ascii_case("api-version"))
    {
        return Err(ModelSettingsError::InvalidEndpoint);
    }
    if settings.name.trim().is_empty() {
        return Err(ModelSettingsError::EmptyModelName);
    }
    if settings.api_key_env.trim().is_empty() {
        return Err(ModelSettingsError::EmptyApiKeyEnvironment);
    }
    if settings.context_length == 0 {
        return Err(ModelSettingsError::ZeroContextLength);
    }
    if settings.max_output_tokens == 0 {
        return Err(ModelSettingsError::ZeroMaxOutputTokens);
    }
    if settings.max_output_tokens > settings.context_length {
        return Err(ModelSettingsError::OutputExceedsContext);
    }
    if settings.input_price_microusd_per_million > i64::MAX as u64
        || settings.output_price_microusd_per_million > i64::MAX as u64
    {
        return Err(ModelSettingsError::PriceOutOfRange);
    }
    if settings.default_token_budget == 0 {
        return Err(ModelSettingsError::ZeroDefaultTokenBudget);
    }
    if settings.default_cost_budget_microusd == 0 {
        return Err(ModelSettingsError::ZeroDefaultCostBudget);
    }
    if settings.default_token_budget > i64::MAX as u64
        || settings.default_cost_budget_microusd > i64::MAX as u64
    {
        return Err(ModelSettingsError::BudgetOutOfRange);
    }
    if settings
        .reasoning_mode
        .parse::<genai::chat::ReasoningEffort>()
        .is_err()
    {
        return Err(ModelSettingsError::UnsupportedReasoningMode);
    }
    Ok(())
}

fn public_settings(settings: &ModelSettings) -> PublicModelSettings {
    PublicModelSettings {
        provider: settings.provider.clone(),
        endpoint: settings.endpoint.clone(),
        name: settings.name.clone(),
        api_key_env: settings.api_key_env.clone(),
        api_key_configured: has_configured_key(settings).unwrap_or(false),
        context_length: settings.context_length,
        max_output_tokens: settings.max_output_tokens,
        reasoning_mode: settings.reasoning_mode.clone(),
        input_price_microusd_per_million: settings.input_price_microusd_per_million,
        output_price_microusd_per_million: settings.output_price_microusd_per_million,
        default_token_budget: settings.default_token_budget,
        default_cost_budget_microusd: settings.default_cost_budget_microusd,
    }
}

fn unavailable_public_settings() -> PublicModelSettings {
    PublicModelSettings {
        provider: String::new(),
        endpoint: String::new(),
        name: String::new(),
        api_key_env: String::new(),
        api_key_configured: false,
        context_length: 0,
        max_output_tokens: 0,
        reasoning_mode: String::new(),
        input_price_microusd_per_million: 0,
        output_price_microusd_per_million: 0,
        default_token_budget: 0,
        default_cost_budget_microusd: 0,
    }
}

fn has_configured_key(settings: &ModelSettings) -> Result<bool, ModelSettingsError> {
    resolve_secret(settings).map(|secret| secret.is_some())
}

fn resolve_secret(settings: &ModelSettings) -> Result<Option<SecretValue>, ModelSettingsError> {
    if let Some(secret) = settings.temporary_api_key.clone() {
        return Ok(Some(secret));
    }
    match env::var(&settings.api_key_env) {
        Ok(value) => Ok(SecretValue::new(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(ModelSettingsError::InvalidEnvironmentSecret),
    }
}

fn normalized_endpoint(endpoint: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(endpoint) else {
        return endpoint.to_owned();
    };
    if !parsed.path().ends_with('/') {
        let path = format!("{}/", parsed.path());
        parsed.set_path(&path);
    }
    parsed.into()
}
