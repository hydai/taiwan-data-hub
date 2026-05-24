//! `time_convert_timezone` MCP tool — convert a date-time from one
//! IANA timezone to another (e.g. `Asia/Taipei` → `UTC`,
//! `America/New_York`).
//!
//! Uses `chrono` for parsing + arithmetic and `chrono-tz` for the
//! IANA tzdata. Both are already in the workspace dep graph
//! (`chrono` directly, `chrono-tz` via the existing `date_tools`).

use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone};
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::json_helpers::kind_of;

pub const TOOL_NAME: &str = "time_convert_timezone";

#[derive(Debug, Default, Clone)]
pub struct TimezoneConvertTool;
impl TimezoneConvertTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for TimezoneConvertTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Convert an ISO-8601 datetime between IANA \
                          timezones (e.g. Asia/Taipei → UTC). Input \
                          formats accepted: RFC 3339 (with offset), or \
                          naive `YYYY-MM-DDTHH:MM:SS` + an explicit \
                          `from_tz`. Output is RFC 3339 in the target \
                          tz."
            .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let datetime_str = parse_string(&args, "datetime")?;
        let to_tz_name = parse_string(&args, "to_tz")?;
        let from_tz_opt = parse_optional_string(&args, "from_tz")?;

        let to_tz: chrono_tz::Tz = to_tz_name.parse().map_err(|e: chrono_tz::ParseError| {
            ToolError::InvalidArguments(format!("`to_tz` is not a valid IANA timezone name: {e}"))
        })?;

        // First try parsing as an RFC 3339 with explicit offset; if
        // that fails, fall back to a naive parse + explicit from_tz.
        // `effective_from_tz` carries the label we'll echo in the
        // response — either the explicit `from_tz` arg, or a
        // synthesized "(from offset)" / "UTC" tag derived from the
        // RFC-3339 trailer.
        let (utc_instant, effective_from_tz) = if let Ok(fixed) =
            DateTime::<FixedOffset>::parse_from_rfc3339(&datetime_str)
        {
            // If both an offset (via RFC-3339) AND `from_tz` were
            // supplied, reject — silently echoing back a `from_tz`
            // that doesn't correspond to the parsed offset would
            // mislead callers about what the tool actually did.
            // (Strict validation that the named zone's offset
            // matches the literal offset at that instant would be
            // even better, but requires resolving DST for the
            // instant — the simpler rule "don't mix the two" is
            // less surprising.)
            if from_tz_opt.is_some() {
                return Err(ToolError::InvalidArguments(
                    "`from_tz` must not be supplied when `datetime` includes its own offset \
                     — the offset already encodes the source timezone unambiguously"
                        .into(),
                ));
            }
            let label = if datetime_str.ends_with('Z') {
                "UTC".to_string()
            } else {
                "(from offset)".to_string()
            };
            (fixed.with_timezone(&chrono::Utc), label)
        } else {
            let from_tz_name = from_tz_opt.clone().ok_or_else(|| {
                ToolError::InvalidArguments(
                    "`datetime` lacks a timezone offset; supply `from_tz` to disambiguate".into(),
                )
            })?;
            let from_tz: chrono_tz::Tz =
                from_tz_name.parse().map_err(|e: chrono_tz::ParseError| {
                    ToolError::InvalidArguments(format!(
                        "`from_tz` is not a valid IANA timezone name: {e}"
                    ))
                })?;
            let naive = NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%dT%H:%M:%S")
                .or_else(|_| NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%d %H:%M:%S"))
                .map_err(|e| {
                    ToolError::InvalidArguments(format!(
                        "`datetime` could not be parsed as RFC 3339 or as `YYYY-MM-DDTHH:MM:SS`: {e}"
                    ))
                })?;
            // `from_local_datetime` returns `MappedLocalTime` to
            // describe DST ambiguities. Two cases:
            //   - SPRING-FORWARD GAP (no local representation):
            //     `.earliest()` returns `None`; we surface an
            //     InvalidArguments rather than fabricating an
            //     instant. The error message says "DST gap" so the
            //     caller knows what happened.
            //   - FALL-BACK FOLD (two local representations):
            //     `.earliest()` returns the earlier (pre-fall-back)
            //     UTC instant. Callers who need the later instant
            //     should pass an RFC-3339 string with the desired
            //     offset.
            let utc = from_tz
                .from_local_datetime(&naive)
                .earliest()
                .ok_or_else(|| {
                    ToolError::InvalidArguments(format!(
                        "datetime `{datetime_str}` does not exist in {from_tz_name} (DST gap)"
                    ))
                })?
                .with_timezone(&chrono::Utc);
            (utc, from_tz_name)
        };
        let local = utc_instant.with_timezone(&to_tz);
        Ok(json!({
            "from_tz": effective_from_tz,
            "to_tz": to_tz_name,
            "utc": utc_instant.to_rfc3339(),
            "converted": local.to_rfc3339(),
        }))
    }
}

fn parse_string(args: &Value, key: &str) -> Result<String, ToolError> {
    match args.get(key) {
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                Err(ToolError::InvalidArguments(format!(
                    "`{key}` must be a non-empty string"
                )))
            } else {
                Ok(t.to_string())
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments(format!("missing `{key}`"))),
    }
}

fn parse_optional_string(args: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                Ok(None)
            } else {
                Ok(Some(t.to_string()))
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a string, got {}",
            kind_of(other)
        ))),
    }
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["datetime", "to_tz"],
        "properties": {
            "datetime": {
                "type": "string",
                "minLength": 1,
                "description": "RFC-3339 string (with offset) or `YYYY-MM-DDTHH:MM:SS` + from_tz"
            },
            "from_tz": {
                "type": "string",
                "description": "IANA tz name (e.g. Asia/Taipei). Required when datetime has no offset."
            },
            "to_tz": {
                "type": "string",
                "minLength": 1,
                "description": "Target IANA tz name (e.g. UTC, America/New_York)"
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["from_tz", "to_tz", "utc", "converted"],
        "properties": {
            "from_tz": {"type": "string"},
            "to_tz": {"type": "string"},
            "utc": {"type": "string"},
            "converted": {"type": "string"}
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: Value) -> Result<Value, ToolError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(TimezoneConvertTool::new().call(args))
    }

    #[test]
    fn taipei_to_utc_from_naive() {
        let out = run(json!({
            "datetime": "2024-06-01T12:00:00",
            "from_tz": "Asia/Taipei",
            "to_tz": "UTC",
        }))
        .unwrap();
        // Taipei is UTC+8 year-round (no DST), so 12:00 → 04:00 UTC.
        assert_eq!(out["utc"], "2024-06-01T04:00:00+00:00");
    }

    #[test]
    fn rfc3339_input_with_offset() {
        let out = run(json!({
            "datetime": "2024-06-01T12:00:00+08:00",
            "to_tz": "America/New_York",
        }))
        .unwrap();
        // 12:00 +0800 → 04:00 UTC → 00:00 EDT (UTC-4 in June).
        assert!(
            out["converted"]
                .as_str()
                .unwrap()
                .starts_with("2024-06-01T00:00:00-04:00")
        );
    }

    #[test]
    fn rejects_invalid_target_tz() {
        let err = run(json!({
            "datetime": "2024-06-01T00:00:00Z",
            "to_tz": "Not/Real",
        }))
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn naive_datetime_without_from_tz_is_error() {
        let err = run(json!({
            "datetime": "2024-06-01T12:00:00",
            "to_tz": "UTC",
        }))
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    /// Spring-forward DST gap: 2024-03-10 02:30 doesn't exist in
    /// `America/New_York` (clocks jumped from 02:00 to 03:00). The
    /// tool should reject this rather than silently fabricate an
    /// instant.
    #[test]
    fn dst_spring_forward_gap_is_error() {
        let err = run(json!({
            "datetime": "2024-03-10T02:30:00",
            "from_tz": "America/New_York",
            "to_tz": "UTC",
        }))
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(matches!(err, ToolError::InvalidArguments(_)));
        assert!(
            msg.contains("DST gap"),
            "error should mention DST gap, got {msg}"
        );
    }

    /// Fall-back DST fold: 2024-11-03 01:30 in `America/New_York`
    /// exists TWICE (clocks went 02:00 → 01:00). `.earliest()`
    /// resolves to the first (EDT) instant, which converts to
    /// 05:30 UTC. The later (EST) instant would be 06:30 UTC.
    #[test]
    fn dst_fall_back_fold_resolves_to_earlier_instant() {
        let out = run(json!({
            "datetime": "2024-11-03T01:30:00",
            "from_tz": "America/New_York",
            "to_tz": "UTC",
        }))
        .unwrap();
        // Earlier (EDT, UTC-4) representation: local 01:30 → 05:30 UTC.
        assert_eq!(out["utc"], "2024-11-03T05:30:00+00:00");
    }

    /// Supplying both an RFC-3339 offset AND `from_tz` is rejected
    /// — silently ignoring `from_tz` would be misleading.
    #[test]
    fn rfc3339_with_explicit_from_tz_is_error() {
        let err = run(json!({
            "datetime": "2024-06-01T12:00:00+08:00",
            "from_tz": "America/New_York",
            "to_tz": "UTC",
        }))
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }
}
