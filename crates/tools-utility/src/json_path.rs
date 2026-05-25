//! Pure `JSONPath` query helper (RFC 9535) shared by the
//! `json_path` MCP tool wrapper.
//!
//! ``JSONPath`` (RFC 9535, finalised in 2024) is the canonical query
//! language for `JSON` documents — agents tend to know it from the
//! `XPath` / `JMESPath` family. We use `serde_json_path` because it
//! tracks the RFC and operates directly on `serde_json::Value`
//! without an intermediate marshal step.
//!
//! Parse failures surface the underlying parser's diagnostic
//! verbatim (it already produces helpful messages with caret
//! positions); the wrapper converts them into
//! `mcp_core::ToolError::InvalidArguments`.

use serde_json::Value;
use serde_json_path::JsonPath;

/// Apply a `JSONPath` expression to a JSON value, returning every
/// node the expression selects. Returns the matches in document
/// order (the RFC 9535 guarantee).
///
/// Matched values are cloned out of the input so the caller can
/// `serde_json::to_value` the result without holding a borrow on
/// the original document — the MCP wrapper needs an owned
/// `Value` to put into its response envelope.
pub fn query(input: &Value, expression: &str) -> Result<Vec<Value>, String> {
    let path = JsonPath::parse(expression).map_err(|e| e.to_string())?;
    Ok(path
        .query(input)
        .all()
        .into_iter()
        .cloned()
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Value {
        // Adapted from RFC 9535 §1.5 (the bookstore example) so
        // the assertions track the published spec instead of
        // hand-rolled corner cases. Keeps reviewers' cross-
        // referencing cheap.
        json!({
            "store": {
                "book": [
                    { "category": "reference", "author": "Nigel Rees", "title": "Sayings of the Century", "price": 8.95 },
                    { "category": "fiction",   "author": "Evelyn Waugh", "title": "Sword of Honour",       "price": 12.99 },
                    { "category": "fiction",   "author": "Herman Melville", "title": "Moby Dick", "isbn": "0-553-21311-3", "price": 8.99 },
                    { "category": "fiction",   "author": "J. R. R. Tolkien", "title": "The Lord of the Rings", "isbn": "0-395-19395-8", "price": 22.99 }
                ],
                "bicycle": { "color": "red", "price": 399 }
            }
        })
    }

    #[test]
    fn root_returns_whole_document() {
        let matches = query(&sample(), "$").expect("root query");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], sample());
    }

    #[test]
    fn descendant_selector_collects_every_match() {
        // `$..author` — every `author` value anywhere under root.
        let matches = query(&sample(), "$..author").expect("descendant query");
        assert_eq!(matches.len(), 4);
        let authors: Vec<&str> = matches.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(authors.contains(&"Nigel Rees"));
        assert!(authors.contains(&"J. R. R. Tolkien"));
    }

    #[test]
    fn filter_expression_works() {
        // Every book over $10 — uses RFC 9535 filter syntax.
        let matches =
            query(&sample(), "$.store.book[?@.price > 10]").expect("filter query");
        assert_eq!(matches.len(), 2);
        // Both expensive titles are present.
        let titles: Vec<&str> = matches
            .iter()
            .map(|v| v["title"].as_str().unwrap())
            .collect();
        assert!(titles.contains(&"Sword of Honour"));
        assert!(titles.contains(&"The Lord of the Rings"));
    }

    #[test]
    fn array_index_and_slice() {
        let matches = query(&sample(), "$.store.book[0]").expect("index query");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["title"], "Sayings of the Century");

        let matches = query(&sample(), "$.store.book[1:3]").expect("slice query");
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn no_match_returns_empty_vec_not_error() {
        // A well-formed expression that selects nothing must
        // succeed with an empty result. The MCP wrapper depends
        // on this so it can return `{"matches": [], "count": 0}`
        // for the "valid expression, no hits" case rather than
        // surfacing it as a tool error.
        let matches = query(&sample(), "$.does.not.exist").expect("no-match query");
        assert!(matches.is_empty());

        let matches = query(&sample(), "$.store.book[?@.price > 9999]").expect("empty filter");
        assert!(matches.is_empty());
    }

    #[test]
    fn invalid_expression_surfaces_parser_error() {
        // The parser's own message is more informative than
        // anything we'd hand-write; preserve it verbatim so
        // callers see the caret + reason.
        let err = query(&sample(), "$.[bad syntax").unwrap_err();
        assert!(!err.is_empty(), "parser must emit a non-empty diagnostic");
    }

    #[test]
    fn matches_returned_in_document_order() {
        let matches = query(&sample(), "$..price").expect("descendant prices");
        // Document order per RFC 9535: book[0..3].price then
        // bicycle.price. Pin the exact sequence so a future RFC-
        // 9535 traversal change in `serde_json_path` doesn't
        // silently re-order results.
        let prices: Vec<f64> = matches
            .iter()
            .map(|v| v.as_f64().unwrap_or_default())
            .collect();
        assert_eq!(prices, vec![8.95, 12.99, 8.99, 22.99, 399.0]);
    }
}
