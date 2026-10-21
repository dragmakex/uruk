//! Unlimited research clocks must never exhaust a run's elapsed-time budget.
use time::{Duration, OffsetDateTime};
use uruk::{records::Budget, store::RunTiming};

#[test]
fn default_budget_has_no_time_limit() {
    assert_eq!(Budget::default().wall_clock_secs, 0);
}

#[test]
fn unlimited_clock_never_expires_after_start_or_provider_recovery() {
    let start = OffsetDateTime::UNIX_EPOCH;
    let mut timing = RunTiming {
        created_at: start,
        started_at: Some(start),
        paused_secs: 0,
        paused_since: None,
        pause_note: None,
    };
    assert_eq!(timing.deadline(0), None);
    assert!(!timing.deadline_passed(0, start + Duration::days(365)));
    timing.paused_secs = 120;
    assert_eq!(timing.deadline(0), None);
    assert!(!timing.deadline_passed(0, start + Duration::days(365)));
    // Explicit finite budgets still work, including settled provider waits.
    assert!(!timing.deadline_passed(3600, start + Duration::seconds(3719)));
    assert!(timing.deadline_passed(3600, start + Duration::seconds(3720)));
}
