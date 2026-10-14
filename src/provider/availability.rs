//! Waiting out a transiently unavailable upstream (SPEC §9.1).
//!
//! A transient failure — any connect failure (including a connect timeout),
//! a non-timeout send or response transport error, or a 408, 425, 429, or
//! 5xx — is never the end of a completion. The
//! request waits for the upstream to recover, bounded only by the
//! caller's cancellation. A process-wide [`UpstreamGate`] per upstream
//! keeps every waiter behind one half-open probe at a time, with
//! exponential backoff capped by [`ProbeBackoff`] and any `Retry-After`
//! the upstream sends honored in full. Terminal request failures — an
//! overall completion timeout, other 4xx, malformed replies, or bad
//! configuration — fail at once and are the adapter's
//! [`crate::Error::Provider`].
//!
//! Budget: one completion is one logical model call, claimed before its
//! first send. Resends after a transient failure are audited as ledger
//! facts but claim no further call, so an outage cannot exhaust the run's
//! model-call budget (the upstream served nothing). The cost of a call
//! that reached the upstream but whose reply was lost in transport stays
//! unknown, as every cost here does.

use super::{Attempts, ChatResponse, budget_refused, pacing::StartPacer};
use crate::{Error, Result};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;
use time::OffsetDateTime;
use tokio::sync::Notify;
use tokio::time::Instant;

/// Exponential backoff between probes of an unavailable upstream: the
/// n-th consecutive failure schedules the next probe `min(base · 2ⁿ, cap)`
/// later (the first failure waits `base`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeBackoff {
    base: Duration,
    cap: Duration,
}

impl ProbeBackoff {
    /// One second, doubling to at most thirty: quick to notice a short
    /// blip, gentle on an upstream that is down for an hour.
    pub const DEFAULT: Self = Self {
        base: Duration::from_secs(1),
        cap: Duration::from_secs(30),
    };

    /// `base` for the first wait, never more than `cap` (a `cap` below
    /// `base` is raised to it).
    pub fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap: cap.max(base),
        }
    }

    /// The wait after `failures` consecutive transient failures (≥ 1).
    fn delay(&self, failures: u32) -> Duration {
        let doublings = failures.saturating_sub(1).min(32);
        self.base
            .saturating_mul(1u32 << doublings.min(31))
            .min(self.cap)
    }
}

impl Default for ProbeBackoff {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Where a transport failure happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPhase {
    /// Resolving or connecting: the upstream was not reached.
    Connect,
    /// Sending the request.
    Send,
    /// Reading the response.
    Response,
}

impl TransportPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Send => "send",
            Self::Response => "response",
        }
    }
}

/// A failure that says nothing about the request and everything about
/// the upstream's availability right now. Typed, so no policy ever parses
/// a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransientFault {
    /// The request did not complete at the transport level.
    Transport {
        phase: TransportPhase,
        detail: String,
    },
    /// The upstream answered that it cannot serve right now.
    Status {
        status: u16,
        /// Its own instruction on when to come back, if it gave one.
        retry_after: Option<Duration>,
        body: String,
    },
}

impl TransientFault {
    pub fn status(status: u16, retry_after: Option<Duration>, body: impl Into<String>) -> Self {
        Self::Status {
            status,
            retry_after,
            body: body.into(),
        }
    }

    pub fn transport(phase: TransportPhase, detail: impl Into<String>) -> Self {
        Self::Transport {
            phase,
            detail: detail.into(),
        }
    }

    /// The upstream's `Retry-After`, when the fault carried one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Status { retry_after, .. } => *retry_after,
            Self::Transport { .. } => None,
        }
    }

    /// Whether an HTTP status is transient: the upstream could not serve
    /// *now* (408, 425, 429, 5xx) rather than refusing *this* request.
    pub fn is_transient_status(status: u16) -> bool {
        matches!(status, 408 | 425 | 429) || (500..=599).contains(&status)
    }
}

impl fmt::Display for TransientFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { phase, detail } => {
                write!(f, "{} failed: {detail}", phase.as_str())
            }
            Self::Status { status, body, .. } => write!(f, "HTTP {status}: {body}"),
        }
    }
}

/// Why one send did not produce a completion.
#[derive(Debug)]
pub(super) enum SendError {
    /// Wait and send again.
    Transient(TransientFault),
    /// End this logical call without changing shared upstream availability.
    RequestTimeout(Error),
    /// Report to the caller without declaring a provider-wide outage.
    Permanent(Error),
}

/// What the gate believes about an upstream.
#[derive(Debug, Clone)]
enum Availability {
    Available,
    Unavailable(Outage),
}

#[derive(Debug, Clone)]
struct Outage {
    /// Wall-clock start, for the record.
    since: OffsetDateTime,
    /// Consecutive transient failures observed by probes.
    failures: u32,
    /// Earliest start of the next probe.
    next_probe: Instant,
    /// Epoch of the probe in flight, if one is.
    probe: Option<u64>,
    /// The failure last reported.
    last_fault: String,
}

#[derive(Debug)]
struct State {
    availability: Availability,
    /// Monotonic probe identity, so a dropped probe can only release
    /// itself, never a successor.
    epoch: u64,
}

/// A snapshot of a gate, for the scheduler's bookkeeping and the UI.
#[derive(Debug, Clone)]
pub struct GateStatus {
    availability: Availability,
    now: Instant,
}

impl GateStatus {
    /// Whether requests may be sent freely.
    pub fn is_available(&self) -> bool {
        matches!(self.availability, Availability::Available)
    }

    /// When the outage started, while one is on.
    pub fn unavailable_since(&self) -> Option<OffsetDateTime> {
        match &self.availability {
            Availability::Unavailable(o) => Some(o.since),
            Availability::Available => None,
        }
    }

    /// Consecutive transient failures of the current outage.
    pub fn failures(&self) -> u32 {
        match &self.availability {
            Availability::Unavailable(o) => o.failures,
            Availability::Available => 0,
        }
    }

    /// How long until the next probe may start; zero when it may start
    /// now; `None` when the upstream is available.
    pub fn next_probe_in(&self) -> Option<Duration> {
        match &self.availability {
            Availability::Unavailable(o) => Some(o.next_probe.saturating_duration_since(self.now)),
            Availability::Available => None,
        }
    }

    /// Whether a half-open probe is in flight right now.
    pub fn probe_in_flight(&self) -> bool {
        matches!(&self.availability, Availability::Unavailable(o) if o.probe.is_some())
    }

    /// The transient failure last reported, while an outage is on.
    pub fn last_fault(&self) -> Option<&str> {
        match &self.availability {
            Availability::Unavailable(o) => Some(&o.last_fault),
            Availability::Available => None,
        }
    }
}

/// The availability gate of one upstream.
///
/// While the upstream is believed available every caller is admitted at
/// once. After a transient failure the gate closes: callers wait, and when
/// the backoff elapses exactly one of them is admitted as the half-open
/// probe. A served probe reopens the gate for everyone; a failed probe
/// extends the backoff and the next waiter takes its turn. Dropping an
/// [`Admission`] mid-probe (cancellation) releases the probe immediately.
/// No lock is held across an await.
#[derive(Debug)]
pub struct UpstreamGate {
    state: Mutex<State>,
    changed: Notify,
}

impl Default for UpstreamGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Permission to send one request, handed out by [`UpstreamGate::admit`].
/// Hand it back with [`UpstreamGate::report_served`] or
/// [`UpstreamGate::report_transient`]; dropping it unreported releases a
/// probe so another waiter can take over.
#[derive(Debug)]
pub struct Admission<'a> {
    gate: &'a UpstreamGate,
    /// `Some` when this admission is the half-open probe.
    probe: Option<u64>,
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        let Some(epoch) = self.probe.take() else {
            return;
        };
        let mut state = self.gate.lock_state();
        if let Availability::Unavailable(outage) = &mut state.availability
            && outage.probe == Some(epoch)
        {
            outage.probe = None;
            drop(state);
            self.gate.changed.notify_waiters();
        }
    }
}

/// How long a waiter behind an in-flight probe sleeps before looking at
/// the gate again even without a wakeup.
const RECHECK: Duration = Duration::from_secs(1);

/// What a waiter does before looking again.
enum Wait {
    /// A probe is in flight: wait for it to report.
    Notified,
    /// The next probe may start at this instant (or sooner, if the gate
    /// reopens meanwhile).
    Until(Instant),
}

impl UpstreamGate {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                availability: Availability::Available,
                epoch: 0,
            }),
            changed: Notify::new(),
        }
    }

    /// One gate per upstream identity, process-wide: every handle for the
    /// same upstream, however configured, waits behind the same probe.
    /// Entries live for the process lifetime; the registry is bounded by
    /// the number of distinct configured upstreams.
    pub fn shared(upstream: &str) -> Arc<Self> {
        type Registry = Mutex<HashMap<String, Arc<UpstreamGate>>>;
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        let registry = REGISTRY.get_or_init(Mutex::default);
        let mut map = registry.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            map.entry(upstream.to_ascii_lowercase())
                .or_insert_with(|| Arc::new(Self::new())),
        )
    }

    /// The gate's current belief.
    pub fn status(&self) -> GateStatus {
        GateStatus {
            availability: self.lock_state().availability.clone(),
            now: Instant::now(),
        }
    }

    /// Wait until a request may be sent. Returns at once while the
    /// upstream is available; during an outage, returns when this caller
    /// is elected the probe. Cancellation-safe: a waiter dropped here holds
    /// nothing.
    pub async fn admit(&self) -> Admission<'_> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            let wait = {
                let mut state = self.lock_state();
                // Registered while the lock is held (see below).
                notified.as_mut().enable();
                match &mut state.availability {
                    Availability::Available => {
                        return Admission {
                            gate: self,
                            probe: None,
                        };
                    }
                    Availability::Unavailable(outage) => {
                        if outage.probe.is_some() {
                            Wait::Notified
                        } else if Instant::now() >= outage.next_probe {
                            state.epoch += 1;
                            let epoch = state.epoch;
                            if let Availability::Unavailable(outage) = &mut state.availability {
                                outage.probe = Some(epoch);
                            }
                            return Admission {
                                gate: self,
                                probe: Some(epoch),
                            };
                        } else {
                            Wait::Until(outage.next_probe)
                        }
                    }
                }
            };
            // `notify_waiters` only reaches registrations that already
            // exist, and the registration above was made under the lock,
            // so a report between the decision and the await cannot be
            // missed. The bounded sleep is belt and braces: a waiter never
            // depends on a wakeup alone to look at the gate again.
            match wait {
                Wait::Notified => {
                    tokio::select! {
                        _ = notified => {}
                        _ = tokio::time::sleep(RECHECK) => {}
                    }
                }
                Wait::Until(at) => {
                    tokio::select! {
                        _ = tokio::time::sleep_until(at) => {}
                        _ = notified => {}
                    }
                }
            }
        }
    }

    /// The upstream answered (a completion or a permanent refusal): it is
    /// available. Reopens the gate for every waiter.
    pub fn report_served(&self, mut admission: Admission<'_>) {
        admission.probe = None;
        let mut state = self.lock_state();
        let was_down = matches!(state.availability, Availability::Unavailable(_));
        state.availability = Availability::Available;
        drop(state);
        if was_down {
            tracing::info!("upstream available again");
            self.changed.notify_waiters();
        }
    }

    /// The upstream could not serve. Closes the gate (or extends the
    /// outage) and schedules the next probe: the backoff for the number
    /// of consecutive probe failures, or the upstream's own `Retry-After`
    /// when that is later. Returns when the next probe may start.
    pub fn report_transient(
        &self,
        mut admission: Admission<'_>,
        fault: &TransientFault,
        backoff: &ProbeBackoff,
    ) -> Instant {
        let probe = admission.probe.take();
        let now = Instant::now();
        let mut state = self.lock_state();
        let next = match &mut state.availability {
            Availability::Available => {
                let next = now
                    + backoff
                        .delay(1)
                        .max(fault.retry_after().unwrap_or_default());
                state.availability = Availability::Unavailable(Outage {
                    since: OffsetDateTime::now_utc(),
                    failures: 1,
                    next_probe: next,
                    probe: None,
                    last_fault: fault.to_string(),
                });
                next
            }
            Availability::Unavailable(outage) => {
                if probe.is_some() && outage.probe == probe {
                    // The probe failed: back off further.
                    outage.failures = outage.failures.saturating_add(1);
                    outage.probe = None;
                    outage.next_probe = now
                        + backoff
                            .delay(outage.failures)
                            .max(fault.retry_after().unwrap_or_default());
                } else if let Some(retry_after) = fault.retry_after() {
                    // A request that was in flight when the gate closed:
                    // no new evidence about the schedule beyond an explicit
                    // instruction.
                    outage.next_probe = outage.next_probe.max(now + retry_after);
                }
                outage.last_fault = fault.to_string();
                outage.next_probe
            }
        };
        drop(state);
        self.changed.notify_waiters();
        next
    }

    /// The state is plain always-valid data, so a poisoned lock recovers
    /// instead of cascading.
    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Drive one completion to a served answer: claim the logical model call,
/// then send through `gate` (and `pacer`, when the upstream is paced)
/// until the upstream serves it, waiting out every transient failure.
/// Ends only with a served reply, a terminal request failure, a budget
/// refusal before the first send, or the caller dropping the future.
pub(super) async fn complete_through_gate<F, Fut>(
    gate: &UpstreamGate,
    pacer: Option<&StartPacer>,
    backoff: &ProbeBackoff,
    attempts: &Attempts,
    send: F,
) -> Result<ChatResponse>
where
    F: Fn() -> Fut,
    Fut: Future<Output = std::result::Result<ChatResponse, SendError>>,
{
    // The budget is consulted once per logical call, before anything goes
    // out: a call it cannot cover is never sent.
    if attempts.try_start().await?.is_none() {
        return Err(budget_refused(attempts, None));
    }
    let mut sends = 0u32;
    loop {
        let admission = gate.admit().await;
        // Pace every send, probes included: the upstream's rate limit does
        // not pause for an outage.
        if let Some(pacer) = pacer {
            pacer.pace(None).await;
        }
        if sends > 0 {
            attempts.resend().await?;
        }
        sends += 1;
        match send().await {
            Ok(response) => {
                gate.report_served(admission);
                return Ok(response);
            }
            Err(SendError::Permanent(error)) => {
                gate.report_served(admission);
                return Err(error);
            }
            Err(SendError::RequestTimeout(error)) => {
                // One slow request is not evidence either that the upstream
                // is down or that a concurrent outage has recovered.
                return Err(error);
            }
            Err(SendError::Transient(fault)) => {
                let next = gate.report_transient(admission, &fault, backoff);
                tracing::warn!(
                    sends,
                    %fault,
                    next_probe_ms = u64::try_from(
                        next.saturating_duration_since(Instant::now()).as_millis()
                    )
                    .unwrap_or(u64::MAX),
                    "upstream unavailable; waiting for it to recover"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_base_and_stops_at_cap() {
        let b = ProbeBackoff::new(Duration::from_secs(1), Duration::from_secs(8));
        let delays: Vec<_> = (1..=6).map(|n| b.delay(n)).collect();
        assert_eq!(
            delays,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(8),
                Duration::from_secs(8),
            ]
        );
    }

    #[test]
    fn a_cap_below_the_base_is_raised_to_it() {
        let b = ProbeBackoff::new(Duration::from_secs(5), Duration::from_secs(1));
        assert_eq!(b.delay(1), Duration::from_secs(5));
        assert_eq!(b.delay(9), Duration::from_secs(5));
    }

    #[test]
    fn huge_failure_counts_do_not_overflow() {
        let b = ProbeBackoff::new(Duration::from_secs(1), Duration::from_secs(30));
        assert_eq!(b.delay(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn transient_statuses_are_the_documented_set() {
        for s in [408u16, 425, 429, 500, 502, 503, 504, 599] {
            assert!(TransientFault::is_transient_status(s), "{s}");
        }
        for s in [200u16, 400, 401, 403, 404, 409, 422] {
            assert!(!TransientFault::is_transient_status(s), "{s}");
        }
    }

    #[tokio::test]
    async fn shared_registry_hands_out_one_gate_per_upstream() {
        let a = UpstreamGate::shared("gate-test.example:443");
        let b = UpstreamGate::shared("GATE-TEST.example:443");
        let other = UpstreamGate::shared("gate-test.example:80");
        assert!(Arc::ptr_eq(&a, &b), "case does not make a second gate");
        assert!(!Arc::ptr_eq(&a, &other));
    }

    #[tokio::test(start_paused = true)]
    async fn a_served_probe_releases_every_waiter_at_once() {
        let gate = Arc::new(UpstreamGate::new());
        let backoff = ProbeBackoff::new(Duration::from_secs(1), Duration::from_secs(1));
        let first = gate.admit().await;
        gate.report_transient(first, &TransientFault::status(503, None, "down"), &backoff);

        let mut waiters = Vec::new();
        for _ in 0..3 {
            let gate = Arc::clone(&gate);
            waiters.push(tokio::spawn(async move {
                let admission = gate.admit().await;
                let was_probe = admission.probe.is_some();
                gate.report_served(admission);
                was_probe
            }));
        }
        let mut probes = 0;
        for w in waiters {
            if w.await.unwrap() {
                probes += 1;
            }
        }
        assert_eq!(probes, 1, "one probe, then open admission for the rest");
        assert!(gate.status().is_available());
    }

    #[tokio::test]
    async fn a_request_timeout_does_not_erase_a_concurrent_outage() {
        let gate = Arc::new(UpstreamGate::new());
        let attempts = Arc::new(Attempts::default());
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let completion = tokio::spawn({
            let gate = Arc::clone(&gate);
            let attempts = Arc::clone(&attempts);
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            async move {
                complete_through_gate(&gate, None, &ProbeBackoff::DEFAULT, &attempts, || {
                    let entered = Arc::clone(&entered);
                    let release = Arc::clone(&release);
                    async move {
                        entered.notify_one();
                        release.notified().await;
                        Err(SendError::RequestTimeout(Error::Provider(
                            "request timed out".into(),
                        )))
                    }
                })
                .await
            }
        });
        entered.notified().await;

        let concurrent = gate.admit().await;
        gate.report_transient(
            concurrent,
            &TransientFault::status(503, None, "down"),
            &ProbeBackoff::DEFAULT,
        );
        release.notify_one();

        assert!(matches!(completion.await.unwrap(), Err(Error::Provider(_))));
        assert!(
            !gate.status().is_available(),
            "the timeout proves nothing about the concurrent outage"
        );
    }
}
