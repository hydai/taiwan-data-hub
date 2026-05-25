//! Pure JSON Schema validation helper shared by the
//! `json_schema_validate` MCP tool.
//!
//! Builds on the `jsonschema` crate (the de-facto Rust
//! implementation). Default draft is **2020-12** — the current
//! shipping standard — with `7` and `2019-09` selectable per
//! request for compatibility with older schema corpora (most of
//! the public `OpenAPI` 3.0 ecosystem still authors against
//! draft 7).
//!
//! Schema compilation errors are distinct from validation
//! failures: a malformed schema is the caller's fault (the
//! wrapper surfaces it as `InvalidArguments`), while validation
//! failures against a well-formed schema are routine output and
//! land in the `errors` list of the response.

use jsonschema::{Draft, Validator};
use serde_json::Value;

/// Supported JSON Schema drafts. Mirrors the subset of
/// [`jsonschema::Draft`] variants the tool exposes — older drafts
/// (4, 6) are intentionally omitted because none of the catalog
/// schemas currently authored against them ship with the project,
/// and the wider Rust ecosystem has consolidated on draft 7+
/// since ~2019.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaDraft {
    Draft7,
    Draft201909,
    Draft202012,
}

impl SchemaDraft {
    /// Default draft when the caller doesn't specify one. 2020-12
    /// is the latest published standard and the format-vocabulary
    /// shapes every new spec is now authored against.
    #[must_use]
    pub fn default_draft() -> Self {
        Self::Draft202012
    }

    /// Parse a user-supplied draft string. Accepts the canonical
    /// shorthands operators tend to type; rejects anything else so
    /// a typo doesn't silently fall back to the default and validate
    /// against the wrong shape.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "7" | "draft7" | "draft-07" => Ok(Self::Draft7),
            "2019-09" | "draft2019-09" | "draft-2019-09" => Ok(Self::Draft201909),
            "2020-12" | "draft2020-12" | "draft-2020-12" => Ok(Self::Draft202012),
            other => Err(format!(
                "unsupported draft {other:?}; use one of \"7\", \"2019-09\", \"2020-12\""
            )),
        }
    }

    fn to_jsonschema_draft(self) -> Draft {
        match self {
            Self::Draft7 => Draft::Draft7,
            Self::Draft201909 => Draft::Draft201909,
            Self::Draft202012 => Draft::Draft202012,
        }
    }
}

/// One validation failure. Mirrors the shape `jsonschema` emits
/// but trimmed to the two fields that are actionable for an LLM
/// agent: the JSON Pointer into `data` where the problem sits,
/// and a human-readable explanation. Multi-line schemas often
/// produce many errors per `validate` call; the tool returns
/// them in document order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError {
    /// RFC 6901 JSON Pointer into the validated `data` value
    /// (e.g. `/users/2/email`). Empty when the failure applies
    /// to the root document.
    pub instance_path: String,
    /// Free-form message from the underlying validator. Includes
    /// the failing keyword (e.g. `"required"`, `"type"`) and the
    /// specific value that tripped it.
    pub message: String,
}

/// Compile + run the schema against `data`, returning at most
/// `limit` errors. Returns `Ok((errors, truncated))` where
/// `truncated` is `true` iff the validator produced more failures
/// than `limit`; trailing errors are discarded without ever being
/// formatted into `SchemaError` structs. `Err(String)` is
/// reserved for malformed schemas (the caller's fault, not the
/// document's).
///
/// Early-stop matters: `jsonschema::iter_errors` is itself lazy
/// (it walks the schema/data pair on demand), so iterating with
/// `take(limit + 1)` bounds both the *count* of allocated error
/// records and the *work* the validator does — important for
/// pathological schemas like `{"items": {"not": {}}}` against a
/// million-element array.
pub fn validate(
    data: &Value,
    schema: &Value,
    draft: SchemaDraft,
    limit: usize,
) -> Result<(Vec<SchemaError>, bool), String> {
    let validator = jsonschema::options()
        .with_draft(draft.to_jsonschema_draft())
        .build(schema)
        .map_err(|e| format!("schema is invalid: {e}"))?;
    Ok(collect_errors(&validator, data, limit))
}

fn collect_errors(validator: &Validator, data: &Value, limit: usize) -> (Vec<SchemaError>, bool) {
    let mut errors = Vec::new();
    let mut truncated = false;
    for err in validator.iter_errors(data) {
        if errors.len() >= limit {
            truncated = true;
            break;
        }
        errors.push(SchemaError {
            instance_path: err.instance_path.to_string(),
            message: err.to_string(),
        });
    }
    (errors, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Generous cap for tests that don't care about truncation;
    /// the truncation-specific test uses a tiny explicit cap.
    const UNBOUNDED: usize = 1_000_000;

    fn user_schema() -> Value {
        json!({
            "type": "object",
            "required": ["name", "age"],
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "age":  { "type": "integer", "minimum": 0 },
                "email": { "type": "string", "format": "email" }
            },
            "additionalProperties": false
        })
    }

    #[test]
    fn valid_document_returns_empty_errors() {
        let data = json!({ "name": "Alice", "age": 30 });
        let errors = {
            let (errors, _) = validate(
                &data,
                &user_schema(),
                SchemaDraft::default_draft(),
                UNBOUNDED,
            )
            .expect("compile");
            errors
        };
        assert!(
            errors.is_empty(),
            "valid doc must produce 0 errors, got {errors:?}"
        );
    }

    #[test]
    fn missing_required_field_is_an_error() {
        let data = json!({ "name": "Alice" });
        let errors = {
            let (errors, _) = validate(
                &data,
                &user_schema(),
                SchemaDraft::default_draft(),
                UNBOUNDED,
            )
            .expect("compile");
            errors
        };
        assert!(!errors.is_empty());
        let combined = errors
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        assert!(
            combined.contains("age"),
            "error message must name the missing field; got {combined:?}",
        );
    }

    #[test]
    fn wrong_type_carries_pointer_into_failing_field() {
        let data = json!({ "name": 42, "age": 30 });
        let errors = {
            let (errors, _) = validate(
                &data,
                &user_schema(),
                SchemaDraft::default_draft(),
                UNBOUNDED,
            )
            .expect("compile");
            errors
        };
        // At least one error should pin the path to /name.
        assert!(
            errors.iter().any(|e| e.instance_path == "/name"),
            "errors must include instance_path=/name; got {errors:?}",
        );
    }

    #[test]
    fn multiple_errors_returned_for_multi_failure_doc() {
        // Both `name` (wrong type) and `age` (negative) fail; the
        // validator should emit one error per failure so the
        // agent can fix them in one round-trip.
        let data = json!({ "name": 42, "age": -1 });
        let errors = {
            let (errors, _) = validate(
                &data,
                &user_schema(),
                SchemaDraft::default_draft(),
                UNBOUNDED,
            )
            .expect("compile");
            errors
        };
        assert!(
            errors.len() >= 2,
            "expected ≥ 2 errors, got {} ({errors:?})",
            errors.len(),
        );
    }

    #[test]
    fn additional_properties_false_rejects_unknown_keys() {
        let data = json!({ "name": "Alice", "age": 30, "spurious": 1 });
        let errors = {
            let (errors, _) = validate(
                &data,
                &user_schema(),
                SchemaDraft::default_draft(),
                UNBOUNDED,
            )
            .expect("compile");
            errors
        };
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("spurious") || e.message.contains("additional")),
            "additionalProperties:false must reject `spurious`; got {errors:?}",
        );
    }

    #[test]
    fn boolean_schemas_round_trip_through_validator() {
        // The JSON Schema spec lets the top-level schema be a
        // boolean: `true` accepts everything, `false` rejects
        // everything. The `jsonschema` crate honours that, so the
        // pure helper must too — pinning this behaviour here so
        // the tool wrapper can rely on it without re-testing.
        let (ok, _) = validate(
            &json!({"any": "value"}),
            &json!(true),
            SchemaDraft::default_draft(),
            UNBOUNDED,
        )
        .expect("compile bool-true");
        assert!(ok.is_empty(), "true schema → 0 errors");
        let (bad, _) = validate(
            &json!(42),
            &json!(false),
            SchemaDraft::default_draft(),
            UNBOUNDED,
        )
        .expect("compile bool-false");
        assert_eq!(bad.len(), 1, "false schema → exactly one rejection");
    }

    #[test]
    fn malformed_schema_is_an_err_not_a_validation_failure() {
        // `"type": "objet"` — typo. The compile step must fail
        // (so the wrapper can return InvalidArguments) rather
        // than silently accepting the schema and producing a
        // misleading pass for every document.
        let schema = json!({ "type": "objet" });
        let result = validate(&json!({}), &schema, SchemaDraft::default_draft(), UNBOUNDED);
        assert!(
            result.is_err(),
            "malformed schema must be Err, got {result:?}"
        );
    }

    #[test]
    fn draft_parse_accepts_canonical_spellings_and_rejects_others() {
        assert_eq!(SchemaDraft::parse("7").unwrap(), SchemaDraft::Draft7);
        assert_eq!(SchemaDraft::parse("draft7").unwrap(), SchemaDraft::Draft7);
        assert_eq!(
            SchemaDraft::parse("2019-09").unwrap(),
            SchemaDraft::Draft201909,
        );
        assert_eq!(
            SchemaDraft::parse("draft-2020-12").unwrap(),
            SchemaDraft::Draft202012,
        );
        assert!(SchemaDraft::parse("12").is_err());
        assert!(SchemaDraft::parse("latest").is_err());
        assert!(SchemaDraft::parse("").is_err());
    }

    #[test]
    fn draft_2020_12_const_keyword_works() {
        // `const` is in every modern draft. This test pins that the
        // 2020-12 selector compiles a schema using it correctly.
        let schema = json!({ "const": "fixed" });
        let (ok, _) = validate(
            &json!("fixed"),
            &schema,
            SchemaDraft::Draft202012,
            UNBOUNDED,
        )
        .expect("compile");
        assert!(ok.is_empty());
        let (bad, _) = validate(
            &json!("other"),
            &schema,
            SchemaDraft::Draft202012,
            UNBOUNDED,
        )
        .expect("compile");
        assert!(!bad.is_empty());
    }

    #[test]
    fn limit_stops_at_cap_with_truncated_flag() {
        // Array of N integers against `items: {"not": {}}` — every
        // item fails, so the validator emits N errors. Cap at 3
        // and verify both early stop and truncation flag.
        let schema = json!({
            "type": "array",
            "items": { "not": {} }
        });
        let data = json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

        let (errors, truncated) =
            validate(&data, &schema, SchemaDraft::Draft202012, 3).expect("compile");
        assert_eq!(errors.len(), 3, "must cap at limit");
        assert!(truncated, "must flag truncation");

        // At-limit: not truncated.
        let (errors, truncated) =
            validate(&data, &schema, SchemaDraft::Draft202012, 10).expect("compile");
        assert_eq!(errors.len(), 10);
        assert!(!truncated, "exactly-at-limit is not truncation");
    }
}
