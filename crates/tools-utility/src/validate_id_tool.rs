//! `tw_validate_id` MCP tool — entry point for the four ID validators.
//!
//! One tool with `kind` dispatch keeps the agent surface small: the
//! LLM sees a single "validate a Taiwan ID" affordance instead of
//! having to pick the right validator first.
//!
//! `kind = "auto"` (the default) routes by the input's length+shape
//! signature, which is unambiguous because each format has a unique
//! envelope:
//! - 10 chars matching the `national_id` envelope (first char A-Z plus
//!   either `[1289]` for the modern format or another A-Z for the
//!   legacy 2-letter resident format) → `national_id`
//! - 9 digits                                                       → `passport`
//! - 8 digits                                                       → `tax_id`
//! - anything else (including 10-char strings that fail the envelope) → `unknown`
//!
//! The output shape per the issue's Definition of Done is `{valid,
//! kind, parsed}`. `parsed` is `{}` in two cases:
//! - the input doesn't match any known shape (`kind: "unknown"`); or
//! - the input matches a known shape but is rejected by an explicit
//!   sub-kind restriction — e.g. a valid citizen ID with `kind=arc`
//!   returns `{valid: false, kind: "arc", parsed: {}}`.
//!
//! The per-validator modules document their own `parsed` schemas.

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::{national_id, passport, tax_id};

pub const TOOL_NAME: &str = "tw_validate_id";

/// User-facing `kind` values accepted on input. `arc` is an alias for
/// the modern unified resident-permit format (covered by
/// `national_id` since 2021). It's kept as a separate input value so
/// callers asking "is this an ARC?" can express that intent
/// explicitly; we route it through `national_id::validate` and echo
/// the *requested* kind (`arc` or `legacy_resident`) in the output —
/// not the underlying narrowed kind like `resident`. See
/// [`dispatch_arc`] for the full echo-vs-narrow rules.
const ACCEPTED_KINDS: &[&str] = &["auto", "national_id", "tax_id", "arc", "passport"];

#[derive(Debug, Default, Clone)]
pub struct ValidateIdTool;

impl ValidateIdTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for ValidateIdTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_NAME.to_string(),
            description: "Validate a Taiwan identifier — national ID (身分證), \
                          unified business tax ID (統一編號), resident permit \
                          (居留證 / 統一證號), or ROC passport — and return \
                          structured metadata. Use kind=auto to detect by shape, \
                          or pass an explicit kind when the caller already knows \
                          which family the value belongs to."
                .to_string(),
            input_schema: input_schema(),
            output_schema: Some(output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let value = parse_value(&args)?;
        let requested_kind = parse_kind(&args)?;
        let strict = parse_strict(&args)?;
        // `strict` only affects the tax_id validator's 2023 leniency
        // window. It's silently ignored for kinds that don't traverse
        // tax_id (national_id / arc / passport) — that's the simplest
        // contract for callers who always pass the same opts struct
        // regardless of `kind`.
        let tax_opts = tax_id::Options { strict };

        let response = match requested_kind.as_str() {
            "auto" => dispatch_auto(&value, tax_opts),
            "national_id" => dispatch_national_id(&value),
            "arc" => dispatch_arc(&value),
            "tax_id" => dispatch_tax_id(&value, tax_opts),
            "passport" => dispatch_passport(&value),
            other => {
                return Err(ToolError::InvalidArguments(format!(
                    "`kind` must be one of {ACCEPTED_KINDS:?}, got {other:?}"
                )));
            }
        };
        Ok(response)
    }
}

/// Trims surrounding whitespace before returning so downstream
/// dispatch functions don't re-trim (and the input-schema description
/// "surrounding whitespace is stripped" is honored uniformly). A
/// value that is non-empty but trims to empty (e.g. `"   "`) is
/// rejected here as `InvalidArguments` rather than silently surfacing
/// as `kind: "unknown"` downstream — consistent with how the rest
/// of the codebase treats blank-after-trim values (cf. `query_rows`).
fn parse_value(args: &Value) -> Result<String, ToolError> {
    match args.get("value") {
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Err(ToolError::InvalidArguments(
                    "`value` must be a non-empty string (after trimming whitespace)".into(),
                ))
            } else {
                Ok(trimmed.to_string())
            }
        }
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`value` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolError::InvalidArguments("missing `value`".into())),
    }
}

fn parse_kind(args: &Value) -> Result<String, ToolError> {
    match args.get("kind") {
        None | Some(Value::Null) => Ok("auto".to_string()),
        Some(Value::String(s)) if s.is_empty() => Ok("auto".to_string()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`kind` must be a string, got {}",
            kind_of(other)
        ))),
    }
}

/// Default is `false` (permissive — accept the legacy +1 alternative
/// for tax IDs whose 7th digit is `7`). Set `true` to enforce the
/// strict 2023 rule; see [`tax_id::Options`] for the underlying
/// algorithm.
fn parse_strict(args: &Value) -> Result<bool, ToolError> {
    match args.get("strict") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`strict` must be a boolean, got {}",
            kind_of(other)
        ))),
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Try each validator in length+shape order. Returns the first match.
/// When no shape recognizes the input, returns
/// `{valid: false, kind: "unknown", parsed: {}}`.
///
/// For length-10 inputs we route through [`national_id::validate`]'s
/// own envelope check rather than length alone — a 10-digit string
/// (e.g. `0123456789`) has the right length but doesn't satisfy the
/// `national_id` shape (first char must be A-Z), so auto-dispatch
/// should report `unknown` instead of `national_id` with an empty
/// parse. Explicit `kind="national_id"` still surfaces the `kind`
/// echo for direct callers.
fn dispatch_auto(value: &str, tax_opts: tax_id::Options) -> Value {
    let trimmed = value.trim();
    if trimmed.len() == 10 {
        let (valid, parsed) = national_id::validate(value);
        if let Some(p) = parsed {
            return json!({
                "valid": valid,
                "kind": p.kind.as_str(),
                "parsed": p,
            });
        }
        // Length matches but envelope doesn't — fall through to
        // unknown rather than emit a misleading national_id kind.
    }
    if trimmed.len() == 9 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return dispatch_passport(value);
    }
    if trimmed.len() == 8 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return dispatch_tax_id(value, tax_opts);
    }
    json!({
        "valid": false,
        "kind": "unknown",
        "parsed": {},
    })
}

fn dispatch_national_id(value: &str) -> Value {
    let (valid, parsed) = national_id::validate(value);
    match parsed {
        Some(p) => {
            // `national_id::ParsedNationalId` already serializes to
            // the schema we advertise — embed it under `parsed`.
            json!({
                "valid": valid,
                "kind": p.kind.as_str(),
                "parsed": p,
            })
        }
        None => json!({
            "valid": false,
            "kind": "national_id",
            "parsed": {},
        }),
    }
}

/// `kind = "arc"` narrows to the resident-permit subset of the
/// `national_id` space. If the input parses as a citizen ID, return
/// `valid: false` with `kind: "arc"` so the caller's intent
/// ("validate as ARC") is honored — citizen IDs don't qualify as ARCs
/// even when they're well-formed.
fn dispatch_arc(value: &str) -> Value {
    let (valid, parsed) = national_id::validate(value);
    let Some(p) = parsed else {
        return json!({"valid": false, "kind": "arc", "parsed": {}});
    };
    match p.kind {
        national_id::NationalIdKind::Resident => json!({
            "valid": valid,
            "kind": "arc",
            "parsed": p,
        }),
        national_id::NationalIdKind::LegacyResident => json!({
            // Legacy 2-letter shape is recognized but unverified
            // (see national_id module docs). Still flag the legacy
            // kind so callers see "this looks like a legacy ARC".
            "valid": false,
            "kind": "legacy_resident",
            "parsed": p,
        }),
        national_id::NationalIdKind::Citizen => json!({
            // Caller asked for ARC, parse landed on citizen — report
            // the caller's intent.
            "valid": false,
            "kind": "arc",
            "parsed": {},
        }),
    }
}

fn dispatch_tax_id(value: &str, opts: tax_id::Options) -> Value {
    let (valid, parsed) = tax_id::validate_with(value, opts);
    match parsed {
        Some(p) => json!({
            "valid": valid,
            "kind": "tax_id",
            "parsed": p,
        }),
        None => json!({
            "valid": false,
            "kind": "tax_id",
            "parsed": {},
        }),
    }
}

fn dispatch_passport(value: &str) -> Value {
    let (valid, parsed) = passport::validate(value);
    match parsed {
        Some(p) => json!({
            "valid": valid,
            "kind": "passport",
            "parsed": p,
        }),
        None => json!({
            "valid": false,
            "kind": "passport",
            "parsed": {},
        }),
    }
}

fn input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["value"],
        "properties": {
            "value": {
                "type": "string",
                "minLength": 1,
                "description": "The identifier to validate. ASCII letters are \
                                 normalized to upper case; surrounding whitespace \
                                 is stripped. Whitespace-only values (e.g. `\"   \"`) \
                                 are rejected as InvalidArguments rather than \
                                 silently treated as unknown."
            },
            "kind": {
                "type": "string",
                "enum": ["auto", "national_id", "tax_id", "arc", "passport"],
                "default": "auto",
                "description": "Identifier family. `auto` (default) routes by \
                                 length+shape signature; explicit kinds force a \
                                 single validator and return valid=false when the \
                                 input doesn't match that family."
            },
            "strict": {
                "type": "boolean",
                "default": false,
                "description": "Only affects the 統一編號 (tax_id) validator's \
                                 2023 leniency window. Default false: accept the \
                                 legacy `+1` alternative when the 7th digit is 7. \
                                 Set true to reject the legacy form (modern \
                                 issuance only). Silently ignored for kinds that \
                                 don't traverse tax_id."
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["valid", "kind", "parsed"],
        "properties": {
            "valid": { "type": "boolean" },
            "kind": {
                "type": "string",
                "enum": [
                    "citizen",
                    "resident",
                    "legacy_resident",
                    "tax_id",
                    "passport",
                    "arc",
                    "national_id",
                    "unknown",
                ],
                "description": "Detected (or requested) ID family. `citizen`, \
                                 `resident`, and `legacy_resident` appear when \
                                 the national_id validator could narrow further \
                                 — including for explicit `kind=national_id` \
                                 requests, which still surface the most specific \
                                 family on a successful parse. Explicit \
                                 `kind=arc` echoes back as `arc` (or \
                                 `legacy_resident` for the 2-letter shape); \
                                 `tax_id` and `passport` always echo as-is. \
                                 `unknown` is reserved for auto-detection when \
                                 no shape matches."
            },
            "parsed": {
                "type": "object",
                "description": "Family-specific structured fields. Empty in \
                                 two cases: (a) the input didn't match any \
                                 known shape (kind=unknown); (b) the input \
                                 matched a known shape but was rejected by an \
                                 explicit sub-kind restriction (e.g. a valid \
                                 citizen ID submitted with kind=arc)."
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled output schema must be an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(args: Value) -> Value {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ValidateIdTool::new().call(args))
            .expect("call ok")
    }

    fn invoke_err(args: Value) -> ToolError {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(ValidateIdTool::new().call(args))
            .expect_err("call should error")
    }

    #[test]
    fn descriptor_advertises_input_and_output_schemas() {
        let d = ValidateIdTool::new().descriptor();
        assert_eq!(d.name, "tw_validate_id");
        assert!(d.output_schema.is_some());
    }

    #[test]
    fn missing_value_is_invalid_arguments() {
        let err = invoke_err(json!({}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn non_string_value_is_invalid_arguments() {
        let err = invoke_err(json!({"value": 42}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn unknown_kind_is_invalid_arguments() {
        let err = invoke_err(json!({"value": "A123456789", "kind": "passport2"}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn non_bool_strict_is_invalid_arguments() {
        let err = invoke_err(json!({"value": "12345675", "strict": "yes"}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn strict_true_rejects_legacy_plus_one_alternative() {
        // 12345675 — MOEA's canonical published example; validates
        // only via the legacy +1 rule (digit[6] = 7). See
        // tax_id::tests::canonical_12345675_is_legacy_form for the
        // arithmetic. Permissive default ⇒ valid. strict=true ⇒ invalid.
        let permissive = invoke(json!({"value": "12345675", "kind": "tax_id"}));
        assert_eq!(permissive["valid"], true);
        let strict = invoke(json!({"value": "12345675", "kind": "tax_id", "strict": true}));
        assert_eq!(strict["valid"], false);
        // Parsed metadata still surfaces the legacy diagnosis so the
        // caller can present a meaningful "this is a legacy-format
        // ID; verify with the issuer" message.
        assert_eq!(strict["parsed"]["legacy_alternative"], true);
    }

    #[test]
    fn strict_affects_auto_dispatch_to_tax_id() {
        // Auto-dispatch routes 8-digit input through dispatch_tax_id.
        // strict=true should propagate the option through.
        let out = invoke(json!({"value": "12345675", "strict": true}));
        assert_eq!(out["valid"], false);
        assert_eq!(out["kind"], "tax_id");
    }

    #[test]
    fn strict_is_silently_ignored_for_non_tax_id_kinds() {
        // strict=true on a national_id input doesn't break validation
        // — A123456789 stays valid.
        let out = invoke(json!({"value": "A123456789", "strict": true}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "citizen");
    }

    #[test]
    fn auto_routes_national_id_by_length() {
        let out = invoke(json!({"value": "A123456789"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "citizen");
        assert_eq!(out["parsed"]["canonical"], "A123456789");
    }

    #[test]
    fn auto_routes_tax_id_by_length() {
        // 04595257 is a strict-2023 valid tax_id (digit[6] = 5, so
        // the legacy +1 branch never applies). Picked so the auto-
        // dispatch routing assertion isn't entangled with the legacy
        // form — that wiring has its own dedicated test.
        let out = invoke(json!({"value": "04595257"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "tax_id");
        assert_eq!(out["parsed"]["canonical"], "04595257");
        assert_eq!(out["parsed"]["strict_2023"], true);
        assert_eq!(out["parsed"]["legacy_alternative"], false);
    }

    #[test]
    fn auto_routes_passport_by_length() {
        let out = invoke(json!({"value": "123456789"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "passport");
        assert_eq!(out["parsed"]["canonical"], "123456789");
    }

    #[test]
    fn auto_unknown_for_garbled_input() {
        let out = invoke(json!({"value": "hello"}));
        assert_eq!(out["valid"], false);
        assert_eq!(out["kind"], "unknown");
        assert_eq!(out["parsed"], json!({}));
    }

    #[test]
    fn auto_length_10_without_letter_prefix_is_unknown_not_national_id() {
        // A 10-digit string has the right *length* for national_id
        // but not the right *envelope* (first char must be A-Z).
        // Auto-dispatch must fall through to unknown rather than
        // emit a misleading `kind: "national_id"` echo.
        let out = invoke(json!({"value": "0123456789"}));
        assert_eq!(out["valid"], false);
        assert_eq!(out["kind"], "unknown");
        assert_eq!(out["parsed"], json!({}));
    }

    #[test]
    fn explicit_arc_rejects_citizen_id() {
        // A123456789 is a valid citizen ID — but asking "is this an
        // ARC?" must return false.
        let out = invoke(json!({"value": "A123456789", "kind": "arc"}));
        assert_eq!(out["valid"], false);
        assert_eq!(out["kind"], "arc");
    }

    #[test]
    fn explicit_arc_accepts_modern_resident_id() {
        // Computed in national_id::tests: A812345671 valid modern resident.
        let out = invoke(json!({"value": "A812345671", "kind": "arc"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "arc");
    }

    #[test]
    fn explicit_tax_id_returns_legacy_alternative_flag() {
        // 12345675 — MOEA's canonical legacy +1 form (digit[6] = 7).
        let out = invoke(json!({"value": "12345675", "kind": "tax_id"}));
        assert_eq!(out["valid"], true);
        assert_eq!(out["kind"], "tax_id");
        assert_eq!(out["parsed"]["strict_2023"], false);
        assert_eq!(out["parsed"]["legacy_alternative"], true);
    }

    #[test]
    fn checksum_failure_keeps_valid_false_and_returns_parsed_metadata() {
        let out = invoke(json!({"value": "A123456788"}));
        assert_eq!(out["valid"], false);
        // Auto-routing recognized it as a national_id shape and
        // narrowed to citizen, even though the check digit failed.
        assert_eq!(out["kind"], "citizen");
        assert_eq!(out["parsed"]["canonical"], "A123456788");
    }

    #[test]
    fn explicit_national_id_passes_through_kind_narrowing() {
        let out = invoke(json!({"value": "A123456789", "kind": "national_id"}));
        assert_eq!(out["valid"], true);
        // national_id narrows further on auto-detection — here we
        // surface the narrowed value (citizen) rather than the broad
        // umbrella kind, so callers always see the most specific
        // family for valid inputs.
        assert_eq!(out["kind"], "citizen");
    }

    #[test]
    fn empty_value_is_invalid_arguments() {
        let err = invoke_err(json!({"value": ""}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn whitespace_only_value_is_invalid_arguments() {
        // A whitespace-only value is rejected at parse_value: the
        // input schema promises whitespace is stripped, so callers
        // who submit `"   "` should get an explicit error, not a
        // silent `kind: "unknown"` result that swallows the typo.
        let err = invoke_err(json!({"value": "   "}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn output_conforms_to_declared_schema_shape() {
        // Cheap structural check (same pattern as list_domains).
        for value in ["A123456789", "12345675", "123456789", "garbled"] {
            let out = invoke(json!({"value": value}));
            assert!(out["valid"].is_boolean(), "{value}");
            assert!(out["kind"].is_string(), "{value}");
            assert!(out["parsed"].is_object(), "{value}");
        }
    }
}
