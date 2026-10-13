//! Deterministic provider for tests and offline runs (SPEC §13).
//!
//! CI must need no paid LLM or live network, so every acceptance check drives
//! the system through this.

use super::availability::{SendError, complete_through_gate};
use super::{
    Attempts, ChatRequest, ChatResponse, Choice, FinishReason, Message, ProbeBackoff, Provider,
    TransientFault, UpstreamGate, Usage,
};
use crate::{Error, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How a rule replies: fixed text, or text computed from the prompt (so a
/// test can cite a record ID that only exists at run time).
#[derive(Clone)]
pub enum Reply {
    Text(String),
    Dynamic(Arc<dyn Fn(&str) -> String + Send + Sync>),
}

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(t) => f.debug_tuple("Text").field(t).finish(),
            Self::Dynamic(_) => f.write_str("Dynamic(..)"),
        }
    }
}

/// A scripted reply: when the rendered prompt contains `when`, reply `reply`.
#[derive(Debug, Clone)]
pub struct MockRule {
    pub when: String,
    pub reply: Reply,
}

impl MockRule {
    pub fn new(when: impl Into<String>, reply: impl Into<String>) -> Self {
        Self {
            when: when.into(),
            reply: Reply::Text(reply.into()),
        }
    }
}

/// A provider that replies from a script instead of calling a model.
#[derive(Debug, Clone)]
pub struct MockProvider {
    name: String,
    model: String,
    rules: Arc<Mutex<Vec<MockRule>>>,
    /// Replies used in order when no rule matches, with a truncation flag.
    queue: Arc<Mutex<Vec<(String, bool)>>>,
    default_reply: Arc<Mutex<Option<String>>>,
    calls: Arc<AtomicU32>,
    /// Prompts received, for asserting what a judge was actually shown.
    seen: Arc<Mutex<Vec<ChatRequest>>>,
    /// Refuse the first N calls permanently, to exercise terminal failure.
    fail_first: Arc<AtomicU32>,
    /// Artificial latency per call, so tests can observe a run mid-flight.
    delay: Arc<Mutex<Option<std::time::Duration>>>,
    /// The stand-in upstream's availability: while `false`, every send is
    /// a transient failure, exactly like an upstream outage.
    available: Arc<AtomicBool>,
    /// Transient failures handed out so far.
    transient_failures: Arc<AtomicU32>,
    /// This mock's own availability gate (clones share it).
    gate: Arc<UpstreamGate>,
    /// Probe spacing while unavailable; short by default so outage tests
    /// run in milliseconds.
    probe_backoff: ProbeBackoff,
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl MockProvider {
    pub fn new() -> Self {
        Self {
            name: "mock".into(),
            model: "mock-model".into(),
            rules: Arc::new(Mutex::new(Vec::new())),
            queue: Arc::new(Mutex::new(Vec::new())),
            default_reply: Arc::new(Mutex::new(None)),
            calls: Arc::new(AtomicU32::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
            fail_first: Arc::new(AtomicU32::new(0)),
            delay: Arc::new(Mutex::new(None)),
            available: Arc::new(AtomicBool::new(true)),
            transient_failures: Arc::new(AtomicU32::new(0)),
            gate: Arc::new(UpstreamGate::new()),
            probe_backoff: ProbeBackoff::new(Duration::from_millis(5), Duration::from_millis(50)),
        }
    }

    /// Reply with `reply` whenever the prompt contains `when`.
    pub fn rule(self, when: impl Into<String>, reply: impl Into<String>) -> Self {
        self.rules.lock().unwrap().push(MockRule::new(when, reply));
        self
    }

    /// Append another provider's rules, queue, and default to this one's
    /// shared script, so rules can be added after the provider was handed
    /// out.
    pub fn merge_rules(self, other: MockProvider) -> Self {
        let rules: Vec<MockRule> = other.rules.lock().unwrap().clone();
        self.rules.lock().unwrap().extend(rules);
        let queue: Vec<(String, bool)> = other.queue.lock().unwrap().clone();
        self.queue.lock().unwrap().extend(queue);
        if let Some(d) = other.default_reply.lock().unwrap().clone() {
            *self.default_reply.lock().unwrap() = Some(d);
        }
        self
    }

    /// Reply with `f(prompt)` whenever the prompt contains `when`.
    pub fn rule_with(
        self,
        when: impl Into<String>,
        f: impl Fn(&str) -> String + Send + Sync + 'static,
    ) -> Self {
        self.rules.lock().unwrap().push(MockRule {
            when: when.into(),
            reply: Reply::Dynamic(Arc::new(f)),
        });
        self
    }

    /// Queue replies to be returned in order when no rule matches.
    pub fn push_reply(self, reply: impl Into<String>) -> Self {
        self.queue.lock().unwrap().push((reply.into(), false));
        self
    }

    /// Queue a reply reported as cut off by the token limit.
    pub fn push_truncated_reply(self, reply: impl Into<String>) -> Self {
        self.queue.lock().unwrap().push((reply.into(), true));
        self
    }

    /// Sleep this long before every reply.
    pub fn with_delay(self, delay: std::time::Duration) -> Self {
        *self.delay.lock().unwrap() = Some(delay);
        self
    }

    /// Reply used when nothing else matches.
    pub fn default_reply(self, reply: impl Into<String>) -> Self {
        *self.default_reply.lock().unwrap() = Some(reply.into());
        self
    }

    /// Fail the next `n` calls with a transient error.
    pub fn fail_first(self, n: u32) -> Self {
        self.fail_first.store(n, Ordering::SeqCst);
        self
    }

    /// Probe spacing while the stand-in upstream is unavailable.
    pub fn with_probe_backoff(mut self, backoff: ProbeBackoff) -> Self {
        self.probe_backoff = backoff;
        self
    }

    /// Flip the stand-in upstream's availability. While unavailable every
    /// send fails transiently (a 503), so callers wait exactly as they
    /// would for a real outage; flipping it back lets the next probe
    /// through.
    pub fn set_available(&self, available: bool) {
        self.available.store(available, Ordering::SeqCst);
    }

    /// Transient failures handed out while unavailable.
    pub fn transient_failures(&self) -> u32 {
        self.transient_failures.load(Ordering::SeqCst)
    }

    /// Calls the stand-in upstream actually served (replies and permanent
    /// refusals); the same count as [`Self::call_count`].
    pub fn served_count(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn call_count(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    /// Every request this provider received.
    pub fn seen_requests(&self) -> Vec<ChatRequest> {
        self.seen.lock().unwrap().clone()
    }

    /// The rendered text of every prompt received.
    pub fn seen_prompts(&self) -> Vec<String> {
        self.seen_requests().iter().map(|r| r.rendered()).collect()
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
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
        complete_through_gate(&self.gate, None, &self.probe_backoff, attempts, || {
            self.send_once(&request)
        })
        .await
    }

    fn upstream_gate(&self) -> Option<Arc<UpstreamGate>> {
        Some(Arc::clone(&self.gate))
    }
}

impl MockProvider {
    /// One send to the stand-in upstream.
    async fn send_once(
        &self,
        request: &ChatRequest,
    ) -> std::result::Result<ChatResponse, SendError> {
        if !self.available.load(Ordering::SeqCst) {
            self.transient_failures.fetch_add(1, Ordering::SeqCst);
            return Err(SendError::Transient(TransientFault::status(
                503,
                None,
                "mock upstream unavailable",
            )));
        }
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(request.clone());

        let delay = *self.delay.lock().unwrap();
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }

        if self.fail_first.load(Ordering::SeqCst) > n {
            return Err(SendError::Permanent(Error::Provider(format!(
                "mock provider refused call {}",
                n + 1
            ))));
        }

        let prompt = request.rendered();
        let reply = {
            let rules = self.rules.lock().unwrap();
            rules
                .iter()
                .find(|r| prompt.contains(&r.when))
                .map(|r| match &r.reply {
                    Reply::Text(t) => (t.clone(), false),
                    Reply::Dynamic(f) => (f(&prompt), false),
                })
        };

        let (reply, truncated) = match reply {
            Some(r) => r,
            None => {
                let mut queue = self.queue.lock().unwrap();
                if queue.is_empty() {
                    let fallback = self.default_reply.lock().unwrap().clone().ok_or_else(|| {
                        SendError::Permanent(Error::Provider(format!(
                            "mock provider has no reply for prompt:\n{}",
                            &prompt[..prompt.len().min(400)]
                        )))
                    })?;
                    (fallback, false)
                } else {
                    queue.remove(0)
                }
            }
        };

        // Rough token accounting: enough for budget tests to be meaningful.
        let prompt_tokens = (prompt.len() / 4) as u64;
        let completion_tokens = (reply.len() / 4) as u64;

        Ok(ChatResponse {
            id: format!("mock-{}", n),
            model: request.model.clone(),
            choices: vec![Choice {
                index: 0,
                message: Message::assistant(reply),
                finish_reason: if truncated {
                    FinishReason::Length
                } else {
                    FinishReason::Stop
                },
            }],
            usage: Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
            // A mock reports no cost, exercising the unknown-cost path.
            cost_usd: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rules_match_on_prompt_content() {
        let p = MockProvider::new()
            .rule("compare", "better: 1")
            .default_reply("fallback");

        let r = p
            .complete(ChatRequest::new(
                "m",
                vec![Message::user("please compare these")],
            ))
            .await
            .unwrap();
        assert_eq!(r.text(), "better: 1");

        let r = p
            .complete(ChatRequest::new("m", vec![Message::user("something else")]))
            .await
            .unwrap();
        assert_eq!(r.text(), "fallback");
        assert_eq!(p.call_count(), 2);
    }

    #[tokio::test]
    async fn permanent_refusals_are_surfaced_at_once() {
        let p = MockProvider::new().fail_first(1).default_reply("ok");
        assert!(p.complete(ChatRequest::new("m", vec![])).await.is_err());
        assert!(p.complete(ChatRequest::new("m", vec![])).await.is_ok());
    }

    #[tokio::test]
    async fn an_unavailable_upstream_is_waited_out_not_failed() {
        let p = MockProvider::new().default_reply("ok");
        p.set_available(false);
        let waiter = tokio::spawn({
            let p = p.clone();
            async move { p.complete(ChatRequest::new("m", vec![])).await }
        });
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!waiter.is_finished(), "the call waits for recovery");
        assert!(p.transient_failures() >= 1);
        p.set_available(true);
        let reply = waiter.await.unwrap().unwrap();
        assert_eq!(reply.text(), "ok");
        assert_eq!(p.served_count(), 1, "one served call, however many probes");
    }
}
