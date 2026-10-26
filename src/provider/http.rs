//! OpenAI-compatible HTTP provider (SPEC §9.1).
//!
//! Speaks `POST {base_url}/chat/completions` with the wire types in the parent
//! module, which is what OpenAI, Anthropic's compatibility endpoint, Together,
//! Groq, vLLM, llama.cpp, Ollama and OpenRouter accept.
//!
//! This adapter classifies every outcome of a send as served, terminal for
//! this request, or evidence of an upstream outage. An outage failure (a
//! connect, send, or response transport error (including timeouts), or a
//! 408, 425, 429, or 5xx) is waited out
//! through the process-wide [`UpstreamGate`] of the upstream — one half-open
//! probe at a time, capped backoff, `Retry-After` honored — until the upstream
//! serves the request. The wait is bounded only by the caller's cancellation
//! (the run's stop request; see [`crate::agents::AgentContext::complete`]),
//! which drops the in-flight send and any pending probe. Each send has a
//! finite timeout; expiry enters the same visible provider wait and retries
//! with exponential backoff instead of terminating research. Retries may
//! duplicate provider-side computation; its cost remains unknown. Permanent
//! failures (request-specific 4xx, an unparseable reply, a malformed request) are
//! [`Error::Provider`] at once. Request-start pacing applies to every send.

use super::availability::{SendError, complete_through_gate};
use super::keys::{ApiKey, ApiKeyPool};
use super::pacing::StartPacer;
use super::{
    Attempts, ChatRequest, ChatResponse, ProbeBackoff, Provider, TransientFault, TransportPhase,
    UpstreamGate,
};
use crate::{Error, Result};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Environment variables that configure the provider.
pub const ENV_URL: &str = "URUK_PROVIDER_URL";
pub const ENV_KEY: &str = "URUK_API_KEY";
/// Directory containing one API key per `*.key` file. It is re-read for
/// every logical completion so operators can add accounts without a restart.
pub const ENV_KEY_DIR: &str = "URUK_API_KEY_DIR";
pub const ENV_MODEL: &str = "URUK_MODEL";
/// Optional `reasoning_effort` sent with every request (`none`, `low`, `medium`,
/// `high`). Reasoning models spend most of their time thinking otherwise.
pub const ENV_REASONING: &str = "URUK_REASONING_EFFORT";
/// Optional overall completion-request timeout in seconds, greater than the
/// 20-second connect timeout. This is deliberately much longer because
/// non-streaming providers do not return headers until generation completes.
pub const ENV_TIMEOUT_SECS: &str = "URUK_PROVIDER_TIMEOUT_SECS";
/// Optional request-start pacing in requests per minute. Unset or `0` leaves
/// requests unpaced.
pub const ENV_RPM: &str = "URUK_PROVIDER_RPM";
/// A 20,000-token non-streaming completion can legitimately take more than
/// five minutes; keep the connection open while the provider is working.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30 * 60;
const CONNECT_TIMEOUT_SECS: u64 = 20;

#[derive(Debug, Clone)]
pub struct HttpProvider {
    client: reqwest::Client,
    request_timeout: Duration,
    base_url: String,
    api_keys: Arc<ApiKeyPool>,
    model: String,
    name: String,
    /// Normalized upstream identity, `host:effective-port`. URL spellings of
    /// one endpoint — explicit default port, userinfo, host casing — collapse
    /// to the same identity, so they share one pacer schedule.
    upstream: String,
    /// Spacing of the probes this handle schedules while its upstream is
    /// unavailable.
    probe_backoff: ProbeBackoff,
    reasoning_effort: Option<String>,
    /// This handle's pacing target; `None` means unpaced. The shared
    /// schedule it resolves to at request time is one per upstream/account
    /// identity, making each account's limit process-wide.
    rpm: Option<NonZeroU32>,
}

fn parse_request_timeout(value: Option<&str>) -> Result<Duration> {
    let raw = value.unwrap_or("1800").trim();
    let seconds = raw.parse::<u64>().map_err(|_| {
        Error::validation(format!(
            "{ENV_TIMEOUT_SECS} must be a positive whole number of seconds, got {raw:?}"
        ))
    })?;
    if seconds <= CONNECT_TIMEOUT_SECS {
        return Err(Error::validation(format!(
            "{ENV_TIMEOUT_SECS} must be greater than the {CONNECT_TIMEOUT_SECS}-second \
             connection timeout"
        )));
    }
    Ok(Duration::from_secs(seconds))
}

/// Parse a `Retry-After` header value against the given wall-clock `now`.
///
/// Supports both RFC 9110 forms: delta-seconds and IMF-fixdate HTTP-dates
/// (the obsolete RFC 850 / asctime date forms are not). Dates at or before
/// `now` yield [`Duration::ZERO`]; malformed values yield `None` so the
/// caller falls back to plain backoff. Valid waits are honored in full, not
/// capped: a long wait is bounded by the caller cancelling the call (the
/// normal run deadline), not rewritten here.
fn parse_retry_after(value: &str, now: time::OffsetDateTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // HTTP-dates are always anchored to GMT, which the RFC 2822 parser does
    // not accept as a zone name; rewrite it to the equivalent fixed offset.
    let normalized = value.strip_suffix("GMT").map(|rest| format!("{rest}+0000"));
    let date = time::OffsetDateTime::parse(
        normalized.as_deref().unwrap_or(value),
        &time::format_description::well_known::Rfc2822,
    )
    .ok()?;
    // Positive deltas convert exactly (`time`'s i64 seconds always fit, so
    // conversion cannot overflow); a negative delta — a past date — fails
    // the conversion and means "retry immediately", never under-wait by
    // rounding a fractional delta down.
    Some(Duration::try_from(date - now).unwrap_or(Duration::ZERO))
}

impl HttpProvider {
    /// `base_url` is the API root, e.g. `https://api.openai.com/v1`.
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        Self::new_with_timeout(
            base_url,
            api_key,
            model,
            Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
        )
    }

    fn new_with_timeout(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
        request_timeout: Duration,
    ) -> Result<Self> {
        let base_url: String = base_url.into();
        let base_url = base_url.trim_end_matches('/').to_string();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(Error::validation(format!(
                "provider URL must start with http:// or https://, got {base_url:?}"
            )));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS))
            .timeout(request_timeout)
            .build()
            .map_err(|e| Error::Provider(format!("cannot build HTTP client: {e}")))?;

        let url = reqwest::Url::parse(&base_url).map_err(|e| {
            Error::validation(format!("cannot parse provider URL {base_url:?}: {e}"))
        })?;
        let host = url
            .host_str()
            .ok_or_else(|| Error::validation(format!("provider URL {base_url:?} has no host")))?;
        // Display host: as addressed, minus userinfo and any default port.
        let display_host = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };
        // Upstream identity: effective (host, port), so `https://h/v1` and
        // `https://h:443/v1` resolve to one identity and share one pacer.
        let upstream = match url.port_or_known_default() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };

        Ok(Self {
            client,
            request_timeout,
            base_url,
            api_keys: Arc::new(ApiKeyPool::new(None, api_key)?),
            model: model.into(),
            name: format!("openai-compatible@{display_host}"),
            upstream,
            probe_backoff: ProbeBackoff::DEFAULT,
            reasoning_effort: None,
            rpm: None,
        })
    }

    #[cfg(test)]
    fn new_with_api_keys(
        base_url: impl Into<String>,
        api_keys: Vec<String>,
        model: impl Into<String>,
    ) -> Result<Self> {
        let mut provider = Self::new(base_url, None, model)?;
        provider.api_keys = Arc::new(ApiKeyPool::with_fallback(None, api_keys)?);
        Ok(provider)
    }

    /// Configure from `URUK_PROVIDER_URL`, `URUK_API_KEY_DIR`,
    /// `URUK_API_KEY`, and `URUK_MODEL`. Returns `None` when no URL is set.
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
        let key_directory = std::env::var(ENV_KEY_DIR)
            .ok()
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from);
        let effort = std::env::var(ENV_REASONING)
            .ok()
            .filter(|e| !e.trim().is_empty());
        let timeout = parse_request_timeout(
            std::env::var(ENV_TIMEOUT_SECS)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .as_deref(),
        )?;
        let mut provider =
            Self::new_with_timeout(url, key.clone(), model, timeout)?.with_reasoning_effort(effort);
        provider.api_keys = Arc::new(ApiKeyPool::new(key_directory, key)?);
        if let Some(rpm) = std::env::var(ENV_RPM).ok().filter(|v| !v.trim().is_empty()) {
            let rpm: u32 = rpm.trim().parse().map_err(|_| {
                Error::validation(format!(
                    "{ENV_RPM} must be a whole number of requests per minute (0 disables \
                     pacing), got {rpm:?}"
                ))
            })?;
            provider = provider.with_requests_per_minute(NonZeroU32::new(rpm));
        }
        Ok(Some(provider))
    }

    /// How this handle spaces its probes of an unavailable upstream; tests
    /// shrink it. The gate itself is shared per upstream, so the handle
    /// that reports a failure sets the wait before the next probe.
    pub fn with_probe_backoff(mut self, backoff: ProbeBackoff) -> Self {
        self.probe_backoff = backoff;
        self
    }

    /// Set this handle's request-start pacing target to `rpm` per minute;
    /// `None` disables pacing for this handle.
    ///
    /// The target binds from this handle's next request, replacing any value
    /// set earlier on the same handle. All
    /// handles of one upstream/account identity share one process-wide schedule
    /// paced at the *strictest* target among them —
    /// one start per `60/rpm` seconds, applied to every outbound attempt
    /// including retries — so the identity's starts can never exceed the
    /// stricter configured allowance. The shared rate is a process-lifetime
    /// ratchet: once any handle has requested a stricter target for the
    /// identity, it stays in force even after that handle is dropped, so
    /// this handle may run below its own target. Requests are *not*
    /// serialized: a prior request may still be in flight when the next
    /// paced start fires.
    pub fn with_requests_per_minute(mut self, rpm: Option<NonZeroU32>) -> Self {
        self.rpm = rpm;
        self
    }

    /// This handle's configured pacing target, if any. The effective shared
    /// rate is never higher than this; see
    /// [`with_requests_per_minute`](Self::with_requests_per_minute).
    pub fn requests_per_minute(&self) -> Option<u32> {
        self.rpm.map(NonZeroU32::get)
    }

    /// Resolve the shared pacer for this handle's upstream identity, if
    /// paced. Resolution happens at request time, not configuration time.
    #[cfg(test)]
    fn pacer(&self) -> Option<Arc<StartPacer>> {
        self.pacer_for(None)
    }

    /// Each named key has an independent start schedule while all keys still
    /// share one endpoint availability gate.
    fn pacer_for(&self, key: Option<&ApiKey>) -> Option<Arc<StartPacer>> {
        self.rpm.map(|rpm| {
            let interval = Duration::from_secs(60).div_f64(f64::from(rpm.get()));
            let identity = match key {
                Some(key) => format!("{}|key:{}", self.upstream, key.id()),
                None => self.upstream.clone(),
            };
            StartPacer::shared(&identity, interval)
        })
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

    /// The process-wide availability gate of this handle's upstream.
    fn gate(&self) -> Arc<UpstreamGate> {
        UpstreamGate::shared(&self.upstream)
    }

    /// Persist each physical send separately, including failed probes.
    async fn send_once(
        &self,
        url: &str,
        body: &serde_json::Value,
        attempts: &Attempts,
        key: Option<&ApiKey>,
    ) -> std::result::Result<ChatResponse, SendError> {
        use crate::diagnostics::{Meta, Redactor, RequestLog};
        let mut log = if let Some(context) = &attempts.diagnostics {
            let meta = Meta {
                direction: "provider",
                request_id: uuid::Uuid::new_v4().to_string(),
                logical_id: Some(context.logical_id.clone()),
                run_id: context.run_id.clone(),
                task_id: context.task_id.clone(),
                attempt: Some(attempts.resends().saturating_add(1)),
            };
            let mut redactor = Redactor(
                key.map(|key| key.secret().to_string())
                    .into_iter()
                    .collect(),
            );
            redactor.observe_url(url);
            let mut capture = crate::diagnostics::Capture::default();
            capture.push(body.to_string().as_bytes());
            capture.complete = true;
            redactor.observe_capture(&capture);
            let request = serde_json::json!({
                "method":"POST", "url":crate::diagnostics::url(url),
                "headers":{"content-type":"application/json","authorization":"[REDACTED]"},
                "content":capture.snapshot("application/json", &redactor),
                "timeout_ms": self.request_timeout.as_millis(),
                "retry_policy": format!("{:?}", self.probe_backoff),
            });
            Some(
                RequestLog::begin(context.store.clone(), meta, request, redactor)
                    .await
                    .map_err(SendError::Permanent)?,
            )
        } else {
            None
        };
        let result = self.send_once_inner(url, body, log.as_mut(), key).await;
        if let Some(log) = &mut log {
            let (outcome, error) = match &result {
                Ok(_) => ("success", None),
                Err(SendError::Transient(fault)) => ("retryable", Some(fault.to_string())),
                Err(SendError::Permanent(error)) => (
                    "permanent_error",
                    Some(crate::diagnostics::error_chain(error)),
                ),
            };
            log.finish(outcome, error.as_deref(), None)
                .await
                .map_err(SendError::Permanent)?;
        }
        result
    }

    async fn send_once_inner(
        &self,
        url: &str,
        body: &serde_json::Value,
        mut log: Option<&mut crate::diagnostics::RequestLog>,
        key: Option<&ApiKey>,
    ) -> std::result::Result<ChatResponse, SendError> {
        let mut req = self.client.post(url).json(body);
        if let Some(key) = key {
            req = req.bearer_auth(key.secret());
        }
        let mut resp = req
            .send()
            .await
            .map_err(|error| classify_request_error(error, self.request_timeout))?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| parse_retry_after(v, time::OffsetDateTime::now_utc()));
        if let Some(log) = &mut log {
            log.status = Some(status.as_u16());
            log.response =
                serde_json::json!({"headers": crate::diagnostics::headers(resp.headers())});
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("text/plain")
                .to_owned();
            log.response_capture = Some((Default::default(), content_type));
        }
        // A status is classified even if its diagnostic body stalls. Limit
        // diagnostic reads independently so errors cannot hide behind a body.
        let read = async {
            let mut bytes = Vec::new();
            let mut complete = true;
            while let Some(chunk) = resp.chunk().await? {
                if let Some(log) = &mut log
                    && let Some((capture, _)) = &mut log.response_capture
                {
                    capture.push(&chunk);
                }
                bytes.extend_from_slice(&chunk);
                if !status.is_success() && bytes.len() >= crate::diagnostics::BODY_LIMIT {
                    complete = false;
                    break;
                }
            }
            if let Some(log) = &mut log
                && let Some((capture, _)) = &mut log.response_capture
            {
                capture.complete = complete;
            }
            Ok::<_, reqwest::Error>(String::from_utf8_lossy(&bytes).into_owned())
        };
        let text = if status.is_success() {
            read.await
                .map_err(|error| classify_request_error(error, self.request_timeout))?
        } else {
            match tokio::time::timeout(Duration::from_millis(100), read).await {
                Ok(Ok(text)) => text,
                other => {
                    let detail = match other {
                        Ok(Err(error)) => crate::diagnostics::error_chain(&error.without_url()),
                        _ => "diagnostic response body exceeded 100ms read deadline".into(),
                    };
                    if let Some(log) = &mut log
                        && let Some((capture, _)) = &mut log.response_capture
                    {
                        capture.error = Some(detail);
                    }
                    status.canonical_reason().unwrap_or("HTTP error").to_owned()
                }
            }
        };
        if TransientFault::is_transient_status(status.as_u16()) {
            return Err(SendError::Transient(TransientFault::status(
                status.as_u16(),
                retry_after,
                clip_redacted(&text, 300, key),
            )));
        }
        if key.is_some() && matches!(status.as_u16(), 401 | 403) {
            return Err(SendError::Transient(TransientFault::status(
                status.as_u16(),
                None,
                clip_redacted(&text, 300, key),
            )));
        }
        if status.is_success() {
            return serde_json::from_str::<ChatResponse>(&text).map_err(|e| {
                SendError::Permanent(Error::Provider(format!(
                    "unparseable chat completion ({e}): {}",
                    clip_redacted(&text, 300, key)
                )))
            });
        }
        Err(SendError::Permanent(Error::Provider(format!(
            "HTTP {status}: {}",
            clip_redacted(&text, 300, key)
        ))))
    }

    /// Try one full round-robin key cycle before declaring the provider
    /// unavailable. Invalid requests and malformed replies stay terminal:
    /// another credential cannot repair their payload.
    async fn send_key_cycle(
        &self,
        url: &str,
        body: &serde_json::Value,
        attempts: &Attempts,
    ) -> std::result::Result<ChatResponse, SendError> {
        let keys = self.api_keys.next_keys().map_err(SendError::Permanent)?;
        if keys.is_empty() {
            if let Some(pacer) = self.pacer_for(None) {
                pacer.pace(None).await;
            }
            return self.send_once(url, body, attempts, None).await;
        }

        let mut last_fault = None;
        for key in &keys {
            let pacer = self.pacer_for(Some(key));
            if let Some(pacer) = &pacer {
                pacer.pace(None).await;
            }
            match self.send_once(url, body, attempts, Some(key)).await {
                Ok(response) => return Ok(response),
                Err(SendError::Transient(fault)) => {
                    if let (Some(pacer), Some(delay)) = (&pacer, fault.retry_after()) {
                        pacer.defer_until(tokio::time::Instant::now() + delay);
                    }
                    tracing::warn!(key = key.id(), "API key could not serve call; rotating");
                    last_fault = Some(fault);
                }
                Err(error @ SendError::Permanent(_)) => return Err(error),
            }
        }

        Err(SendError::Transient(
            last_fault.expect("a non-empty key cycle records a result"),
        ))
    }
}

/// Classify a request failure without parsing its display text.
///
/// Connection failures and completion timeouts enter the shared recovery
/// loop: until a usable reply arrives, the run waits visibly and probes with
/// exponential backoff. Expiry limits each send, not the logical call.
fn classify_request_error(e: reqwest::Error, request_timeout: Duration) -> SendError {
    let e = e.without_url();
    if e.is_builder() || e.is_redirect() {
        return SendError::Permanent(Error::Provider(format!(
            "request failed: {}",
            crate::diagnostics::error_chain(&e)
        )));
    }
    if e.is_connect() {
        return SendError::Transient(TransientFault::transport(
            TransportPhase::Connect,
            crate::diagnostics::error_chain(&e),
        ));
    }
    if e.is_timeout() {
        return SendError::Transient(TransientFault::transport(
            TransportPhase::Response,
            format!(
                "request timed out after {request_timeout:?} while waiting for the model provider; {}",
                crate::diagnostics::error_chain(&e)
            ),
        ));
    }
    let phase = if e.is_body() || e.is_decode() {
        TransportPhase::Response
    } else {
        TransportPhase::Send
    };
    SendError::Transient(TransientFault::transport(
        phase,
        crate::diagnostics::error_chain(&e),
    ))
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
        self.complete_counted(request, &Attempts::default()).await
    }

    async fn complete_counted(
        &self,
        request: ChatRequest,
        attempts: &Attempts,
    ) -> Result<ChatResponse> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut body = serde_json::to_value(&request)
            .map_err(|e| Error::Provider(format!("serialising request: {e}")))?;
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning_effort"] = serde_json::Value::String(effort.clone());
        }
        let gate = self.gate();
        complete_through_gate(&gate, None, &self.probe_backoff, attempts, || {
            self.send_key_cycle(&url, &body, attempts)
        })
        .await
    }

    fn upstream_gate(&self) -> Option<Arc<UpstreamGate>> {
        Some(self.gate())
    }
}

/// Truncate `s` to at most `n` bytes on a `char` boundary, appending `…`
/// when anything was cut; strings within the limit pass through unchanged.
fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Clip provider text only after removing the active credential. Some
/// upstreams echo malformed authorization values in error bodies.
fn clip_redacted(s: &str, n: usize, key: Option<&ApiKey>) -> String {
    match key {
        Some(key) => clip(&s.replace(key.secret(), "[REDACTED]"), n),
        None => clip(s, n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::keys::ApiKeyPool;
    use std::path::PathBuf;
    use time::format_description::well_known::Rfc2822;
    use time::macros::datetime;

    #[test]
    fn provider_error_text_cannot_echo_the_active_key() {
        let pool = ApiKeyPool::new(None, Some("sk-rc-cannot-log-this".into())).unwrap();
        let key = pool.next_keys().unwrap().remove(0);
        let body = format!("authorization failed for {}", key.secret());

        let detail = clip_redacted(&body, 300, Some(&key));

        assert_eq!(detail, "authorization failed for [REDACTED]");
        assert!(!detail.contains(key.secret()));
        assert!(!format!("{key:?}").contains(key.secret()));
    }

    #[test]
    fn key_directory_is_extensible_and_round_robin() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("mia.key"), "sk-rc-mia\n").unwrap();
        std::fs::write(directory.path().join("alex.key"), "sk-rc-alex\n").unwrap();
        std::fs::write(directory.path().join("ignore.txt"), "sk-rc-ignore\n").unwrap();

        let pool = ApiKeyPool::new(Some(PathBuf::from(directory.path())), None).unwrap();

        assert_eq!(pool.next_cycle().unwrap(), ["sk-rc-alex", "sk-rc-mia"]);
        assert_eq!(pool.next_cycle().unwrap(), ["sk-rc-mia", "sk-rc-alex"]);

        std::fs::write(directory.path().join("zoe.key"), "sk-rc-zoe\n").unwrap();
        assert_eq!(
            pool.next_cycle().unwrap(),
            ["sk-rc-zoe", "sk-rc-alex", "sk-rc-mia"],
            "new .key files become available without code changes or a restart"
        );
    }

    #[test]
    fn key_directory_rejects_malformed_keys() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("bad.key"), "contains whitespace\n").unwrap();

        let error = ApiKeyPool::new(Some(directory.path().to_path_buf()), None).unwrap_err();
        assert!(error.to_string().contains("bad.key"));
        assert!(!error.to_string().contains("contains whitespace"));
    }

    #[tokio::test]
    async fn failed_key_rotates_before_the_provider_gate_waits() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let server = tokio::spawn({
            let seen = Arc::clone(&seen);
            async move {
                for response in [
                    "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 4\r\n\r\nslow".to_string(),
                    successful_response("ok"),
                ] {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = vec![0; 8192];
                    let read = stream.read(&mut request).await.unwrap();
                    let request = String::from_utf8_lossy(&request[..read]);
                    let auth = request
                        .lines()
                        .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
                        .unwrap()
                        .trim()
                        .to_string();
                    seen.lock().unwrap().push(auth);
                    stream.write_all(response.as_bytes()).await.unwrap();
                }
            }
        });
        let provider = HttpProvider::new_with_api_keys(
            format!("http://{address}"),
            vec!["sk-rc-one".into(), "sk-rc-two".into()],
            "m",
        )
        .unwrap()
        .with_requests_per_minute(None);
        let attempts = Attempts::default();

        let response = provider
            .complete_counted(
                ChatRequest::new("m", vec![super::super::Message::user("hi")]),
                &attempts,
            )
            .await
            .unwrap();

        assert_eq!(response.text(), "ok");
        assert_eq!(
            attempts.resends(),
            0,
            "key rotation is one logical send cycle"
        );
        assert!(provider.upstream_gate().unwrap().status().is_available());
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "authorization: Bearer sk-rc-one",
                "authorization: Bearer sk-rc-two"
            ]
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn provider_wait_starts_only_after_every_key_fails() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let server = tokio::spawn({
            let seen = Arc::clone(&seen);
            async move {
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = vec![0; 8192];
                    let read = stream.read(&mut request).await.unwrap();
                    let request = String::from_utf8_lossy(&request[..read]);
                    let auth = request
                        .lines()
                        .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
                        .unwrap()
                        .trim()
                        .to_string();
                    seen.lock().unwrap().push(auth);
                    stream
                        .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 3\r\n\r\nbad")
                        .await
                        .unwrap();
                }
            }
        });
        let provider = HttpProvider::new_with_api_keys(
            format!("http://{address}"),
            vec!["sk-rc-one".into(), "sk-rc-two".into()],
            "m",
        )
        .unwrap()
        .with_requests_per_minute(None)
        .with_probe_backoff(ProbeBackoff::new(
            Duration::from_secs(60),
            Duration::from_secs(60),
        ));

        let result = tokio::time::timeout(
            Duration::from_millis(250),
            provider.complete(ChatRequest::new(
                "m",
                vec![super::super::Message::user("hi")],
            )),
        )
        .await;

        assert!(
            result.is_err(),
            "the exhausted pool should wait behind the gate"
        );
        assert!(!provider.upstream_gate().unwrap().status().is_available());
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "every key is tried once first"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_request_is_terminal_without_rotating_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 7\r\n\r\ninvalid")
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_ok()
        });
        let provider = HttpProvider::new_with_api_keys(
            format!("http://{address}"),
            vec!["sk-rc-one".into(), "sk-rc-two".into()],
            "m",
        )
        .unwrap()
        .with_requests_per_minute(None);

        let error = provider
            .complete(ChatRequest::new(
                "m",
                vec![super::super::Message::user("hi")],
            ))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("HTTP 400"));
        assert!(provider.upstream_gate().unwrap().status().is_available());
        assert!(
            !server.await.unwrap(),
            "a permanent 4xx must not try another key"
        );
    }

    fn successful_response(text: &str) -> String {
        let body = serde_json::json!({
            "id": "chatcmpl-key-rotation",
            "model": "m",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
        .to_string();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn retry_after_parses_delta_seconds() {
        let now = datetime!(2026-10-09 12:00:00 UTC);
        assert_eq!(parse_retry_after("7", now), Some(Duration::from_secs(7)));
        assert_eq!(
            parse_retry_after(" 2 ", now),
            Some(Duration::from_secs(2)),
            "surrounding whitespace is tolerated"
        );
    }

    #[test]
    fn retry_after_parses_an_http_date() {
        let now = datetime!(2026-10-09 12:00:00 UTC);
        let header = (now + Duration::from_secs(30))
            .format(&Rfc2822)
            .unwrap()
            .replace("+0000", "GMT");
        assert_eq!(
            parse_retry_after(&header, now),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn retry_after_dates_in_the_past_mean_no_extra_wait() {
        let now = datetime!(2026-10-09 12:00:00 UTC);
        let header = (now - Duration::from_secs(30))
            .format(&Rfc2822)
            .unwrap()
            .replace("+0000", "GMT");
        assert_eq!(parse_retry_after(&header, now), Some(Duration::ZERO));
    }

    #[test]
    fn malformed_retry_after_values_are_ignored() {
        let now = datetime!(2026-10-09 12:00:00 UTC);
        for bad in ["soon", "-5", "", "1.5", "99999999999999999999999999"] {
            assert_eq!(parse_retry_after(bad, now), None, "{bad:?}");
        }
    }

    #[test]
    fn retry_after_delta_seconds_above_300_are_honored_uncapped() {
        let now = datetime!(2026-10-09 12:00:00 UTC);
        assert_eq!(
            parse_retry_after("301", now),
            Some(Duration::from_secs(301))
        );
        assert_eq!(
            parse_retry_after("100000", now),
            Some(Duration::from_secs(100_000)),
            "a valid server instruction must not be silently shortened"
        );
    }

    #[test]
    fn retry_after_http_dates_are_not_rounded_down_against_a_fractional_now() {
        // The wall clock is mid-second when the header arrives; the true
        // delta is 30.6 s and must not be truncated to 30 s (under-waiting).
        let now = datetime!(2026-10-09 12:00:00.4 UTC);
        let header = datetime!(2026-10-09 12:00:31 UTC)
            .format(&Rfc2822)
            .unwrap()
            .replace("+0000", "GMT");
        assert_eq!(
            parse_retry_after(&header, now),
            Some(Duration::from_millis(30_600))
        );
    }

    #[test]
    fn provider_request_timeout_defaults_to_thirty_minutes_and_is_configurable() {
        assert_eq!(
            parse_request_timeout(None).unwrap(),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            parse_request_timeout(Some("2400")).unwrap(),
            Duration::from_secs(2400)
        );
        assert_eq!(
            parse_request_timeout(Some("21")).unwrap(),
            Duration::from_secs(21)
        );
        for invalid in ["0", "20", "-1", "1.5", "nope"] {
            assert!(
                parse_request_timeout(Some(invalid)).is_err(),
                "invalid timeout {invalid:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn completion_timeouts_keep_retrying_until_cancelled() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicU32::new(0));
        let server = tokio::spawn({
            let accepted = Arc::clone(&accepted);
            async move {
                let mut connections = Vec::new();
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    accepted.fetch_add(1, Ordering::SeqCst);
                    connections.push(stream);
                }
            }
        });
        let provider = HttpProvider::new_with_timeout(
            format!("http://{address}"),
            None,
            "m",
            Duration::from_millis(20),
        )
        .unwrap()
        .with_probe_backoff(ProbeBackoff::new(
            Duration::from_millis(5),
            Duration::from_millis(20),
        ));
        let attempts = Attempts::default();

        let result = tokio::time::timeout(
            Duration::from_millis(300),
            provider.complete_counted(
                ChatRequest::new("m", vec![super::super::Message::user("hi")]),
                &attempts,
            ),
        )
        .await;
        assert!(
            result.is_err(),
            "an unavailable provider must not terminate the logical call: {result:?}"
        );
        assert!(
            accepted.load(Ordering::SeqCst) >= 2,
            "timed-out requests must be retried"
        );
        assert!(attempts.resends() >= 1);
        let gate = provider.upstream_gate().unwrap();
        assert!(
            !gate.status().is_available(),
            "waiting must be visible to the scheduler and research clock"
        );
        assert!(
            !gate.status().probe_in_flight(),
            "dropping the future must release the probe"
        );
        server.abort();
    }

    #[tokio::test]
    async fn retryable_status_does_not_wait_for_a_stalled_body() {
        stalled_response_recovers(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 100\r\n\r\n",
        )
        .await;
    }

    #[tokio::test]
    async fn completion_timeout_recovers_after_provider_answers_again() {
        // A successful header with a body that never arrives must also be
        // retried; header-only availability is not a usable completion.
        stalled_response_recovers(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n").await;
    }

    async fn stalled_response_recovers(headers: &'static [u8]) {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stalled, _) = listener.accept().await.unwrap();
            stalled.write_all(headers).await.unwrap();

            let (mut recovered, _) = listener.accept().await.unwrap();
            let body = serde_json::json!({
                "id": "chatcmpl-1",
                "object": "chat.completion",
                "model": "m",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })
            .to_string();
            recovered
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let provider = HttpProvider::new_with_timeout(
            format!("http://{address}"),
            None,
            "m",
            if headers.starts_with(b"HTTP/1.1 200") {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(2)
            },
        )
        .unwrap()
        .with_probe_backoff(ProbeBackoff::new(
            Duration::from_millis(1),
            Duration::from_millis(1),
        ));
        let attempts = Attempts::default();

        let response = tokio::time::timeout(
            Duration::from_millis(1000),
            provider.complete_counted(
                ChatRequest::new("m", vec![super::super::Message::user("hi")]),
                &attempts,
            ),
        )
        .await
        .expect("the retryable status must be classified from its headers")
        .unwrap();

        assert_eq!(response.text(), "ok");
        assert_eq!(attempts.resends(), 1);
        server.await.unwrap();
    }

    #[test]
    fn clip_keeps_strings_within_the_limit_intact() {
        assert_eq!(clip("boom", 300), "boom");
        assert_eq!(clip("", 300), "");
    }

    #[test]
    fn clip_truncates_over_limit_strings_on_a_char_boundary() {
        assert_eq!(clip("aaaaaaaa", 4), "aaaa…");
        // 'é' is two bytes; a limit inside the char must back up, not split it.
        assert_eq!(clip("ééé", 3), "é…");
    }

    fn paced(url: &str) -> HttpProvider {
        HttpProvider::new(url, None, "m")
            .unwrap()
            .with_requests_per_minute(NonZeroU32::new(5))
    }

    fn pacer_of(p: &HttpProvider) -> Arc<StartPacer> {
        p.pacer().expect("provider is paced")
    }

    #[test]
    fn pacer_identity_ignores_url_spelling_of_the_same_upstream() {
        let plain = paced("https://pacer-id.example/v1");
        let explicit_port = paced("https://pacer-id.example:443/v1");
        let userinfo_and_case = paced("https://key@PACER-ID.example/v1");
        assert!(
            Arc::ptr_eq(&pacer_of(&plain), &pacer_of(&explicit_port)),
            "an explicit default port must not create a second limiter"
        );
        assert!(
            Arc::ptr_eq(&pacer_of(&plain), &pacer_of(&userinfo_and_case)),
            "userinfo and host casing must not create a second limiter"
        );
    }

    #[test]
    fn pacer_identity_distinguishes_different_effective_ports() {
        let https = paced("https://pacer-port.example/v1");
        let http = paced("http://pacer-port.example/v1");
        assert!(
            !Arc::ptr_eq(&pacer_of(&https), &pacer_of(&http)),
            "port 443 and port 80 are different upstream endpoints"
        );
    }

    #[test]
    fn differing_rpm_targets_share_one_pacer_at_the_stricter_rate() {
        let strict = HttpProvider::new("https://rpm-conflict.example/v1", None, "m")
            .unwrap()
            .with_requests_per_minute(NonZeroU32::new(14));
        let loose = HttpProvider::new("https://rpm-conflict.example/v1", None, "m")
            .unwrap()
            .with_requests_per_minute(NonZeroU32::new(30));
        let a = pacer_of(&strict);
        let b = pacer_of(&loose);
        assert!(
            Arc::ptr_eq(&a, &b),
            "one upstream identity must never get two independent schedules"
        );
        assert_eq!(
            b.interval(),
            Duration::from_secs(60).div_f64(14.0),
            "the stricter configured allowance must bind both handles"
        );
    }
}
