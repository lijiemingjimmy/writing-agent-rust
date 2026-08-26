use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
use genai::{
    Client, ModelIden, ServiceTarget,
    chat::{ChatMessage, ChatOptions, ChatRequest, ReasoningEffort},
    resolver::{AuthData, Endpoint},
};
use tokio_util::sync::CancellationToken;

use crate::{
    domain::Usage,
    llm::settings::{
        ModelCallSettings, ModelSettingsError, ModelSettingsStore, adapter_kind_for_provider,
        endpoint_is_loopback,
    },
};

const MAX_RESPONSE_ID_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelMessage {
    pub role: ModelRole,
    pub content: String,
}

impl ModelMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(ModelRole::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(ModelRole::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(ModelRole::Assistant, content)
    }

    fn new(role: ModelRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelRequest {
    pub messages: Vec<ModelMessage>,
    pub temperature: Option<f64>,
}

impl ModelRequest {
    pub fn from_user(content: impl Into<String>) -> Self {
        Self {
            messages: vec![ModelMessage::user(content)],
            temperature: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelResponse {
    pub content: String,
    pub reasoning: Option<String>,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    pub stop_reason: Option<String>,
    pub response_id: Option<String>,
    pub latency_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ModelError {
    #[error("model request was cancelled")]
    Cancelled,
    #[error("model request is invalid")]
    InvalidRequest,
    #[error("model settings are invalid or incomplete")]
    Configuration,
    #[error("model provider request failed")]
    Provider,
    #[error("model provider returned an invalid response")]
    InvalidResponse,
}

impl From<ModelSettingsError> for ModelError {
    fn from(_: ModelSettingsError) -> Self {
        Self::Configuration
    }
}

#[async_trait]
pub trait ModelGateway: Send + Sync {
    async fn complete(
        &self,
        request: ModelRequest,
        settings: ModelCallSettings,
        cancellation: CancellationToken,
    ) -> Result<ModelResponse, ModelError>;
}

#[derive(Clone, Debug)]
pub struct GenaiModelGateway {
    settings: Arc<ModelSettingsStore>,
}

impl GenaiModelGateway {
    pub fn new(settings: Arc<ModelSettingsStore>) -> Self {
        Self { settings }
    }

    pub async fn complete(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelResponse, ModelError> {
        let settings = self.settings.lease_for_call()?;
        <Self as ModelGateway>::complete(self, request, settings, cancellation).await
    }
}

#[async_trait]
impl ModelGateway for GenaiModelGateway {
    async fn complete(
        &self,
        request: ModelRequest,
        settings: ModelCallSettings,
        cancellation: CancellationToken,
    ) -> Result<ModelResponse, ModelError> {
        if cancellation.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        if request.messages.is_empty() {
            return Err(ModelError::InvalidRequest);
        }

        let adapter_kind =
            adapter_kind_for_provider(&settings.provider).ok_or(ModelError::Configuration)?;
        let secret = settings.secret.ok_or(ModelError::Configuration)?;
        let client = client_for_endpoint(adapter_kind, &settings.endpoint)?;
        let target = ServiceTarget {
            endpoint: Endpoint::from_owned(settings.endpoint),
            auth: AuthData::from_single(secret.expose_secret()),
            model: ModelIden::new(adapter_kind, settings.name),
        };
        let chat_request = ChatRequest::new(
            request
                .messages
                .into_iter()
                .map(into_genai_message)
                .collect(),
        );
        let mut options = ChatOptions::default()
            .with_max_tokens(settings.max_output_tokens)
            .with_capture_raw_body(true)
            .with_normalize_reasoning_content(true)
            .with_reasoning_effort(
                settings
                    .reasoning_mode
                    .parse::<ReasoningEffort>()
                    .map_err(|_| ModelError::Configuration)?,
            );
        if let Some(temperature) = request.temperature {
            if !temperature.is_finite() {
                return Err(ModelError::InvalidRequest);
            }
            options = options.with_temperature(temperature);
        }

        let started_at = Instant::now();
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(ModelError::Cancelled),
            response = client.exec_chat(target, chat_request, Some(&options)) => {
                response.map_err(|_| ModelError::Provider)?
            }
        };
        let latency_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);

        map_response(response, latency_ms)
    }
}

fn client_for_endpoint(
    adapter_kind: genai::adapter::AdapterKind,
    endpoint: &str,
) -> Result<Client, ModelError> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| ModelError::Configuration)?;
    let is_loopback = endpoint_is_loopback(&url);
    let builder = Client::builder().with_adapter_kind(adapter_kind);
    if is_loopback {
        let web_client = reqwest13::Client::builder()
            .no_proxy()
            .build()
            .map_err(|_| ModelError::Configuration)?;
        Ok(builder.with_reqwest(web_client).build())
    } else {
        Ok(builder.build())
    }
}

fn into_genai_message(message: ModelMessage) -> ChatMessage {
    match message.role {
        ModelRole::System => ChatMessage::system(message.content),
        ModelRole::User => ChatMessage::user(message.content),
        ModelRole::Assistant => ChatMessage::assistant(message.content),
    }
}

fn map_response(
    response: genai::chat::ChatResponse,
    latency_ms: u64,
) -> Result<ModelResponse, ModelError> {
    let input_tokens = checked_token_count(response.usage.prompt_tokens)?;
    let output_tokens = checked_token_count(response.usage.completion_tokens)?;
    let provider_adapter = response.provider_model_iden.adapter_kind;
    let response_id = valid_response_id(response.response_id.clone()).or_else(|| {
        matches!(
            provider_adapter,
            genai::adapter::AdapterKind::OpenAI | genai::adapter::AdapterKind::DeepSeek
        )
        .then(|| {
            response
                .captured_raw_body
                .as_ref()
                .and_then(|body| body.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .flatten()
        .and_then(|id| valid_response_id(Some(id)))
    });
    let content = response.texts().join("\n");
    if content.is_empty() {
        return Err(ModelError::InvalidResponse);
    }

    Ok(ModelResponse {
        content,
        reasoning: response.reasoning_content,
        provider: response
            .provider_model_iden
            .adapter_kind
            .as_lower_str()
            .to_owned(),
        model: response.provider_model_iden.model_name.to_string(),
        usage: Usage {
            input_tokens,
            output_tokens,
        },
        stop_reason: response.stop_reason.map(|reason| reason.raw().to_owned()),
        response_id,
        latency_ms,
    })
}

fn valid_response_id(response_id: Option<String>) -> Option<String> {
    response_id.filter(|response_id| {
        !response_id.trim().is_empty()
            && response_id.len() <= MAX_RESPONSE_ID_BYTES
            && !response_id.chars().any(char::is_control)
    })
}

fn checked_token_count(count: Option<i32>) -> Result<u64, ModelError> {
    count
        .ok_or(ModelError::InvalidResponse)
        .and_then(|count| u64::try_from(count).map_err(|_| ModelError::InvalidResponse))
}
