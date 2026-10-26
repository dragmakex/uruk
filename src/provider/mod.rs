//! Model provider adapters (SPEC §9.1).
//!
//! Wire types follow the OpenAI `/chat/completions` shape, which is the
//! de-facto format for OpenAI, Anthropic's compatibility endpoint, Together,
//! Groq, vLLM, llama.cpp, Ollama and OpenRouter. An HTTP adapter therefore
//! only has to serialize [`ChatRequest`] and parse [`ChatResponse`].
//!
//! Model selection stays outside research policy (SPEC §13): agents receive a
//! `&dyn Provider` and never name a vendor.

mod availability;
mod http;
mod keys;
mod mock;
mod pacing;

pub use availability::{
    Admission, GateStatus, ProbeBackoff, TransientFault, TransportPhase, UpstreamGate,
};
pub use http::{
    DEFAULT_REQUEST_TIMEOUT_SECS, ENV_KEY, ENV_MODEL, ENV_REASONING, ENV_RPM, ENV_TIMEOUT_SECS,
    ENV_URL, HttpProvider,
};
pub use mock::{MockProvider, MockRule, Reply};
pub use pacing::StartPacer;

use crate::records::CostActual;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// A conversation role, as the wire format spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// One message in a request or response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
        }
    }
}

/// An OpenAI-shaped chat completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Passed through where a provider honours it; recorded either way so a
    /// task's sampling configuration is auditable (SPEC §5.1). On the wire it
    /// is reduced to 31 bits, see [`wire_seed`].
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "wire_seed")]
    pub seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

/// OpenAI-compatible servers parse `seed` as a signed machine integer (Ollama
/// uses Go's `int`, llama.cpp a 32-bit value), so anything above `i32::MAX`
/// is rejected with a 400. The mapping is fixed, so a recorded seed still
/// reproduces the same request.
fn wire_seed<S: serde::Serializer>(
    seed: &Option<u64>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    serde::Serialize::serialize(&seed.map(|v| v & 0x7fff_ffff), s)
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: None,
            max_tokens: None,
            top_p: None,
            seed: None,
            stop: None,
        }
    }

    pub fn with_temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Concatenated prompt text, for content hashing and debugging.
    pub fn rendered(&self) -> String {
        self.messages
            .iter()
            .map(|m| format!("[{:?}] {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Why the model stopped generating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
    /// Any other reason a provider reports (`tool_calls`, `end_turn`, ...).
    #[serde(other)]
    Other,
}

/// Token accounting, as reported by the provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// One returned choice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: Message,
    pub finish_reason: FinishReason,
}

/// An OpenAI-shaped chat completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Usage,
    /// Cost in USD when the provider reports it. `None` means unknown, which
    /// SPEC §6 requires be reported as unknown rather than recorded as zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl ChatResponse {
    /// Text of the first choice.
    pub fn text(&self) -> &str {
        self.choices
            .first()
            .map(|c| c.message.content.as_str())
            .unwrap_or("")
    }

    /// Whether the response was cut off by the token limit.
    ///
    /// A truncated completion cannot be trusted to contain a complete typed
    /// record, so callers treat it as a validation failure (SPEC §5.1).
    pub fn truncated(&self) -> bool {
        self.choices
            .first()
            .map(|c| c.finish_reason == FinishReason::Length)
            .unwrap_or(false)
    }

    /// This response's contribution to a task's recorded cost.
    pub fn cost(&self) -> CostActual {
        CostActual {
            model_calls: 1,
            prompt_tokens: self.usage.prompt_tokens,
            completion_tokens: self.usage.completion_tokens,
            tool_executions: 0,
            cost_usd: self.cost_usd,
        }
    }
}

/// The caller's authority over whether one more logical model call may be
/// made. Implemented over durable run accounting (see
/// [`crate::agents::CallAllowance`]); the adapter only asks.
#[async_trait::async_trait]
pub trait AttemptBudget: Send + Sync + std::fmt::Debug {
    /// Claim a model call for the logical call with the given `ordinal`,
    /// unique across every completion of the task attempt (see
    /// [`Attempts`]). `Ok(false)` means the budget cannot cover it, so the
    /// request must not go out.
    async fn claim(&self, ordinal: u32) -> Result<bool>;

    /// Record that the logical call `ordinal` is being sent again (its
    /// `resend`-th resend, from 1) after a transient upstream failure. An
    /// audit fact, not a charge: the upstream served nothing. The default
    /// records nothing.
    async fn note_resend(&self, _ordinal: u32, _resend: u32) -> Result<()> {
        Ok(())
    }
}

/// The logical model calls of one completion, claimed as each starts and
/// gated by the caller's [`AttemptBudget`], plus the resends each needed.
///
/// `started` is local to this completion and is what the caller charges
/// for it: one logical call per completion, claimed before the first send.
/// `sequence` is shared across every completion of one task attempt (a
/// debate makes several), so each logical call carries an ordinal that is
/// unique task-attempt-wide; the budget keys its durable records by it, and
/// no two calls can ever share a record. Resends after a transient
/// failure are counted and audited but claim no further call.
///
/// Shared with the caller so the counts outlive the call: when cancellation
/// drops the in-flight future, the call that was already claimed is still
/// on record, and may still be billed (SPEC §9.2: preserve possible
/// incurred cost). A call the budget refuses is never sent.
#[derive(Debug, Default)]
pub struct Attempts {
    started: AtomicU32,
    /// Ordinal of the logical call most recently claimed.
    ordinal: AtomicU32,
    resends: AtomicU32,
    sequence: Arc<AtomicU32>,
    /// `None` is unbounded: callers outside a budgeted run.
    budget: Option<Arc<dyn AttemptBudget>>,
    diagnostics: Option<crate::diagnostics::LogContext>,
}

impl Attempts {
    pub(crate) fn with_diagnostics(mut self, context: crate::diagnostics::LogContext) -> Self {
        self.diagnostics = Some(context);
        self
    }
    /// Attempts gated by `budget`, numbered from the shared `sequence`.
    pub fn bounded(budget: Arc<dyn AttemptBudget>, sequence: Arc<AtomicU32>) -> Self {
        Self {
            started: AtomicU32::new(0),
            ordinal: AtomicU32::new(0),
            resends: AtomicU32::new(0),
            sequence,
            budget: Some(budget),
            diagnostics: None,
        }
    }

    /// Claim a logical call and record that it is about to be sent;
    /// returns this completion's call number, from 1, or `None` when the
    /// budget is spent, in which case nothing was recorded and the request
    /// must not go out. A refused ordinal is consumed, never reused.
    ///
    /// # Errors
    ///
    /// Propagates a failure of the budget's own store.
    pub async fn try_start(&self) -> Result<Option<u32>> {
        let ordinal = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(budget) = &self.budget
            && !budget.claim(ordinal).await?
        {
            return Ok(None);
        }
        self.ordinal.store(ordinal, Ordering::SeqCst);
        Ok(Some(self.started.fetch_add(1, Ordering::SeqCst) + 1))
    }

    /// Record one more send of the current logical call after a transient
    /// failure; returns the resend number, from 1.
    ///
    /// # Errors
    ///
    /// Propagates a failure of the budget's own store.
    pub async fn resend(&self) -> Result<u32> {
        let resend = self.resends.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(budget) = &self.budget {
            budget
                .note_resend(self.ordinal.load(Ordering::SeqCst), resend)
                .await?;
        }
        Ok(resend)
    }

    /// Logical calls claimed so far: what the caller is charged.
    pub fn count(&self) -> u32 {
        self.started.load(Ordering::SeqCst)
    }

    /// Sends beyond the first of each logical call, across the completion.
    pub fn resends(&self) -> u32 {
        self.resends.load(Ordering::SeqCst)
    }
}

/// The error for a call the budget would not cover, worded so a task
/// reports budget exhaustion rather than a provider outage.
fn budget_refused(attempts: &Attempts, last_error: Option<&str>) -> Error {
    let sent = attempts.count();
    let plural = if sent == 1 { "" } else { "s" };
    let message = match (sent, last_error) {
        (0, _) => "model-call budget exhausted before the request could be sent".to_string(),
        (n, Some(last)) => {
            format!("model-call budget exhausted after {n} call{plural}; last: {last}")
        }
        (n, None) => format!("model-call budget exhausted after {n} call{plural}"),
    };
    Error::Budget(message)
}

/// A model provider.
///
/// Implementations: [`HttpProvider`] for any OpenAI-compatible endpoint, and
/// [`MockProvider`] for tests and offline runs.
#[async_trait::async_trait]
pub trait Provider: Send + Sync + std::fmt::Debug {
    /// Identifier recorded on every task that used this provider.
    fn name(&self) -> &str;

    /// Default model when the plan does not name one.
    fn default_model(&self) -> &str;

    /// Perform one completion.
    ///
    /// Cancellation is the caller's concern: it races this future against a
    /// cancellation token, so implementations need not poll for it. This is
    /// the original trait surface; providers that only implement it are
    /// counted and budgeted by the default [`Provider::complete_counted`].
    async fn complete(&self, request: ChatRequest) -> Result<ChatResponse>;

    /// Perform one completion, calling [`Attempts::try_start`] before the
    /// first outbound request so the logical call is on record (and refused
    /// when the budget cannot cover it) even if the future is dropped
    /// before the reply arrives, and [`Attempts::resend`] before every
    /// further send of the same call after a transient failure.
    ///
    /// A transient upstream failure never ends the completion: the adapter
    /// waits for the upstream to recover (see [`UpstreamGate`]), bounded
    /// only by the caller dropping the future. An `Err` is a terminal
    /// request refusal, a budget refusal, or a store failure. Completion
    /// timeouts remain inside the adapter's recovery loop.
    ///
    /// The default claims exactly one call and delegates to
    /// [`Provider::complete`].
    async fn complete_counted(
        &self,
        request: ChatRequest,
        attempts: &Attempts,
    ) -> Result<ChatResponse> {
        if attempts.try_start().await?.is_none() {
            return Err(budget_refused(attempts, None));
        }
        self.complete(request).await
    }

    /// The availability gate of this provider's upstream, when it has one:
    /// the scheduler watches it to tell a run paused on a provider outage
    /// from a run doing research. `None` for providers without transient
    /// failure handling.
    fn upstream_gate(&self) -> Option<Arc<UpstreamGate>> {
        None
    }
}

/// Choose the model provider from the environment (SPEC §13: model selection
/// stays outside research policy).
///
/// With `URUK_PROVIDER_URL` and `URUK_MODEL` set, any OpenAI-compatible
/// endpoint is used. Without them the run proceeds offline against a
/// stand-in that says plainly that nothing was analysed. Every run the web
/// API dispatches goes through this exact selection.
pub fn from_env_or_offline() -> Result<std::sync::Arc<dyn Provider>> {
    if let Some(http) = HttpProvider::from_env()? {
        tracing::info!(
            provider = http.name(),
            model = http.default_model(),
            "using HTTP provider"
        );
        return Ok(std::sync::Arc::new(http));
    }
    tracing::warn!(
        "no model provider configured ({ENV_URL} / {ENV_MODEL} unset); running offline \
         with a stand-in that performs no analysis"
    );
    Ok(std::sync::Arc::new(
        MockProvider::new()
            .rule("Requested deliverable:", no_provider_deliverable())
            .default_reply(no_provider_review()),
    ))
}

/// Stand-in deliverable used when no model provider is configured.
///
/// Structurally valid, and explicit that nothing was analysed: SPEC §6 forbids
/// implying a missing check passed.
fn no_provider_deliverable() -> String {
    let body = concat!(
        "## No model provider configured\n\n",
        "This run completed source ingestion, record-keeping, and export without a ",
        "configured model provider, so no model-generated analysis was performed. The ",
        "sources listed in this report were ingested and hashed, but nothing in them has ",
        "been read or interpreted by a model.\n\n",
        "Set URUK_PROVIDER_URL, URUK_MODEL, and URUK_API_KEY and re-run to obtain ",
        "an analysed deliverable."
    );
    serde_json::json!({
        "title": "No model provider configured",
        "body": body,
        "claims": [],
        "disagreements": [],
        "limitations": "No model provider was configured, so no model reasoning contributed to this report and no source content was analysed.",
        "next_actions": [
            "Configure a model provider and re-run to obtain an analysed deliverable."
        ],
    })
    .to_string()
}

/// Stand-in review used when no model provider is configured.
fn no_provider_review() -> String {
    serde_json::json!({
        "assessment_text": "No model provider is configured for this run, so no review was performed.",
        "proposed_assessment": "inconclusive",
        "objections": [],
        "unknowns": ["No model provider was configured."],
        "next_actions": [],
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_omits_unset_optional_fields() {
        let req = ChatRequest::new("m", vec![Message::user("hi")]);
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("temperature").is_none());
        assert!(json.get("seed").is_none());
        assert_eq!(json["messages"][0]["role"], "user");
    }

    #[test]
    fn seed_fits_a_signed_32_bit_integer_on_the_wire() {
        let huge = ChatRequest::new("m", vec![Message::user("hi")]).with_seed(u64::MAX);
        assert_eq!(
            serde_json::to_value(&huge).unwrap()["seed"],
            i32::MAX as u64
        );
        let small = ChatRequest::new("m", vec![Message::user("hi")]).with_seed(7);
        assert_eq!(serde_json::to_value(&small).unwrap()["seed"], 7);
    }

    #[test]
    fn response_parses_the_openai_shape() {
        // Exactly what a /chat/completions endpoint returns.
        let raw = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 9, "completion_tokens": 2, "total_tokens": 11}
        }"#;
        let resp: ChatResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(resp.text(), "hello");
        assert_eq!(resp.usage.total_tokens, 11);
        assert!(!resp.truncated());
        assert_eq!(resp.cost().cost_usd, None, "unreported cost stays unknown");
    }

    #[test]
    fn length_finish_is_truncation() {
        let raw = r#"{
            "id": "x", "model": "m",
            "choices": [{"index":0,"message":{"role":"assistant","content":"par"},
                         "finish_reason":"length"}],
            "usage": {"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
        }"#;
        let resp: ChatResponse = serde_json::from_str(raw).unwrap();
        assert!(resp.truncated());
    }
}
