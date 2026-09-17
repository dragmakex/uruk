//! Deterministic provider for tests and offline runs (SPEC §13).
//!
//! CI must need no paid LLM or live network, so every acceptance check drives
//! the system through this.

use super::{ChatRequest, ChatResponse, Choice, FinishReason, Message, Provider, Usage};
use crate::{Error, Result};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

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
    /// Fail the first N calls, to exercise bounded retry.
    fail_first: Arc<AtomicU32>,
    /// Artificial latency per call, so tests can observe a run mid-flight.
    delay: Arc<Mutex<Option<std::time::Duration>>>,
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
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(request.clone());

        let delay = *self.delay.lock().unwrap();
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }

        if self.fail_first.load(Ordering::SeqCst) > n {
            return Err(Error::Provider(format!(
                "mock transient failure on call {}",
                n + 1
            )));
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
                        Error::Provider(format!(
                            "mock provider has no reply for prompt:\n{}",
                            &prompt[..prompt.len().min(400)]
                        ))
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
            model: request.model,
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
    async fn transient_failures_are_surfaced() {
        let p = MockProvider::new().fail_first(1).default_reply("ok");
        assert!(p.complete(ChatRequest::new("m", vec![])).await.is_err());
        assert!(p.complete(ChatRequest::new("m", vec![])).await.is_ok());
    }
}
