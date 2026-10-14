//! OpenAI-compatible HTTP provider (SPEC §9.1).
//!
//! Speaks `POST {base_url}/chat/completions` with the wire types in the parent
//! module, which is what OpenAI, Anthropic's compatibility endpoint, Together,
//! Groq, vLLM, llama.cpp, Ollama and OpenRouter accept.
//!
//! This adapter classifies every outcome of a send as served, permanently
//! refused, or transient, and nothing more: a transient failure (a
//! transport error, or a 408, 425, 429, or 5xx) is waited out through the
//! process-wide [`UpstreamGate`] of the upstream — one half-open probe at a
//! time, capped backoff, `Retry-After` honored — until the upstream serves
//! the request. The wait is bounded only by the caller's cancellation
//! (the run's stop request; see [`crate::agents::AgentContext::complete`]),
//! which drops the in-flight send and any pending probe. A permanent
//! refusal (another 4xx, an unparseable reply, a malformed request) is
//! [`Error::Provider`] at once. Request-start pacing applies to every send,
//! probes included.

use super::availability::{SendError, complete_through_gate};
use super::pacing::StartPacer;
use super::{
    Attempts, ChatRequest, ChatResponse, Choice, FinishReason, Message, ProbeBackoff, Provider,
    TransientFault, TransportPhase, UpstreamGate, Usage,
};
use crate::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

/// Environment variables that configure the provider.
pub const ENV_URL: &str = "URUK_PROVIDER_URL";
pub const ENV_KEY: &str = "URUK_API_KEY";
pub const ENV_MODEL: &str = "URUK_MODEL";
/// Optional `reasoning_effort` sent with every request (`none`, `low`, `medium`,
/// `high`). Reasoning models spend most of their time thinking otherwise.
pub const ENV_REASONING: &str = "URUK_REASONING_EFFORT";
/// Optional positive overall completion-request timeout in seconds. This is
/// deliberately much longer than the connect timeout because large model
/// generations can take many minutes even when streamed.
pub const ENV_TIMEOUT_SECS: &str = "URUK_PROVIDER_TIMEOUT_SECS";
/// Optional request-start pacing in requests per minute. Unset keeps the
/// per-endpoint default ([`SWISSAI_DEFAULT_RPM`] for SwissAI hosts, unpaced
/// otherwise); `0` disables pacing explicitly.
pub const ENV_RPM: &str = "URUK_PROVIDER_RPM";

/// SwissAI publishes a 14 requests/minute limit on its OpenAI-compatible
/// endpoint, so providers pointed at a SwissAI host pace request starts to
/// one per 60/14 s unless [`ENV_RPM`] overrides it.
pub const SWISSAI_DEFAULT_RPM: u32 = 14;
/// A 20,000-token completion can legitimately take more than five minutes;
/// keep the connection open while the provider is working.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30 * 60;

#[derive(Debug, Clone)]
pub struct HttpProvider {
    client: reqwest::Client,
    request_timeout: Duration,
    base_url: String,
    api_key: Option<String>,
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
    /// Stream response chunks so long generations keep intermediary proxies
    /// alive. Enabled by default for SwissAI, whose proxy otherwise terminates
    /// silent non-streaming requests at about five minutes.
    streaming: bool,
    /// This handle's pacing target; `None` means unpaced. The shared
    /// schedule it resolves to at request time (see [`Self::pacer`]) is one
    /// per upstream identity, making the limit process-wide.
    rpm: Option<NonZeroU32>,
}

fn parse_request_timeout(value: Option<&str>) -> Result<Duration> {
    let raw = value.unwrap_or("1800").trim();
    let seconds = raw.parse::<u64>().map_err(|_| {
        Error::validation(format!(
            "{ENV_TIMEOUT_SECS} must be a positive whole number of seconds, got {raw:?}"
        ))
    })?;
    if seconds == 0 {
        return Err(Error::validation(format!(
            "{ENV_TIMEOUT_SECS} must be greater than zero"
        )));
    }
    Ok(Duration::from_secs(seconds))
}

const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    cost_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    index: u32,
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<FinishReason>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Default)]
struct PartialChoice {
    content: String,
    finish_reason: Option<FinishReason>,
}

#[derive(Debug, Default)]
struct StreamAssembly {
    id: String,
    model: String,
    choices: BTreeMap<u32, PartialChoice>,
    usage: Option<Usage>,
    cost_usd: Option<f64>,
    done: bool,
}

impl StreamAssembly {
    fn accept(&mut self, data: &str) -> std::result::Result<(), SendError> {
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let chunk: StreamChunk = serde_json::from_str(data).map_err(|error| {
            SendError::Permanent(Error::Provider(format!(
                "unparseable streaming chat completion ({error}): {}",
                clip(data, 300)
            )))
        })?;
        if !chunk.id.is_empty() {
            self.id = chunk.id;
        }
        if !chunk.model.is_empty() {
            self.model = chunk.model;
        }
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage);
        }
        if chunk.cost_usd.is_some() {
            self.cost_usd = chunk.cost_usd;
        }
        for choice in chunk.choices {
            let partial = self.choices.entry(choice.index).or_default();
            if let Some(content) = choice.delta.content {
                partial.content.push_str(&content);
            }
            if choice.finish_reason.is_some() {
                partial.finish_reason = choice.finish_reason;
            }
        }
        Ok(())
    }

    fn finish(self, fallback_model: &str) -> std::result::Result<ChatResponse, SendError> {
        if !self.done {
            return Err(SendError::Transient(TransientFault::transport(
                TransportPhase::Response,
                "stream ended before the [DONE] marker",
            )));
        }
        if self.choices.is_empty() {
            return Err(SendError::Permanent(Error::Provider(
                "streaming chat completion contained no choices".into(),
            )));
        }
        let usage = self.usage.ok_or_else(|| {
            SendError::Permanent(Error::Provider(
                "streaming chat completion contained no usage summary".into(),
            ))
        })?;
        let choices = self
            .choices
            .into_iter()
            .map(|(index, partial)| {
                let finish_reason = partial.finish_reason.ok_or_else(|| {
                    SendError::Permanent(Error::Provider(format!(
                        "streaming chat completion choice {index} had no finish reason"
                    )))
                })?;
                Ok(Choice {
                    index,
                    message: Message::assistant(partial.content),
                    finish_reason,
                })
            })
            .collect::<std::result::Result<Vec<_>, SendError>>()?;
        Ok(ChatResponse {
            id: self.id,
            model: if self.model.is_empty() {
                fallback_model.to_string()
            } else {
                self.model
            },
            choices,
            usage,
            cost_usd: self.cost_usd,
        })
    }
}

fn line_ending_len(buffer: &[u8], index: usize) -> Option<usize> {
    match buffer.get(index) {
        Some(b'\n') => Some(1),
        Some(b'\r') if buffer.get(index + 1) == Some(&b'\n') => Some(2),
        Some(b'\r') => Some(1),
        _ => None,
    }
}

fn next_sse_event(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let mut index = 0;
    while index < buffer.len() {
        let Some(first_len) = line_ending_len(buffer, index) else {
            index += 1;
            continue;
        };
        let next = index + first_len;
        if let Some(second_len) = line_ending_len(buffer, next) {
            let event = buffer[..index].to_vec();
            buffer.drain(..next + second_len);
            return Some(event);
        }
        index = next;
    }
    None
}

fn sse_data(event: &[u8]) -> std::result::Result<Option<String>, SendError> {
    let text = std::str::from_utf8(event).map_err(|error| {
        SendError::Permanent(Error::Provider(format!(
            "streaming chat completion was not UTF-8: {error}"
        )))
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut values = Vec::new();
    for line in normalized.lines() {
        let Some(value) = line.strip_prefix("data:") else {
            continue;
        };
        values.push(value.strip_prefix(' ').unwrap_or(value));
    }
    Ok((!values.is_empty()).then(|| values.join("\n")))
}

async fn read_streaming_response(
    mut response: reqwest::Response,
    request_timeout: Duration,
    fallback_model: &str,
) -> std::result::Result<ChatResponse, SendError> {
    let mut received = 0usize;
    let mut buffer = Vec::new();
    let mut assembly = StreamAssembly::default();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| classify_request_error(error, request_timeout))?
    {
        received = received.checked_add(chunk.len()).ok_or_else(|| {
            SendError::Permanent(Error::Provider("streaming response was too large".into()))
        })?;
        if received > MAX_STREAM_BYTES {
            return Err(SendError::Permanent(Error::Provider(format!(
                "streaming response exceeded {MAX_STREAM_BYTES} bytes"
            ))));
        }
        buffer.extend_from_slice(&chunk);
        while let Some(event) = next_sse_event(&mut buffer) {
            if let Some(data) = sse_data(&event)? {
                assembly.accept(&data)?;
                if assembly.done {
                    return assembly.finish(fallback_model);
                }
            }
        }
    }
    // A trailing, unterminated event is not dispatchable SSE. If the socket
    // closed mid-JSON, the missing [DONE] is a retryable transport truncation,
    // not a permanent JSON-protocol failure.
    assembly.finish(fallback_model)
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

/// Whether `host` (no port) is a SwissAI upstream.
///
/// Matches on whole dot-separated host labels (`api.swissai.cscs.ch`,
/// `swiss-ai.example`), so unrelated hosts that merely contain the substring
/// (`swissair.example.com`, `notswissai.example.com`) stay unpaced.
fn is_swissai_host(host: &str) -> bool {
    host.split('.').any(|label| {
        label.eq_ignore_ascii_case("swissai") || label.eq_ignore_ascii_case("swiss-ai")
    })
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
            .connect_timeout(Duration::from_secs(20))
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

        let default_rpm = is_swissai_host(host)
            .then(|| NonZeroU32::new(SWISSAI_DEFAULT_RPM))
            .flatten();
        Ok(Self {
            client,
            request_timeout,
            base_url,
            api_key,
            model: model.into(),
            name: format!("openai-compatible@{display_host}"),
            upstream,
            probe_backoff: ProbeBackoff::DEFAULT,
            reasoning_effort: None,
            streaming: is_swissai_host(host),
            rpm: default_rpm,
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
        let timeout = parse_request_timeout(
            std::env::var(ENV_TIMEOUT_SECS)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .as_deref(),
        )?;
        let mut provider =
            Self::new_with_timeout(url, key, model, timeout)?.with_reasoning_effort(effort);
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
    /// set earlier on the same handle (including the SwissAI default). All
    /// handles of one upstream identity (effective host and port) share one
    /// process-wide schedule paced at the *strictest* target among them —
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
    /// paced. Resolution happens at request time, not configuration time, so
    /// a provisional target (e.g. the SwissAI default before an
    /// [`ENV_RPM`] override lands) never tightens the shared schedule.
    fn pacer(&self) -> Option<Arc<StartPacer>> {
        self.rpm.map(|rpm| {
            let interval = Duration::from_secs(60).div_f64(f64::from(rpm.get()));
            StartPacer::shared(&self.upstream, interval)
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

    /// Enable or disable SSE streaming for chat completions. SwissAI handles
    /// default to streaming; this setter primarily supports explicit adapters
    /// and deterministic stand-in tests.
    pub fn with_streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    pub fn streaming(&self) -> bool {
        self.streaming
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The process-wide availability gate of this handle's upstream.
    fn gate(&self) -> Arc<UpstreamGate> {
        UpstreamGate::shared(&self.upstream)
    }

    /// Send the request once and classify what came back.
    async fn send_once(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> std::result::Result<ChatResponse, SendError> {
        let mut req = self.client.post(url).json(body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|error| classify_request_error(error, self.request_timeout))?;
        let status = resp.status();
        let event_stream = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"));
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| parse_retry_after(v, time::OffsetDateTime::now_utc()));
        if status.is_success() && self.streaming && event_stream {
            return read_streaming_response(resp, self.request_timeout, &self.model).await;
        }
        let text = resp
            .text()
            .await
            .map_err(|error| classify_request_error(error, self.request_timeout))?;
        if status.is_success() {
            return serde_json::from_str::<ChatResponse>(&text).map_err(|e| {
                SendError::Permanent(Error::Provider(format!(
                    "unparseable chat completion ({e}): {}",
                    clip(&text, 300)
                )))
            });
        }
        let code = status.as_u16();
        if TransientFault::is_transient_status(code) {
            return Err(SendError::Transient(TransientFault::status(
                code,
                retry_after,
                clip(&text, 300),
            )));
        }
        Err(SendError::Permanent(Error::Provider(format!(
            "HTTP {status}: {}",
            clip(&text, 300)
        ))))
    }
}

/// A request that produced no response at all: a configuration or
/// redirect problem is permanent; everything else is a transport fault
/// of the upstream, waited out.
fn classify_request_error(e: reqwest::Error, request_timeout: Duration) -> SendError {
    if e.is_builder() || e.is_redirect() {
        return SendError::Permanent(Error::Provider(format!("request failed: {e}")));
    }
    if e.is_connect() {
        return SendError::Transient(TransientFault::transport(
            TransportPhase::Connect,
            e.to_string(),
        ));
    }
    if e.is_timeout() {
        return SendError::Transient(TransientFault::timeout(request_timeout));
    }
    let phase = if e.is_body() || e.is_decode() {
        TransportPhase::Response
    } else {
        TransportPhase::Send
    };
    SendError::Transient(TransientFault::transport(phase, e.to_string()))
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
        if self.streaming {
            body["stream"] = serde_json::Value::Bool(true);
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }
        let gate = self.gate();
        let pacer = self.pacer();
        complete_through_gate(
            &gate,
            pacer.as_deref(),
            &self.probe_backoff,
            attempts,
            || self.send_once(&url, &body),
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use time::format_description::well_known::Rfc2822;
    use time::macros::datetime;

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
    fn swissai_defaults_to_streaming_but_other_openai_endpoints_do_not() {
        assert!(
            HttpProvider::new("https://api.swissai.svc.cscs.ch/v1", None, "m")
                .unwrap()
                .streaming()
        );
        assert!(
            !HttpProvider::new("https://api.openai.com/v1", None, "m")
                .unwrap()
                .streaming()
        );
    }

    #[test]
    fn an_incomplete_stream_is_retryable_instead_of_becoming_partial_output() {
        let mut assembly = StreamAssembly::default();
        assembly
            .accept(
                r#"{"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        let SendError::Transient(fault) = assembly.finish("m").unwrap_err() else {
            panic!("a stream without [DONE] must be retried")
        };
        assert!(fault.to_string().contains("before the [DONE] marker"));
    }

    #[test]
    fn sse_decoder_accepts_crlf_boundaries() {
        let mut buffer = b"data: {\"choices\":[]}\r\n\r\ndata: [DONE]\r\n\r\n".to_vec();
        let first = next_sse_event(&mut buffer).unwrap();
        let second = next_sse_event(&mut buffer).unwrap();
        assert_eq!(
            sse_data(&first).unwrap().as_deref(),
            Some("{\"choices\":[]}")
        );
        assert_eq!(sse_data(&second).unwrap().as_deref(), Some("[DONE]"));
        assert!(buffer.is_empty());
    }

    #[test]
    fn sse_decoder_accepts_mixed_and_bare_cr_line_endings_and_a_bom() {
        let mut buffer = b"\xef\xbb\xbfdata: one\r\ndata: two\r\rdata: [DONE]\n\r".to_vec();
        let first = next_sse_event(&mut buffer).unwrap();
        let second = next_sse_event(&mut buffer).unwrap();
        assert_eq!(sse_data(&first).unwrap().as_deref(), Some("one\ntwo"));
        assert_eq!(sse_data(&second).unwrap().as_deref(), Some("[DONE]"));
        assert!(buffer.is_empty());
    }

    #[test]
    fn completed_stream_without_usage_is_rejected_instead_of_counted_as_zero() {
        let mut assembly = StreamAssembly::default();
        assembly
            .accept(
                r#"{"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        assembly.accept("[DONE]").unwrap();
        let SendError::Permanent(error) = assembly.finish("m").unwrap_err() else {
            panic!("missing usage must be a protocol error")
        };
        assert!(error.to_string().contains("usage"));
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
        for invalid in ["0", "-1", "1.5", "nope"] {
            assert!(
                parse_request_timeout(Some(invalid)).is_err(),
                "invalid timeout {invalid:?} must be rejected"
            );
        }
    }

    #[tokio::test]
    async fn request_timeouts_are_reported_as_timeouts() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let error = reqwest::Client::builder()
            .timeout(Duration::from_millis(20))
            .build()
            .unwrap()
            .post(format!("http://{address}"))
            .send()
            .await
            .unwrap_err();

        let SendError::Transient(fault) = classify_request_error(error, Duration::from_millis(20))
        else {
            panic!("a request timeout must remain retryable");
        };
        assert_eq!(
            fault.to_string(),
            "response failed: request timed out after 20ms while waiting for the model provider"
        );
        server.abort();
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

    #[test]
    fn a_provisional_swissai_default_does_not_tighten_an_override() {
        // `from_env` applies `ENV_RPM` after `new` has already set the
        // SwissAI default target; the provisional 14 must not ratchet the
        // shared schedule below the explicit override.
        let p = HttpProvider::new("https://override.swiss-ai.example/v1", None, "m")
            .unwrap()
            .with_requests_per_minute(NonZeroU32::new(30));
        assert_eq!(pacer_of(&p).interval(), Duration::from_secs(2));
    }
}
