//! End-to-end deadline propagation across the scrape pipeline.
//!
//! A [`Deadline`] is constructed once at request entry from
//! `ScrapeRequest.deadline_ms` (falling back to `request.deadline_ms_default`)
//! and threaded through every layer that may sleep, retry, or wait — limiter
//! acquire, HTTP client timeouts, the chrome navigation budget, and post-extract
//! escalation. Each layer clamps its own timeout against [`Deadline::remaining`]
//! so the absolute return time is bounded by the original deadline.

use std::time::{Duration, Instant};

/// Absolute end-of-budget instant for a single request.
///
/// Cheap to copy. Compute remaining time with [`Self::remaining`]; never
/// schedule waits longer than that value.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    absolute: Instant,
    requested_ms: u64,
    /// The caller set this deadline itself and stops waiting when it passes,
    /// as opposed to one the engine (or the SaaS in front of it) chose. Tiers
    /// that otherwise take a fresh budget of their own end by it instead.
    hard: bool,
}

impl Deadline {
    /// Build a deadline `ms` milliseconds in the future, measured from now.
    /// `ms = 0` produces an immediately-expired deadline (useful in tests).
    pub fn from_request_ms(ms: u64) -> Self {
        Self {
            absolute: Instant::now() + Duration::from_millis(ms),
            requested_ms: ms,
            hard: false,
        }
    }

    /// Like [`Self::from_request_ms`], for a deadline the caller set itself
    /// (see [`Self::is_hard`]).
    pub fn hard_ms(ms: u64) -> Self {
        Self {
            hard: true,
            ..Self::from_request_ms(ms)
        }
    }

    /// Build a deadline `d` from now.
    pub fn now_plus(d: Duration) -> Self {
        Self {
            absolute: Instant::now() + d,
            requested_ms: d.as_millis() as u64,
            hard: false,
        }
    }

    /// `true` when the caller set this deadline and stops waiting at it,
    /// `false` when it was chosen for the caller (a configured default, or a
    /// fresh budget a tier built for itself).
    pub fn is_hard(&self) -> bool {
        self.hard
    }

    /// Time remaining until the deadline. Returns `Duration::ZERO` if expired.
    pub fn remaining(&self) -> Duration {
        self.absolute.saturating_duration_since(Instant::now())
    }

    /// `true` once the deadline has passed.
    pub fn expired(&self) -> bool {
        Instant::now() >= self.absolute
    }

    /// How long ago the deadline expired. `Duration::ZERO` if not yet expired.
    ///
    /// For diagnostics only. Do NOT put this in a timeout error shown to a
    /// caller: it is read the moment `remaining()` reaches zero, so it is
    /// always a few milliseconds regardless of the budget, and it used to
    /// produce "Timeout after 1ms" on requests that were given 30 seconds.
    /// Report [`Self::requested_ms`] instead.
    pub fn overrun(&self) -> Duration {
        Instant::now().saturating_duration_since(self.absolute)
    }

    /// The absolute wall-clock instant at which this deadline expires.
    pub fn absolute(&self) -> Instant {
        self.absolute
    }

    /// The budget this deadline was built with, in milliseconds.
    ///
    /// This is what a timeout error should report. `overrun()` answers a
    /// different question — how far past the deadline we noticed — and is
    /// checked the moment `remaining()` reaches zero, so it is always a handful
    /// of milliseconds. Reporting it told a caller who was given 30s that their
    /// request "timed out after 1ms".
    pub fn requested_ms(&self) -> u64 {
        self.requested_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_caller_deadline_is_hard() {
        assert!(Deadline::hard_ms(15_000).is_hard());
        assert_eq!(Deadline::hard_ms(15_000).requested_ms(), 15_000);
        assert!(!Deadline::from_request_ms(15_000).is_hard());
        assert!(!Deadline::now_plus(Duration::from_secs(15)).is_hard());
    }

    #[test]
    fn fresh_deadline_has_remaining() {
        let d = Deadline::from_request_ms(1000);
        assert!(d.remaining() > Duration::from_millis(900));
        assert!(!d.expired());
    }

    #[test]
    fn zero_ms_is_expired() {
        let d = Deadline::from_request_ms(0);
        assert!(d.expired());
        assert_eq!(d.remaining(), Duration::ZERO);
    }

    #[test]
    fn now_plus_matches_remaining() {
        let d = Deadline::now_plus(Duration::from_millis(500));
        assert!(d.remaining() > Duration::from_millis(400));
        assert!(d.remaining() <= Duration::from_millis(500));
    }

    /// The distinction the timeout messages depend on: once the budget is
    /// spent, `overrun()` is a couple of milliseconds while `requested_ms()`
    /// still reports what the caller asked for. Reporting the former is what
    /// told customers with a 30s budget that they "timed out after 1ms".
    #[test]
    fn requested_ms_reports_the_budget_not_the_overrun() {
        let spent = Deadline::from_request_ms(0);
        assert!(spent.expired());
        assert_eq!(spent.requested_ms(), 0);

        let d = Deadline::from_request_ms(30_000);
        assert_eq!(d.requested_ms(), 30_000);
        assert!(
            d.overrun() < Duration::from_millis(50),
            "overrun is near-zero on a live deadline, which is why it must not \
             be the number a timeout error reports"
        );
    }

    #[test]
    fn now_plus_also_carries_its_budget() {
        let d = Deadline::now_plus(Duration::from_millis(2500));
        assert_eq!(d.requested_ms(), 2500);
    }
}
