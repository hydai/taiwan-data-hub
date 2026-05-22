//! Small JSON-arg parsing helpers shared across MCP tool
//! wrappers. Centralised here so we don't drift between tools
//! and emit subtly different error messages for the same shape.

use serde_json::Value;

/// Stable string label for the kind of a JSON [`Value`] — used in
/// `InvalidArguments` error messages so callers can tell what
/// they actually sent (e.g. "expected string, got number").
#[must_use]
pub fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kind_of_each_variant() {
        assert_eq!(kind_of(&Value::Null), "null");
        assert_eq!(kind_of(&json!(true)), "boolean");
        assert_eq!(kind_of(&json!(42)), "number");
        assert_eq!(kind_of(&json!("x")), "string");
        assert_eq!(kind_of(&json!([1, 2])), "array");
        assert_eq!(kind_of(&json!({"a": 1})), "object");
    }
}
