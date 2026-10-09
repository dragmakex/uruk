//! OpenAI-compatible HTTP provider (SPEC §9.1).
//!
//! Speaks `POST {base_url}/chat/completions` with the wire types in the parent
//! module, which is what OpenAI, Anthropic's compatibility endpoint, Together,
//! Groq, vLLM, llama.cpp, Ollama and OpenRouter accept. Bounded retries and
//! timeouts live here; cancellation is the caller's race (see
//! [`crate::agents::AgentContext::complete`]).

use super::pacing::StartPacer;
use super::{ChatRequest, ChatResponse, Provider};
use crate::{Error, Result};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Environment variables that configure the provider.
pub const ENV_URL: &str = "URUK_PROVIDER_URL";
pub const ENV_KEY: &str = "URUK_API_KEY";
pub const ENV_MODEL: &str = "URUK_MODEL";
/// Optional `reasoning_effort` sent with every request (`none`, `low`, `medium`,
/// `high`). Reasoning models spend most of their time thinking otherwise.
pub const ENV_REASONING: &str = "URUK_REASONING_EFFORT";
/// Optional request-start pacing in requests per minute. Unset keeps the
/// per-endpoint default ([`SWISSAI_DEFAULT_RPM`] for SwissAI hosts, unpaced
/// otherwise); `0` disables pacing explicitly.
pub const ENV_RPM: &str = "URUK_PROVIDER_RPM";

/// SwissAI publishes a 14 requests/minute limit on its OpenAI-compatible
/// endpoint, so providers pointed at a SwissAI host pace request starts to
/// one per 60/14 s unless [`ENV_RPM`] overrides it.
pub const SWISSAI_DEFAULT_RPM: u32 = 14;

#[derive(Debug, Clone)]
pub struct HttpProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    name: String,
    /// Normalized upstream identity, `host:effective-port`. URL spellings of
    /// one endpoint — explicit default port, userinfo, host casing — collapse
    /// to the same identity, so they share one pacer schedule.
    upstream: String,
    max_retries: u32,
    retry_base: Duration,
    reasoning_effort: Option<String>,
    /// This handle's pacing target; `None` means unpaced. The shared
    /// schedule it resolves to at request time (see [`Self::pacer`]) is one
    /// per upstream identity, making the limit process-wide.
    rpm: Option<NonZeroU32>,
    /// Retry-backoff jitter source. Clones share it, which is fine: each
    /// draw is independent, so handles still avoid retrying in lockstep.
    /// Locked only for a single draw, never across an `.await`.
    jitter: Arc<Mutex<ChaCha8Rng>>,
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
            base_url,
            api_key,
            model: model.into(),
            name: format!("openai-compatible@{display_host}"),
            upstream,
            max_retries: 3,
            retry_base: Duration::from_millis(500),
            reasoning_effort: None,
            rpm: default_rpm,
            jitter: Arc::new(Mutex::new(ChaCha8Rng::seed_from_u64(rand::random()))),
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
        let mut provider = Self::new(url, key, model)?.with_reasoning_effort(effort);
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

    pub fn with_max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Base delay for exponential backoff; tests shrink it.
    pub fn with_retry_base(mut self, d: Duration) -> Self {
        self.retry_base = d;
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

    /// Seed the retry-backoff jitter; tests use this for determinism.
    pub fn with_jitter_seed(mut self, seed: u64) -> Self {
        self.jitter = Arc::new(Mutex::new(ChaCha8Rng::seed_from_u64(seed)));
        self
    }

    /// Exponential backoff for `attempt` with equal jitter: uniformly drawn
    /// from `[nominal/2, nominal]` where `nominal = retry_base * 2^attempt`.
    /// Bounded above by the un-jittered value, so the retry budget never
    /// grows; randomized below it, so concurrent workers spread out.
    fn jittered_backoff(&self, attempt: u32) -> Duration {
        let nominal = self.retry_base.saturating_mul(2u32.saturating_pow(attempt));
        let factor = {
            // RNG state is valid after any panic; recover a poisoned lock
            // rather than cascading the panic into every later retry.
            let mut rng = self
                .jitter
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            rng.gen_range(0.5..=1.0)
        };
        nominal.mul_f64(factor)
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
        // Earliest permitted start of the next attempt (retry backoff and
        // any honored Retry-After). The pacer folds it into its slot, so the
        // waits run concurrently — never back to back.
        let mut floor: Option<tokio::time::Instant> = None;
        let pacer = self.pacer();

        loop {
            // Pace every outbound attempt, the first as much as any retry.
            match &pacer {
                Some(pacer) => pacer.pace(floor).await,
                None => {
                    if let Some(floor) = floor {
                        tokio::time::sleep_until(floor).await;
                    }
                }
            }

            let mut req = self.client.post(&url).json(&body);
            if let Some(key) = &self.api_key {
                req = req.bearer_auth(key);
            }

            let (transient_error, retry_after) = match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let retry_after = (status.as_u16() == 429)
                        .then(|| {
                            resp.headers()
                                .get(reqwest::header::RETRY_AFTER)?
                                .to_str()
                                .ok()
                                .and_then(|v| parse_retry_after(v, time::OffsetDateTime::now_utc()))
                        })
                        .flatten();
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
                    (message, retry_after)
                }
                Err(e) => {
                    let transient = e.is_connect() || e.is_timeout() || e.is_request();
                    if !transient {
                        return Err(Error::Provider(format!("request failed: {e}")));
                    }
                    (format!("request failed: {e}"), None)
                }
            };

            if attempt >= self.max_retries {
                return Err(Error::Provider(format!(
                    "{transient_error} (after {} attempt(s))",
                    attempt + 1
                )));
            }
            // One delay serves both signals: the larger of jittered backoff
            // and Retry-After, then expressed as a floor so a pacer wait can
            // subsume it rather than add to it.
            let backoff = self.jittered_backoff(attempt);
            let delay = retry_after.map_or(backoff, |ra| ra.max(backoff));
            floor = Some(tokio::time::Instant::now() + delay);
            tracing::warn!(
                attempt,
                %transient_error,
                delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                "provider call failed; retrying"
            );
            attempt += 1;
        }
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

    fn provider() -> HttpProvider {
        HttpProvider::new("http://localhost:9/v1", None, "m").unwrap()
    }

    #[test]
    fn jittered_backoff_stays_between_half_and_full_nominal() {
        let p = provider().with_jitter_seed(7);
        for attempt in 0..4u32 {
            let nominal = Duration::from_millis(500) * 2u32.pow(attempt);
            let jittered = p.jittered_backoff(attempt);
            assert!(
                jittered >= nominal / 2,
                "attempt {attempt}: {jittered:?} below {:?}",
                nominal / 2
            );
            assert!(
                jittered <= nominal,
                "attempt {attempt}: {jittered:?} above {nominal:?}"
            );
        }
    }

    #[test]
    fn same_seed_gives_a_deterministic_backoff_sequence() {
        let a = provider().with_jitter_seed(42);
        let b = provider().with_jitter_seed(42);
        let seq_a: Vec<_> = (0..4).map(|i| a.jittered_backoff(i)).collect();
        let seq_b: Vec<_> = (0..4).map(|i| b.jittered_backoff(i)).collect();
        assert_eq!(seq_a, seq_b);
    }

    #[test]
    fn different_seeds_do_not_back_off_in_lockstep() {
        let a = provider().with_jitter_seed(1);
        let b = provider().with_jitter_seed(2);
        let seq_a: Vec<_> = (0..4).map(|i| a.jittered_backoff(i)).collect();
        let seq_b: Vec<_> = (0..4).map(|i| b.jittered_backoff(i)).collect();
        assert_ne!(seq_a, seq_b, "workers must not retry in lockstep");
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
