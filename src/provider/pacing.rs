//! Request-start pacing for rate-limited upstreams (SPEC §9.1).
//!
//! A [`StartPacer`] spaces out request *starts*: one start per interval,
//! measured on the monotonic Tokio clock. It never serializes whole
//! requests — a prior request may still be in flight when the next paced
//! start fires — and it never holds a lock across an `.await`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;
use tokio::time::Instant;

/// Paces request starts to at most one per interval.
///
/// Each [`pace`](Self::pace) call reserves the next free start slot under a
/// brief lock, then sleeps without the lock until that slot arrives. Waiters
/// therefore hold distinct slots in arrival order: concurrent workers start
/// one interval apart instead of racing one shared deadline.
///
/// A caller-supplied floor (e.g. `Retry-After`) raises the whole schedule,
/// including waiters already sleeping on a reserved slot: on waking they
/// trade a slot inside the backoff window for a fresh one past it.
///
/// Cancellation is safe but conservative: a waiter dropped mid-sleep has
/// already reserved its slot, so that slot goes unused. The schedule itself
/// is never blocked or corrupted by cancellation.
#[derive(Debug)]
pub struct StartPacer {
    schedule: Mutex<Schedule>,
}

#[derive(Debug)]
struct Schedule {
    /// Current spacing between starts: the strictest (largest) interval any
    /// handle has configured for this upstream (see [`StartPacer::shared`]).
    interval: Duration,
    /// When the most recently reserved slot starts. `None` until the first
    /// reservation, so startup never waits; a last start further back than
    /// `interval` admits one immediate start (idle time earns no burst
    /// credit beyond that).
    last_start: Option<Instant>,
    /// Latest upstream-imposed earliest start (e.g. `Retry-After`). Binds
    /// every waiter of this pacer, including those already mid-sleep.
    floor: Option<Instant>,
}

impl Schedule {
    /// Take the next free start slot — one `interval` past the last, and no
    /// earlier than now or the floor.
    fn take_slot(&mut self) -> Instant {
        let now = Instant::now();
        let mut slot = match self.last_start {
            Some(last) => (last + self.interval).max(now),
            None => now,
        };
        if let Some(floor) = self.floor
            && slot < floor
        {
            slot = floor;
        }
        self.last_start = Some(slot);
        slot
    }
}

impl StartPacer {
    pub fn new(interval: Duration) -> Self {
        Self {
            schedule: Mutex::new(Schedule {
                interval,
                last_start: None,
                floor: None,
            }),
        }
    }

    /// One shared pacer per upstream identity, process-wide.
    ///
    /// Every provider handle constructed for the same `upstream` — clones and
    /// independent constructions alike — receives the same pacer, so their
    /// request starts share one schedule. When handles ask for different
    /// intervals, the strictest (largest) one wins: `interval` can only ever
    /// tighten the shared schedule, never loosen it, so the identity's starts
    /// cannot exceed the strictest configured allowance. The tightening is a
    /// process-lifetime ratchet — it keeps the strictest interval *ever*
    /// requested for the identity, even after every handle that requested it
    /// is dropped; it does not track only live handles. A tightened interval
    /// binds from the next reservation; already-reserved starts keep their
    /// slots. Entries live for the process lifetime; the registry is bounded
    /// by the number of distinct configured upstreams.
    pub fn shared(upstream: &str, interval: Duration) -> Arc<Self> {
        /// One schedule per distinct upstream identity.
        type Registry = Mutex<HashMap<String, Arc<StartPacer>>>;
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        let registry = REGISTRY.get_or_init(Mutex::default);
        // The map is never left mid-edit, so a poisoned lock (a panic on
        // another thread) recovers instead of cascading. The tighten happens
        // under the registry lock so no reservation can slip between a
        // pacer's creation and a stricter caller's adjustment.
        let mut map = registry.lock().unwrap_or_else(PoisonError::into_inner);
        let pacer = Arc::clone(
            map.entry(upstream.to_ascii_lowercase())
                .or_insert_with(|| Arc::new(Self::new(interval))),
        );
        {
            let mut schedule = pacer.lock_schedule();
            if schedule.interval < interval {
                schedule.interval = interval;
            }
        }
        pacer
    }

    /// Current start spacing: the strictest interval configured so far.
    pub fn interval(&self) -> Duration {
        self.lock_schedule().interval
    }

    /// Wait until this caller's request may start.
    ///
    /// The reserved slot is the latest of: now, one interval past the last
    /// reserved start, and the schedule's floor. A caller-supplied `floor`
    /// (e.g. from `Retry-After` or retry backoff) raises the shared floor
    /// first, so upstream backpressure binds every waiter of the same
    /// identity — already-sleeping ones re-reserve past it on waking — not
    /// just the caller that saw it. One combined sleep covers pacing and
    /// backoff; the waits overlap rather than add up.
    pub async fn pace(&self, floor: Option<Instant>) {
        let mut slot = {
            let mut schedule = self.lock_schedule();
            if let Some(floor) = floor
                && schedule.floor.is_none_or(|current| current < floor)
            {
                schedule.floor = Some(floor);
            }
            schedule.take_slot()
        };
        loop {
            tokio::time::sleep_until(slot).await;
            // While we slept, a 429 elsewhere may have raised the floor past
            // our slot; starting now would land inside the upstream's
            // declared backoff window. Trade the stale slot for a fresh one.
            let raised = {
                let mut schedule = self.lock_schedule();
                (schedule.floor.is_some_and(|floor| slot < floor)).then(|| schedule.take_slot())
            };
            match raised {
                Some(next) => slot = next,
                None => return,
            }
        }
    }

    /// The schedule is plain always-valid data, so a poisoned lock recovers
    /// instead of cascading.
    fn lock_schedule(&self) -> MutexGuard<'_, Schedule> {
        self.schedule.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
