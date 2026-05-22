//! Rate-limit service (#4.7).
//!
//! Fixed-window per-key counters backing the three-layer
//! rate limit the #4.7 definition of done specifies:
//!
//!   * Layer 1 — IP-based (60 req/min default), applied by the
//!     gateway's outermost middleware. Keyed by client IP.
//!   * Layer 2 — API-key per tier (free / pro / enterprise),
//!     applied by middleware downstream of session / api-key
//!     auth. Keyed by `key:<uuid>` with the per-tier RPM
//!     selected via [`tier_rpm`].
//!   * Layer 3 — Tool-specific stricter limit for
//!     `query_rows`. Applied as an explicit guard INSIDE the
//!     tool implementation (the MCP transport doesn't expose
//!     tool names at the HTTP middleware boundary). Keyed by
//!     `tool:query_rows:<uuid>`.
//!
//! Eventual production backend is `DragonflyDB` (Redis-
//! compatible) for shared counter state across multi-instance
//! gateways; that impl is NOT in this PR. Until it lands, the
//! [`PgRateLimiter`] impl below is the default production
//! path AND the "small deploy without Redis" fallback the
//! spec calls out. An in-memory impl is exported for tests
//! AND used as the personal-mode fallback so the service
//! surface can be exercised without a real DB.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use storage::{CounterTick, RateLimitRepo};

use crate::error::AuthError;

/// Width of the fixed window in seconds. 60 — one minute — is
/// the unit the #4.7 spec's "60/min" speaks in. Hard-coded
/// here (not parameterised) because every layer of the
/// surface uses the same window; a future per-layer window
/// would need a per-call argument anyway.
pub const WINDOW_SECONDS: i64 = 60;

/// Layer 1 default: 60 req/min per IP. Documented in the
/// #4.7 spec.
pub const DEFAULT_IP_RPM: u32 = 60;

/// Layer 3 default: tighter limit for `query_rows` because each
/// invocation can run an expensive Parquet scan. 20/min keeps
/// the limit per-key (multiplied by the user's normal tier
/// limit) so a `pro` user with 600/min overall can still issue
/// up to 20 of those against `query_rows`. Hard-coded for now;
/// layer 3 itself is deferred (see module docs) so there's
/// no override config to wire yet — when the per-caller guard
/// lands it'll grow an env knob alongside.
pub const DEFAULT_QUERY_ROWS_RPM: u32 = 20;

/// Outcome of a single rate-limit check. Carries the data the
/// gateway needs to build the canonical `X-RateLimit-*`
/// response headers AND to decide whether to 429.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitOutcome {
    /// `true` if the request is allowed under the limit. The
    /// caller still inserts the standard headers on the
    /// allowed path so clients can pace themselves.
    pub allowed: bool,
    /// Configured RPM for this key — emitted as
    /// `X-RateLimit-Limit`.
    pub limit: u32,
    /// Remaining requests in the current window (saturating at
    /// 0 when over). Emitted as `X-RateLimit-Remaining`.
    pub remaining: u32,
    /// Seconds until the current window resets — used for both
    /// `Retry-After` (on 429) and `X-RateLimit-Reset`
    /// (delta-seconds form). Always non-negative.
    pub retry_after_seconds: u64,
}

/// Caller-agnostic rate-limit surface. The two impls
/// ([`PgRateLimiter`] and [`InMemoryRateLimiter`]) compose the
/// fixed-window check against their backing store; a future
/// `DragonflyDB` impl plugs into the same trait without
/// touching the middleware code.
#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// Check whether the request keyed by `key` is allowed
    /// against `max_rpm`. Side effect: the counter for the
    /// current window is bumped regardless of the outcome —
    /// requests over the limit still count against future
    /// burst-protection windows so an attacker can't keep
    /// flooding by counting the failures as "free".
    async fn check(
        &self,
        key: &str,
        max_rpm: u32,
        now: DateTime<Utc>,
    ) -> Result<RateLimitOutcome, AuthError>;
}

/// `PostgreSQL`-backed [`RateLimiter`]. Used as the default
/// production path until the `DragonflyDB`-backed impl lands;
/// serves as the documented fallback for small deploys
/// without Redis.
pub struct PgRateLimiter {
    repo: Arc<dyn RateLimitRepo>,
}

impl PgRateLimiter {
    #[must_use]
    pub fn new(repo: Arc<dyn RateLimitRepo>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl RateLimiter for PgRateLimiter {
    async fn check(
        &self,
        key: &str,
        max_rpm: u32,
        now: DateTime<Utc>,
    ) -> Result<RateLimitOutcome, AuthError> {
        let window_start = floor_to_window(now);
        let tick = self
            .repo
            .check_and_increment(key, window_start)
            .await
            .map_err(AuthError::Storage)?;
        Ok(outcome_from_tick(tick, max_rpm, now))
    }
}

/// In-memory [`RateLimiter`] used by tests AND as the
/// personal-mode fallback when no `Storage` is available.
///
/// `std::sync::Mutex` is the deliberate choice over
/// `tokio::sync::Mutex` for two reasons: (1) the critical
/// section is two `HashMap` operations with no `.await` —
/// blocking on contention is microseconds, not milliseconds;
/// (2) `tokio::sync::Mutex` carries async-runtime baggage
/// (futures, polling) that's overkill for a path the spec
/// itself describes as best-effort. Under sustained heavy
/// contention switch to `DashMap` or `parking_lot::Mutex`;
/// that's a follow-up if profiling actually shows the
/// blocking-worker concern materialise.
///
/// The lock is also poison-tolerant: a panic in one task
/// no longer permanently breaks rate limiting in-process —
/// the next call recovers the inner data via
/// `PoisonError::into_inner` and continues. We don't expect
/// any panic on the critical-section path (it's two `HashMap`
/// ops), so this is a belt-and-suspenders defence rather than
/// a known failure mode.
#[derive(Default)]
pub struct InMemoryRateLimiter {
    inner: Mutex<HashMap<String, CounterTick>>,
}

#[async_trait]
impl RateLimiter for InMemoryRateLimiter {
    async fn check(
        &self,
        key: &str,
        max_rpm: u32,
        now: DateTime<Utc>,
    ) -> Result<RateLimitOutcome, AuthError> {
        let window_start = floor_to_window(now);
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tick = match inner.get(key).copied() {
            Some(existing) if existing.window_start >= window_start => CounterTick {
                count: existing.count.saturating_add(1),
                window_start: existing.window_start,
            },
            // Either no row or a strictly-older window — reset.
            _ => CounterTick {
                count: 1,
                window_start,
            },
        };
        inner.insert(key.to_owned(), tick);
        Ok(outcome_from_tick(tick, max_rpm, now))
    }
}

/// Map an `mcp_api_keys.rate_limit_tier` value to RPM. Tiers
/// outside the allowed set return the conservative `free`
/// default — defensive against a future tier rename that
/// hasn't been mirrored here.
#[must_use]
pub fn tier_rpm(tier: &str) -> u32 {
    match tier {
        "pro" => 600,
        "enterprise" => 6_000,
        // `free` (and anything unrecognised) → 60/min.
        _ => DEFAULT_IP_RPM,
    }
}

/// Round `now` down to the start of the current minute-window.
/// All three layers share this granularity, so callers don't
/// have to know about it.
fn floor_to_window(now: DateTime<Utc>) -> DateTime<Utc> {
    let secs = now.timestamp();
    let floor = secs - secs.rem_euclid(WINDOW_SECONDS);
    DateTime::<Utc>::from_timestamp(floor, 0).expect("floor of valid Utc must round-trip")
}

fn outcome_from_tick(tick: CounterTick, max_rpm: u32, now: DateTime<Utc>) -> RateLimitOutcome {
    let max_i32 = i32::try_from(max_rpm).unwrap_or(i32::MAX);
    let allowed = tick.count <= max_i32;
    let remaining = if allowed {
        u32::try_from(max_i32.saturating_sub(tick.count).max(0)).unwrap_or(0)
    } else {
        0
    };
    let window_end = tick.window_start + ChronoDuration::seconds(WINDOW_SECONDS);
    let retry_after_seconds = u64::try_from((window_end - now).num_seconds().max(0)).unwrap_or(0);
    RateLimitOutcome {
        allowed,
        limit: max_rpm,
        remaining,
        retry_after_seconds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn floor_rounds_down_to_minute_boundary() {
        assert_eq!(floor_to_window(at(0)), at(0));
        assert_eq!(floor_to_window(at(59)), at(0));
        assert_eq!(floor_to_window(at(60)), at(60));
        assert_eq!(floor_to_window(at(119)), at(60));
        assert_eq!(floor_to_window(at(120)), at(120));
    }

    #[test]
    fn tier_rpm_known_values() {
        assert_eq!(tier_rpm("free"), 60);
        assert_eq!(tier_rpm("pro"), 600);
        assert_eq!(tier_rpm("enterprise"), 6_000);
    }

    #[test]
    fn tier_rpm_unknown_defaults_to_free() {
        assert_eq!(tier_rpm("godmode"), 60);
        assert_eq!(tier_rpm(""), 60);
    }

    #[test]
    fn outcome_under_limit_marks_allowed() {
        let tick = CounterTick {
            count: 5,
            window_start: at(60),
        };
        let outcome = outcome_from_tick(tick, 10, at(75));
        assert!(outcome.allowed);
        assert_eq!(outcome.limit, 10);
        assert_eq!(outcome.remaining, 5);
        assert_eq!(outcome.retry_after_seconds, 45);
    }

    #[test]
    fn outcome_at_limit_is_allowed_with_zero_remaining() {
        let tick = CounterTick {
            count: 10,
            window_start: at(60),
        };
        let outcome = outcome_from_tick(tick, 10, at(75));
        assert!(outcome.allowed);
        assert_eq!(outcome.remaining, 0);
    }

    #[test]
    fn outcome_over_limit_is_rejected_with_zero_remaining() {
        let tick = CounterTick {
            count: 11,
            window_start: at(60),
        };
        let outcome = outcome_from_tick(tick, 10, at(75));
        assert!(!outcome.allowed);
        assert_eq!(outcome.remaining, 0);
    }

    #[test]
    fn outcome_retry_after_floors_at_zero_when_window_already_past() {
        // `now` is past `window_end` (skewed clocks or rounding).
        let tick = CounterTick {
            count: 1,
            window_start: at(60),
        };
        let outcome = outcome_from_tick(tick, 10, at(200));
        assert_eq!(outcome.retry_after_seconds, 0);
    }

    #[tokio::test]
    async fn in_memory_resets_counter_on_new_window() {
        let limiter = InMemoryRateLimiter::default();
        // 5 calls inside the same minute consume 5 of the limit.
        for i in 0..5 {
            let _ = limiter.check("ip:test", 10, at(i)).await.unwrap();
        }
        // Sixth call is still inside the same window — count = 6.
        let outcome = limiter.check("ip:test", 10, at(59)).await.unwrap();
        assert_eq!(outcome.remaining, 4);
        // Cross the minute boundary — counter resets to 1.
        let outcome = limiter.check("ip:test", 10, at(60)).await.unwrap();
        assert_eq!(outcome.remaining, 9);
    }

    #[tokio::test]
    async fn in_memory_rejects_after_limit_exceeded() {
        let limiter = InMemoryRateLimiter::default();
        for _ in 0..3 {
            let _ = limiter.check("ip:noisy", 3, at(0)).await.unwrap();
        }
        // 4th call: count = 4, limit = 3 → rejected.
        let outcome = limiter.check("ip:noisy", 3, at(0)).await.unwrap();
        assert!(!outcome.allowed);
        assert_eq!(outcome.remaining, 0);
        assert!(outcome.retry_after_seconds > 0);
    }

    #[tokio::test]
    async fn in_memory_keys_are_independent() {
        let limiter = InMemoryRateLimiter::default();
        for _ in 0..5 {
            let _ = limiter.check("ip:a", 5, at(0)).await.unwrap();
        }
        // Different key starts fresh.
        let outcome = limiter.check("ip:b", 5, at(0)).await.unwrap();
        assert!(outcome.allowed);
        assert_eq!(outcome.remaining, 4);
    }
}
