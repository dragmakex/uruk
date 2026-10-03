//! The connector trait, shared HTTP context, per-host pacing, and retries.
//!
//! All connectors are freely accessible public APIs: HTTPS GET only, no
//! credentials beyond an optional contact email, bounded timeouts, capped
//! response sizes, and every raw response body recorded for the audit trail.

use super::types::{SearchPage, SearchQuery};
use crate::{Error, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Honest access classification of a connector's API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiAccess {
    /// Free public API, no credential of any kind.
    Open,
    /// Free public API; a contact email is requested (polite pool) or required.
    OpenWithEmail { required: bool },
}

/// A scholarly metadata connector (a freely accessible public API).
///
/// Discovery-only: full-text resolution is a local decision over the
/// discovery data (see `search::resolve_fulltext`), never a further call.
#[async_trait::async_trait]
pub trait Connector: Send + Sync + std::fmt::Debug {
    /// `"openalex" | "crossref" | "arxiv"`.
    fn name(&self) -> &'static str;
    fn access(&self) -> ApiAccess;
    /// Minimum interval between requests to this host (etiquette floor).
    fn min_interval(&self) -> Duration;
    /// Execute one ranked search.
    async fn search(&self, q: &SearchQuery, cx: &ConnectorCx) -> Result<SearchPage>;
}

/// Metadata responses are capped well below the 25 MiB full-text cap.
pub const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;

/// A raw response body, recorded before parsing so the discovery result is
/// reconstructable offline, byte for byte.
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub connector: &'static str,
    pub url: String,
    pub media_type: &'static str,
    pub body: Vec<u8>,
}

/// Per-host min-interval pacing with at most one in-flight request per host
/// (arXiv requires it; it is harmless elsewhere at MVP scale).
#[derive(Debug, Default, Clone)]
pub struct HostLimiter {
    hosts: Arc<Mutex<HashMap<String, HostState>>>,
}

/// Last-request time for one host; the mutex doubles as the single
/// in-flight slot.
type HostState = Arc<Mutex<Option<tokio::time::Instant>>>;

impl HostLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wait until `min_interval` has passed since the last request to `host`,
    /// then hold the host's slot until the guard drops.
    pub async fn acquire(&self, host: &str, min_interval: Duration) -> HostSlot {
        let entry = {
            let mut hosts = self.hosts.lock().await;
            hosts.entry(host.to_string()).or_default().clone()
        };
        let guard = entry.clone().lock_owned().await;
        if let Some(last) = *guard {
            let ready = last + min_interval;
            let now = tokio::time::Instant::now();
            if ready > now {
                tracing::debug!(
                    host,
                    wait_ms = (ready - now).as_millis() as u64,
                    "limiter wait"
                );
                tokio::time::sleep_until(ready).await;
            }
        }
        HostSlot { guard }
    }
}

/// Held across one request; stamps the host's last-request time on drop.
pub struct HostSlot {
    guard: tokio::sync::OwnedMutexGuard<Option<tokio::time::Instant>>,
}

impl Drop for HostSlot {
    fn drop(&mut self) {
        *self.guard = Some(tokio::time::Instant::now());
    }
}

/// Shared per-task context: one HTTP client, the per-host limiter, the
/// contact email, and the raw-response log.
#[derive(Debug)]
pub struct ConnectorCx {
    client: reqwest::Client,
    limiter: HostLimiter,
    pub contact_email: Option<String>,
    raw: std::sync::Mutex<Vec<RawResponse>>,
}

impl ConnectorCx {
    pub fn new(contact_email: Option<String>) -> Result<Self> {
        // Same builder discipline as ingest: bounded timeouts and an
        // identifying UA, extended with the contact email when set.
        let ua = match &contact_email {
            Some(email) => format!("uruk/{} (mailto:{email})", env!("CARGO_PKG_VERSION")),
            None => concat!("uruk/", env!("CARGO_PKG_VERSION")).to_string(),
        };
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .user_agent(ua)
            .build()
            .map_err(|e| Error::Provider(format!("cannot build HTTP client: {e}")))?;
        Ok(Self {
            client,
            limiter: HostLimiter::new(),
            contact_email,
            raw: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// GET `url`, paced per host, with bounded retries, and record the raw
    /// body under `connector` for the audit trail.
    pub async fn get_recorded(
        &self,
        connector: &'static str,
        url: &str,
        media_type: &'static str,
    ) -> Result<Vec<u8>> {
        let host = host_of(url);
        let body = self.get_with_retries(&host, url).await?;
        // A poisoned lock (panic while pushing) must not silently drop the
        // audit trail; the Vec itself is never left inconsistent.
        self.raw
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(RawResponse {
                connector,
                url: url.to_string(),
                media_type,
                body: body.clone(),
            });
        Ok(body)
    }

    /// Drain the recorded raw responses (the engine persists them as
    /// artifacts).
    pub fn take_raw(&self) -> Vec<RawResponse> {
        std::mem::take(
            &mut *self
                .raw
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// The limiter, so a caller can pace by a connector's floor explicitly.
    pub async fn pace(&self, url: &str, min_interval: Duration) -> HostSlot {
        self.limiter.acquire(&host_of(url), min_interval).await
    }

    async fn get_with_retries(&self, host: &str, url: &str) -> Result<Vec<u8>> {
        const MAX_RETRIES: u32 = 2;
        let backoff = [Duration::from_secs(1), Duration::from_secs(4)];
        let mut attempt = 0;
        loop {
            let result = self.get_once(url).await;
            match result {
                Ok(bytes) => return Ok(bytes),
                Err(GetError::Permanent(msg)) => {
                    return Err(Error::Search(format!("{host}: {msg}")));
                }
                Err(GetError::Transient { msg, retry_after }) => {
                    if attempt >= MAX_RETRIES {
                        return Err(Error::Search(format!(
                            "{host}: {msg} (after {} attempts)",
                            attempt + 1
                        )));
                    }
                    // Honor Retry-After when present, capped at 30 s.
                    let wait = retry_after
                        .map(|s| Duration::from_secs(s.min(30)))
                        .unwrap_or(backoff[attempt as usize]);
                    tracing::debug!(host, attempt, wait_s = wait.as_secs(), %msg, "retrying");
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
            }
        }
    }

    async fn get_once(&self, url: &str) -> std::result::Result<Vec<u8>, GetError> {
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| GetError::Transient {
                msg: format!("request failed: {e}"),
                retry_after: None,
            })?;

        let status = response.status();
        if status.as_u16() == 429 || status.is_server_error() {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            if retry_after.is_some() {
                tracing::warn!(%url, status = status.as_u16(), "rate limited; honoring Retry-After");
            }
            return Err(GetError::Transient {
                msg: format!("HTTP {status}"),
                retry_after,
            });
        }
        if !status.is_success() {
            return Err(GetError::Permanent(format!("HTTP {status}")));
        }
        if response.content_length().unwrap_or(0) > MAX_METADATA_BYTES as u64 {
            return Err(GetError::Permanent(format!(
                "response exceeds the {MAX_METADATA_BYTES}-byte metadata cap"
            )));
        }
        // Stream with the cap enforced as bytes arrive: a response without a
        // Content-Length header must not buffer unboundedly before a
        // post-hoc check.
        let mut body: Vec<u8> = Vec::new();
        loop {
            let chunk = response.chunk().await.map_err(|e| GetError::Transient {
                msg: format!("read failed: {e}"),
                retry_after: None,
            })?;
            let Some(chunk) = chunk else {
                return Ok(body);
            };
            if body.len() + chunk.len() > MAX_METADATA_BYTES {
                return Err(GetError::Permanent(format!(
                    "response exceeds the {MAX_METADATA_BYTES}-byte metadata cap"
                )));
            }
            body.extend_from_slice(&chunk);
        }
    }
}

enum GetError {
    Transient {
        msg: String,
        retry_after: Option<u64>,
    },
    Permanent(String),
}

/// Host portion of a URL, for per-host pacing.
fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

/// Percent-encode a query value (RFC 3986 unreserved characters kept).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoding_covers_spaces_and_reserved_chars() {
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("safe-._~09AZaz"), "safe-._~09AZaz");
    }

    /// The arXiv floor (3 s between requests, one in flight) is verified on
    /// the limiter under paused time, not by wall-clock integration sleeps.
    #[tokio::test(start_paused = true)]
    async fn limiter_enforces_the_min_interval_per_host() {
        let limiter = HostLimiter::new();
        let interval = Duration::from_secs(3);

        let t0 = tokio::time::Instant::now();
        drop(limiter.acquire("export.arxiv.org", interval).await);
        assert_eq!(t0.elapsed(), Duration::ZERO, "first request is immediate");

        drop(limiter.acquire("export.arxiv.org", interval).await);
        assert!(
            t0.elapsed() >= interval,
            "second request must wait the 3 s floor, waited {:?}",
            t0.elapsed()
        );

        // A different host is not paced by arXiv's floor.
        let t1 = tokio::time::Instant::now();
        drop(
            limiter
                .acquire("api.openalex.org", Duration::from_millis(250))
                .await,
        );
        assert_eq!(t1.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn limiter_allows_one_in_flight_request_per_host() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let limiter = HostLimiter::new();
        let slot = limiter
            .acquire("export.arxiv.org", Duration::from_secs(3))
            .await;

        let acquired = Arc::new(AtomicBool::new(false));
        let second = {
            let (limiter, acquired) = (limiter.clone(), acquired.clone());
            tokio::spawn(async move {
                let _slot = limiter
                    .acquire("export.arxiv.org", Duration::from_secs(3))
                    .await;
                acquired.store(true, Ordering::SeqCst);
            })
        };

        // While the first slot is held, the second request must be parked on
        // the host mutex even as paused time advances past the interval.
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert!(
            !acquired.load(Ordering::SeqCst),
            "a second request proceeded while the first was in flight"
        );

        drop(slot);
        second.await.unwrap();
        assert!(acquired.load(Ordering::SeqCst));
    }
}
