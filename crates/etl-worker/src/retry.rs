//! Retry-with-exponential-backoff envelope (#5b.1).
//!
//! Wraps any `Future<Output = Result<T, ConnectorError>>`
//! and retries when the error is transient — server-side
//! 5xx, 429, or a transport-level failure. The classifier
//! is the single source of truth: bugs, decoder failures,
//! and misconfiguration go straight to the DLQ on the
//! first attempt because retrying them just wastes
//! upstream bandwidth.
//!
//! On terminal failure the caller writes a DLQ row;
//! this module stays storage-agnostic so it's unit-
//! testable without a Postgres container.

use std::time::Duration;

use connectors::ConnectorError;

/// Per-source retry policy. Lives in `config/sources.toml`
/// so an operator can tune a flaky source without a
/// rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryConfig {
    /// Maximum total attempts (including the first try).
    /// `1` disables retry.
    pub max_attempts: u32,
    /// Backoff duration before the second attempt.
    /// Subsequent waits double until [`Self::max_backoff`].
    pub initial_backoff: Duration,
    /// Upper bound on a single backoff sleep.
    pub max_backoff: Duration,
}

/// Outcome of [`with_retry`]. On success the caller gets
/// the value; on failure they get the last error AND
/// the total attempts so the DLQ writer can record both.
#[derive(Debug)]
pub enum RetryOutcome<T> {
    Ok(T),
    Err {
        error: ConnectorError,
        /// Includes the failing attempt — always ≥ 1.
        attempts: u32,
    },
}

/// Run `op` with retries. The operation is invoked
/// repeatedly until it succeeds, returns a non-retriable
/// error, or hits `cfg.max_attempts`.
///
/// `sleep` is injected so tests can use a deterministic
/// stand-in instead of `tokio::time::sleep`. The
/// production caller threads `tokio::time::sleep`
/// through.
pub async fn with_retry<F, Fut, S, SleepFut, T>(
    cfg: RetryConfig,
    mut op: F,
    mut sleep: S,
) -> RetryOutcome<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ConnectorError>>,
    S: FnMut(Duration) -> SleepFut,
    SleepFut: std::future::Future<Output = ()>,
{
    // Defensive clamp: the sources.toml loader already
    // rejects `max_attempts == 0`, but a hand-built
    // `RetryConfig` (tests, future callers) could pass 0
    // here and produce surprising outcomes — the loop
    // would still call `op` once but the post-call
    // `attempt >= max_attempts` check would short-circuit
    // immediately, violating the "total tries including
    // first" contract. Clamping to ≥ 1 keeps the API
    // self-consistent regardless of caller.
    let max_attempts = cfg.max_attempts.max(1);
    let mut attempt: u32 = 0;
    // Clamp the initial delay to `max_backoff` for the
    // same reason as the `max_attempts.max(1)` clamp
    // above: the sources.toml loader rejects
    // `initial_backoff > max_backoff` (via
    // `BackoffOutOfOrder`), but a hand-built RetryConfig
    // could pass it. Without this clamp the first sleep
    // would exceed the documented "double until
    // max_backoff" cap, violating the API contract.
    let mut delay = cfg.initial_backoff.min(cfg.max_backoff);
    loop {
        attempt = attempt.saturating_add(1);
        match op().await {
            Ok(t) => return RetryOutcome::Ok(t),
            Err(e) if !is_retriable(&e) => {
                return RetryOutcome::Err {
                    error: e,
                    attempts: attempt,
                };
            }
            Err(e) if attempt >= max_attempts => {
                return RetryOutcome::Err {
                    error: e,
                    attempts: attempt,
                };
            }
            Err(e) => {
                // `log_friendly` bounds the message size —
                // `ConnectorError::BadStatus`'s Display
                // includes the full upstream body, which
                // would flood the log stream when an
                // unhealthy upstream returns megabytes.
                tracing::warn!(
                    attempt,
                    backoff_secs = delay.as_secs(),
                    error_kind = dlq_error_kind(&e),
                    error = %log_friendly(&e),
                    "retriable error; backing off",
                );
                sleep(delay).await;
                // `Duration` multiplication panics on
                // overflow. The sources.toml loader caps
                // `retry_*_backoff_secs` at MAX_BACKOFF_SECS
                // so this can't fire in production, but a
                // hand-built `RetryConfig` (tests, future
                // callers) could pass a huge `max_backoff`.
                // `checked_mul` + fallback to `max_backoff`
                // makes the doubling saturating.
                delay = delay
                    .checked_mul(2)
                    .map_or(cfg.max_backoff, |d| d.min(cfg.max_backoff));
            }
        }
    }
}

/// True when the error is worth retrying. Server-side
/// failures (5xx, 429) and transport errors are
/// retriable; client-side semantic failures (decode,
/// config, invalid cursor, unsupported) are not — the
/// next attempt would produce the identical error.
#[must_use]
pub fn is_retriable(err: &ConnectorError) -> bool {
    match err {
        ConnectorError::Transport(_) => true,
        ConnectorError::BadStatus { status, .. } => *status >= 500 || *status == 429,
        ConnectorError::Decode(_)
        | ConnectorError::Config(_)
        | ConnectorError::InvalidCursor { .. }
        | ConnectorError::Unsupported(_) => false,
    }
}

/// Translate a [`ConnectorError`] into the DLQ
/// `error_kind` enum string. Kept here next to the
/// classifier so the two stay in lockstep.
#[must_use]
pub fn dlq_error_kind(err: &ConnectorError) -> &'static str {
    match err {
        ConnectorError::Transport(_) => "transport",
        ConnectorError::BadStatus { .. } => "bad_status",
        ConnectorError::Decode(_) => "decode",
        ConnectorError::Config(_) => "config",
        ConnectorError::InvalidCursor { .. } => "invalid_cursor",
        ConnectorError::Unsupported(_) => "unsupported",
    }
}

/// Bounded log-friendly form of a [`ConnectorError`].
///
/// `ConnectorError::BadStatus`'s `Display` impl is
/// `HTTP {status}: {body}` and `body` is the full
/// upstream response — multi-MB for an unhealthy
/// upstream. Logging that on every retry attempt or on
/// every terminal failure floods the log stream. This
/// helper collapses `BadStatus` to `HTTP {status}` so
/// the structured log carries just the status code; the
/// full body is preserved in the DLQ row's `payload`
/// where it's already capped at
/// `DLQ_PAYLOAD_BODY_CHAR_LIMIT`.
///
/// Other variants pass through verbatim — their Display
/// impls don't carry external payloads.
#[must_use]
pub fn log_friendly(err: &ConnectorError) -> String {
    match err {
        ConnectorError::BadStatus { status, .. } => format!("HTTP {status}"),
        other => format!("{other}"),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use connectors::SourceId;

    use super::*;

    /// Stand-in for `tokio::time::sleep` — returns
    /// immediately and records the durations the
    /// envelope asked for so the test can assert the
    /// backoff curve.
    fn record_sleeps(
        into: Rc<Cell<Vec<Duration>>>,
    ) -> impl FnMut(Duration) -> std::future::Ready<()> {
        move |d| {
            let mut v = into.take();
            v.push(d);
            into.set(v);
            std::future::ready(())
        }
    }

    #[tokio::test]
    async fn success_first_try_does_not_sleep() {
        let sleeps = Rc::new(Cell::new(Vec::new()));
        let cfg = RetryConfig {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_secs(1),
        };
        let calls = Rc::new(Cell::new(0u32));
        let outcome = with_retry(
            cfg,
            || {
                let c = calls.clone();
                async move {
                    c.set(c.get() + 1);
                    Ok::<u32, ConnectorError>(42)
                }
            },
            record_sleeps(sleeps.clone()),
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Ok(42)));
        assert_eq!(calls.get(), 1);
        assert!(sleeps.take().is_empty());
    }

    #[tokio::test]
    async fn retries_transient_500_then_succeeds() {
        let cfg = RetryConfig {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_secs(1),
        };
        let calls = Rc::new(Cell::new(0u32));
        let sleeps = Rc::new(Cell::new(Vec::new()));
        let outcome = with_retry(
            cfg,
            || {
                let c = calls.clone();
                async move {
                    let n = c.get() + 1;
                    c.set(n);
                    if n < 3 {
                        Err(ConnectorError::BadStatus {
                            status: 503,
                            body: "upstream warming up".into(),
                        })
                    } else {
                        Ok("ok")
                    }
                }
            },
            record_sleeps(sleeps.clone()),
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Ok("ok")));
        assert_eq!(calls.get(), 3);
        // Sleeps recorded between attempts 1→2 and 2→3 only;
        // no sleep after the final success.
        let durations = sleeps.take();
        assert_eq!(durations.len(), 2);
        assert_eq!(durations[0], Duration::from_millis(10));
        assert_eq!(durations[1], Duration::from_millis(20));
    }

    #[tokio::test]
    async fn backoff_doubles_until_capped() {
        let cfg = RetryConfig {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(25),
        };
        let sleeps = Rc::new(Cell::new(Vec::new()));
        let outcome = with_retry(
            cfg,
            || async {
                Err::<(), _>(ConnectorError::BadStatus {
                    status: 500,
                    body: "down".into(),
                })
            },
            record_sleeps(sleeps.clone()),
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Err { attempts: 5, .. }));
        let durations = sleeps.take();
        // 4 sleeps between 5 attempts. 10 → 20 → 25 → 25.
        assert_eq!(
            durations,
            vec![
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(25),
                Duration::from_millis(25),
            ]
        );
    }

    #[tokio::test]
    async fn non_retriable_short_circuits_to_dlq() {
        let cfg = RetryConfig {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(100),
        };
        let calls = Rc::new(Cell::new(0u32));
        let outcome = with_retry(
            cfg,
            || {
                let c = calls.clone();
                async move {
                    c.set(c.get() + 1);
                    Err::<(), _>(ConnectorError::Decode("bad shape".into()))
                }
            },
            record_sleeps(Rc::new(Cell::new(Vec::new()))),
        )
        .await;
        assert!(matches!(
            outcome,
            RetryOutcome::Err {
                attempts: 1,
                error: ConnectorError::Decode(_),
            }
        ));
        // Decode is non-retriable — single attempt, no
        // sleep, straight to the caller.
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn initial_backoff_above_max_backoff_clamped_at_start() {
        // Defensive bar against hand-built RetryConfigs:
        // even when `initial_backoff > max_backoff`, the
        // first sleep must honour the cap.
        let cfg = RetryConfig {
            max_attempts: 2,
            initial_backoff: Duration::from_secs(1000),
            max_backoff: Duration::from_millis(50),
        };
        let sleeps = Rc::new(Cell::new(Vec::new()));
        let outcome = with_retry(
            cfg,
            || async {
                Err::<(), _>(ConnectorError::BadStatus {
                    status: 503,
                    body: String::new(),
                })
            },
            record_sleeps(sleeps.clone()),
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Err { attempts: 2, .. }));
        let durations = sleeps.take();
        assert_eq!(durations, vec![Duration::from_millis(50)]);
    }

    #[tokio::test]
    async fn backoff_doubling_saturates_on_duration_overflow() {
        // `Duration::checked_mul(2)` returns None when the
        // result can't be represented. The envelope must
        // fall back to `max_backoff` instead of panicking.
        // Use a max_backoff just above the Duration::MAX/2
        // boundary so doubling overflows but the initial
        // clamp (initial.min(max_backoff)) is a no-op.
        // `checked_sub` keeps clippy's
        // `unchecked-time-subtraction` lint happy.
        let huge = Duration::MAX
            .checked_sub(Duration::from_secs(1))
            .expect("Duration::MAX - 1s is representable");
        let cfg = RetryConfig {
            max_attempts: 3,
            initial_backoff: huge,
            max_backoff: huge,
        };
        let sleeps = Rc::new(Cell::new(Vec::new()));
        let outcome = with_retry(
            cfg,
            || async {
                Err::<(), _>(ConnectorError::BadStatus {
                    status: 503,
                    body: String::new(),
                })
            },
            // Don't actually sleep — record the requested
            // duration and return.
            {
                let into = sleeps.clone();
                move |d| {
                    let mut v = into.take();
                    v.push(d);
                    into.set(v);
                    std::future::ready(())
                }
            },
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Err { attempts: 3, .. }));
        // First sleep = initial (= max_backoff). Second
        // sleep: 2 * huge overflows, `checked_mul` fails,
        // fallback path returns `max_backoff` (= huge).
        let durations = sleeps.take();
        assert_eq!(durations, vec![huge, huge]);
    }

    #[tokio::test]
    async fn zero_max_attempts_is_clamped_to_one() {
        // Defensive bar: the loader rejects 0, but a
        // hand-built RetryConfig could pass it. The
        // envelope must still honour "at least one
        // attempt" — call op once, return with
        // attempts=1 (not 0), no extra sleep.
        let cfg = RetryConfig {
            max_attempts: 0,
            initial_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(100),
        };
        let calls = Rc::new(Cell::new(0u32));
        let sleeps = Rc::new(Cell::new(Vec::new()));
        let outcome = with_retry(
            cfg,
            || {
                let c = calls.clone();
                async move {
                    c.set(c.get() + 1);
                    Err::<(), _>(ConnectorError::BadStatus {
                        status: 500,
                        body: String::new(),
                    })
                }
            },
            record_sleeps(sleeps.clone()),
        )
        .await;
        assert!(matches!(outcome, RetryOutcome::Err { attempts: 1, .. }));
        assert_eq!(calls.get(), 1);
        assert!(sleeps.take().is_empty());
    }

    #[test]
    fn classifier_treats_429_and_5xx_as_retriable() {
        assert!(is_retriable(&ConnectorError::BadStatus {
            status: 429,
            body: String::new(),
        }));
        assert!(is_retriable(&ConnectorError::BadStatus {
            status: 500,
            body: String::new(),
        }));
        assert!(is_retriable(&ConnectorError::BadStatus {
            status: 503,
            body: String::new(),
        }));
        assert!(!is_retriable(&ConnectorError::BadStatus {
            status: 404,
            body: String::new(),
        }));
        assert!(!is_retriable(&ConnectorError::BadStatus {
            status: 400,
            body: String::new(),
        }));
    }

    #[test]
    fn classifier_treats_decode_config_invalidcursor_unsupported_as_terminal() {
        assert!(!is_retriable(&ConnectorError::Decode("x".into())));
        assert!(!is_retriable(&ConnectorError::Config("x".into())));
        assert!(!is_retriable(&ConnectorError::InvalidCursor {
            connector: SourceId::DataGovTw,
            reason: "x".into(),
        }));
        assert!(!is_retriable(&ConnectorError::Unsupported("x")));
    }

    #[test]
    fn log_friendly_drops_bad_status_body() {
        // BadStatus Display is `HTTP {status}: {body}`,
        // but we don't want the body in logs — the cap
        // on DLQ rows would be moot if the same string
        // landed in `tracing` instead.
        let huge_body = "x".repeat(100_000);
        let s = log_friendly(&ConnectorError::BadStatus {
            status: 502,
            body: huge_body,
        });
        assert_eq!(s, "HTTP 502");
    }

    #[test]
    fn log_friendly_passes_other_variants_through() {
        // Other variants' Display impls don't carry
        // external payloads — pass through verbatim.
        let s = log_friendly(&ConnectorError::Decode("bad shape".into()));
        assert!(s.contains("bad shape"), "got {s:?}");
    }

    #[test]
    fn dlq_kind_mapping_is_total() {
        assert_eq!(
            dlq_error_kind(&ConnectorError::BadStatus {
                status: 500,
                body: String::new(),
            }),
            "bad_status",
        );
        assert_eq!(
            dlq_error_kind(&ConnectorError::Decode("x".into())),
            "decode"
        );
        assert_eq!(
            dlq_error_kind(&ConnectorError::Config("x".into())),
            "config"
        );
        assert_eq!(
            dlq_error_kind(&ConnectorError::InvalidCursor {
                connector: SourceId::Twse,
                reason: "x".into(),
            }),
            "invalid_cursor",
        );
        assert_eq!(
            dlq_error_kind(&ConnectorError::Unsupported("x")),
            "unsupported"
        );
    }
}
