//! OpenAI-compatible HTTP provider (SPEC §9.1).
//!
//! Speaks `POST {base_url}/chat/completions` with the wire types in the parent
//! module, which is what OpenAI, Anthropic's compatibility endpoint, Together,
//! Groq, vLLM, llama.cpp, Ollama and OpenRouter accept. Bounded retries and
//! timeouts live here; cancellation is the caller's race (see
//! [`crate::agents::AgentContext::complete`]).

use super::{ChatRequest, ChatResponse, Provider};
use crate::{Error, Result};
use std::time::Duration;

/// Environment variables that configure the provider.
pub const ENV_URL: &str = "URUK_PROVIDER_URL";
pub const ENV_KEY: &str = "URUK_API_KEY";
pub const ENV_MODEL: &str = "URUK_MODEL";
/// Optional `reasoning_effort` sent with every request (`none`, `low`, `medium`,
/// `high`). Reasoning models spend most of their time thinking otherwise.
pub const ENV_REASONING: &str = "URUK_REASONING_EFFORT";

#[derive(Debug, Clone)]
pub struct HttpProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    name: String,
    max_retries: u32,
    retry_base: Duration,
    reasoning_effort: Option<String>,
}

impl HttpProvider {
    /// `base_url` is the API root, e.g. `https://api.openai.com/v1`.
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let base_url: String = base_url.into();
        let base_url = base_url.trim_end_matches('/').to_string();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(Error::validation(format!(
                "provider URL must start with http:// or https://, got {base_url:?}"
            )));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| Error::Provider(format!("cannot build HTTP client: {e}")))?;

        let host = base_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("provider")
            .to_string();

        Ok(Self {
            client,
            base_url,
            api_key,
            model: model.into(),
            name: format!("openai-compatible@{host}"),
            max_retries: 3,
            retry_base: Duration::from_millis(500),
            reasoning_effort: None,
        })
    }

    /// Configure from `URUK_PROVIDER_URL`, `URUK_API_KEY`, and
    /// `URUK_MODEL`. Returns `None` when no URL is set.
    pub fn from_env() -> Result<Option<Self>> {
        let Ok(url) = std::env::var(ENV_URL) else {
            return Ok(None);
        };
        if url.trim().is_empty() {
            return Ok(None);
        }
        let model = std::env::var(ENV_MODEL).map_err(|_| {
            Error::validation(format!(
                "{ENV_URL} is set but {ENV_MODEL} is not; name the model"
            ))
        })?;
        let key = std::env::var(ENV_KEY).ok().filter(|k| !k.trim().is_empty());
        let effort = std::env::var(ENV_REASONING)
            .ok()
            .filter(|e| !e.trim().is_empty());
        Ok(Some(
            Self::new(url, key, model)?.with_reasoning_effort(effort),
        ))
    }

    pub fn with_max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Base delay for exponential backoff; tests shrink it.
    pub fn with_retry_base(mut self, d: Duration) -> Self {
        self.retry_base = d;
        self
    }

    /// `reasoning_effort` to send with every request; `None` omits the field.
    pub fn with_reasoning_effort(mut self, effort: Option<String>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    pub fn reasoning_effort(&self) -> Option<&str> {
        self.reasoning_effort.as_deref()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

#[async_trait::async_trait]
impl Provider for HttpProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut body = serde_json::to_value(&request)
            .map_err(|e| Error::Provider(format!("serialising request: {e}")))?;
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = serde_json::Value::String(effort.clone());
        }
        let mut attempt = 0u32;

        loop {
            let mut req = self.client.post(&url).json(&body);
            if let Some(key) = &self.api_key {
                req = req.bearer_auth(key);
            }

            let transient_error = match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp
                        .text()
                        .await
                        .map_err(|e| Error::Provider(format!("reading response: {e}")))?;
                    if status.is_success() {
                        return serde_json::from_str::<ChatResponse>(&body).map_err(|e| {
                            Error::Provider(format!(
                                "unparseable chat completion ({e}): {}",
                                clip(&body, 300)
                            ))
                        });
                    }
                    let transient = status.as_u16() == 429 || status.is_server_error();
                    let message = format!("HTTP {status}: {}", clip(&body, 300));
                    if !transient {
                        return Err(Error::Provider(message));
                    }
                    message
                }
                Err(e) => {
                    let transient = e.is_connect() || e.is_timeout() || e.is_request();
                    if !transient {
                        return Err(Error::Provider(format!("request failed: {e}")));
                    }
                    format!("request failed: {e}")
                }
            };

            if attempt >= self.max_retries {
                return Err(Error::Provider(format!(
                    "{transient_error} (after {} attempt(s))",
                    attempt + 1
                )));
            }
            let backoff = self.retry_base * 2u32.saturating_pow(attempt);
            tracing::warn!(attempt, %transient_error, "provider call failed; retrying");
            tokio::time::sleep(backoff).await;
            attempt += 1;
        }
    }
}

fn clip(s: &str, n: usize) -> String {
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= n)
        .last()
        .unwrap_or(0);
    if end >= s.len() {
        s.to_string()
    } else {
        format!("{}…", &s[..end])
    }
}
