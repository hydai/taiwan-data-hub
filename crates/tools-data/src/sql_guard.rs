//! Defense-in-depth gate for the `query_rows` MCP tool.
//!
//! The tool exposes a SQL surface to AI clients — a fundamentally
//! risky API. DESIGN.md §6 mandates multi-layer safety (AST whitelist,
//! table whitelist, forced LIMIT, streaming, timeout, isolation); this
//! module is the **AST whitelist** layer that gates everything before
//! Polars sees the query.
//!
//! The contract:
//!
//! 1. **Single SELECT statement** — no `;` chaining, no DDL/DML.
//! 2. **No CTEs, no set ops, no subqueries** — keeps the surface tiny.
//! 3. **Only `current_dataset`** — the one virtual table the Polars
//!    SQL context binds; nothing else may appear in `FROM` or `JOIN`.
//! 4. **Function allow-list** — every function call is checked against
//!    `ALLOWED_FUNCTIONS`. System-y things (`pg_*`, `current_user`,
//!    `version()`) are rejected because they're not in the list.
//! 5. **`LIMIT` clamp** — if the user didn't supply a `LIMIT`, inject
//!    one. If they did, clamp it down. Cap is [`DEFAULT_MAX_LIMIT`]
//!    (`10_000`) per `DESIGN.md` §9.
//!
//! Each layer should hold even if the others are bypassed. The
//! validator returns the post-validation SQL string with LIMIT
//! enforced so callers can hand it straight to Polars.

use std::ops::ControlFlow;

use sqlparser::ast::{
    Expr, Function, LimitClause, ObjectName, ObjectNamePart, Query, Statement, Value,
    ValueWithSpan, Visit, Visitor,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Span;

/// The single virtual table name the Polars SQL context registers for
/// each query. The caller (`query_rows` tool) maps this name to the
/// actual dataset's cached Parquet path; SQL is unaware of physical
/// storage.
pub const ALLOWED_TABLE: &str = "current_dataset";

/// Default upper bound on the user-visible LIMIT. Per DESIGN.md §9
/// (#1.7 DoD): "forced LIMIT ≤ 10000".
pub const DEFAULT_MAX_LIMIT: u64 = 10_000;

/// Function allow-list, lowercase. Reasonable analytics surface
/// without exposing system or filesystem-touching primitives.
const ALLOWED_FUNCTIONS: &[&str] = &[
    // Aggregates
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "stddev",
    "variance",
    // String
    "lower",
    "upper",
    "length",
    "char_length",
    "trim",
    "ltrim",
    "rtrim",
    "substring",
    "substr",
    "concat",
    "replace",
    // Numeric
    "abs",
    "round",
    "floor",
    "ceil",
    "ceiling",
    "sqrt",
    "power",
    "pow",
    "mod",
    "sign",
    "log",
    "log10",
    "exp",
    // Date / type
    "extract",
    "date_part",
    "date_trunc",
    "cast",
    // Conditional
    "coalesce",
    "nullif",
    "greatest",
    "least",
    // Cardinality helpers people commonly reach for
    "case",
];

/// Errors a malformed or disallowed query can produce.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SqlError {
    #[error("SQL parse error: {0}")]
    Parse(String),
    #[error("exactly one statement is allowed per call (got {0})")]
    MultipleStatements(usize),
    #[error("only SELECT statements are allowed")]
    NotSelect,
    #[error("WITH/CTE clauses are not allowed")]
    Cte,
    #[error("set operations (UNION/INTERSECT/EXCEPT) are not allowed")]
    SetOperation,
    #[error("subqueries are not allowed")]
    Subquery,
    #[error("only the `current_dataset` virtual table is allowed; got `{0}`")]
    DisallowedTable(String),
    #[error("function `{0}` is not in the allow-list")]
    DisallowedFunction(String),
    #[error("SELECT … INTO is not allowed")]
    SelectInto,
    #[error("LIMIT must be a non-negative literal integer; got `{0}`")]
    LimitNotLiteral(String),
}

/// The validated SQL ready to hand to Polars. Wraps the string in a
/// newtype so a caller can't accidentally forget to validate, and
/// carries the post-clamp `LIMIT` so the caller can flag truncation
/// without re-parsing the SQL it just got back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSql {
    sql: String,
    effective_limit: u64,
}

impl ValidatedSql {
    pub fn as_str(&self) -> &str {
        &self.sql
    }

    /// The LIMIT value `enforce_limit` settled on for this query —
    /// either the user's clamped down to `max_limit`, or `max_limit`
    /// itself when no LIMIT was present.
    pub fn effective_limit(&self) -> u64 {
        self.effective_limit
    }
}

/// Parse, validate, and clamp the LIMIT of a user SQL string.
///
/// `max_limit` is the highest LIMIT we'll permit (caps any caller-
/// supplied value and is the default when no LIMIT is present).
pub fn validate(sql: &str, max_limit: u64) -> Result<ValidatedSql, SqlError> {
    let mut stmts = Parser::parse_sql(&PostgreSqlDialect {}, sql)
        .map_err(|e| SqlError::Parse(e.to_string()))?;
    if stmts.len() != 1 {
        return Err(SqlError::MultipleStatements(stmts.len()));
    }

    let Statement::Query(mut query) = stmts.remove(0) else {
        return Err(SqlError::NotSelect);
    };

    // CTEs / WITH clauses widen the surface to arbitrary nested
    // queries, even if our table whitelist would reject most.
    if query.with.is_some() {
        return Err(SqlError::Cte);
    }

    // Reject UNION/INTERSECT/EXCEPT and any top-level set operation
    // before deeper validation runs.
    if let sqlparser::ast::SetExpr::SetOperation { .. } = query.body.as_ref() {
        return Err(SqlError::SetOperation);
    }

    walk_relations(&query)?;
    walk_expressions(&query)?;
    walk_select_into(&query)?;

    let effective_limit = enforce_limit(&mut query, max_limit)?;
    Ok(ValidatedSql {
        sql: query.to_string(),
        effective_limit,
    })
}

/// Visitor that collects the first disallowed table reference.
struct RelationVisitor {
    err: Option<SqlError>,
}

impl Visitor for RelationVisitor {
    type Break = ();

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<()> {
        let qname = object_name_string(relation);
        if !qname.eq_ignore_ascii_case(ALLOWED_TABLE) {
            self.err = Some(SqlError::DisallowedTable(qname));
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    }
}

fn walk_relations(query: &Query) -> Result<(), SqlError> {
    let mut visitor = RelationVisitor { err: None };
    let _ = query.visit(&mut visitor);
    visitor.err.map_or(Ok(()), Err)
}

/// Visitor that collects the first disallowed function call OR
/// nested subquery / EXISTS / IN-subquery.
struct ExpressionVisitor {
    err: Option<SqlError>,
}

impl Visitor for ExpressionVisitor {
    type Break = ();

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
        match expr {
            Expr::Function(Function { name, .. }) => {
                let fname = object_name_string(name).to_ascii_lowercase();
                if !ALLOWED_FUNCTIONS.iter().any(|allowed| *allowed == fname) {
                    self.err = Some(SqlError::DisallowedFunction(fname));
                    return ControlFlow::Break(());
                }
            }
            Expr::Subquery(_) | Expr::InSubquery { .. } | Expr::Exists { .. } => {
                self.err = Some(SqlError::Subquery);
                return ControlFlow::Break(());
            }
            _ => {}
        }
        ControlFlow::Continue(())
    }
}

fn walk_expressions(query: &Query) -> Result<(), SqlError> {
    let mut visitor = ExpressionVisitor { err: None };
    let _ = query.visit(&mut visitor);
    visitor.err.map_or(Ok(()), Err)
}

/// Walk the body for `SELECT ... INTO foo` clauses. Visitor doesn't
/// expose those directly, so a small targeted check on each Select
/// suffices.
fn walk_select_into(query: &Query) -> Result<(), SqlError> {
    if let sqlparser::ast::SetExpr::Select(select) = query.body.as_ref()
        && select.into.is_some()
    {
        return Err(SqlError::SelectInto);
    }
    Ok(())
}

/// Force `LIMIT <= max_limit`. If the existing limit is missing, set
/// it to `max_limit`; if it parses as a literal integer larger than
/// `max_limit`, clamp it down. Non-literal LIMIT expressions (a
/// reference or function call) are rejected so we can't be fooled by
/// a runtime-computed huge value. Returns the value the LIMIT was
/// settled to.
fn enforce_limit(query: &mut Query, max_limit: u64) -> Result<u64, SqlError> {
    match &mut query.limit_clause {
        None => {
            query.limit_clause = Some(LimitClause::LimitOffset {
                limit: Some(literal_u64(max_limit)),
                offset: None,
                limit_by: vec![],
            });
            Ok(max_limit)
        }
        Some(LimitClause::LimitOffset { limit, .. }) => {
            let new_limit = if let Some(expr) = limit.as_ref() {
                clamp_limit_expr(expr, max_limit)?
            } else {
                // `LIMIT ALL` or omitted within a LimitOffset
                // construct — set to the cap.
                max_limit
            };
            *limit = Some(literal_u64(new_limit));
            Ok(new_limit)
        }
        Some(LimitClause::OffsetCommaLimit { limit, .. }) => {
            let new_limit = clamp_limit_expr(limit, max_limit)?;
            *limit = literal_u64(new_limit);
            Ok(new_limit)
        }
    }
}

/// Accept only `Expr::Value(Number(...))` and refuse runtime-computed
/// LIMIT values. A SQL like `LIMIT some_column` would compile but
/// could blow past our cap once Polars evaluates it.
fn clamp_limit_expr(expr: &Expr, max_limit: u64) -> Result<u64, SqlError> {
    let Expr::Value(ValueWithSpan { value, .. }) = expr else {
        return Err(SqlError::LimitNotLiteral(format!("{expr}")));
    };
    let n = match value {
        Value::Number(raw, _) => raw.parse::<u64>().map_err(|_| {
            SqlError::LimitNotLiteral(format!("LIMIT must be a non-negative integer; got `{raw}`"))
        })?,
        _ => return Err(SqlError::LimitNotLiteral(format!("{value:?}"))),
    };
    Ok(n.min(max_limit))
}

/// Build an `Expr::Value(Number(...))` carrying `n` so we can drop
/// it back into the AST.
fn literal_u64(n: u64) -> Expr {
    Expr::Value(ValueWithSpan {
        value: Value::Number(n.to_string(), false),
        span: Span::empty(),
    })
}

/// Flatten `schema.table` (or `db.schema.table`) into a dotted string
/// for error messages and equality checks. Strips quotes so
/// `"current_dataset"` matches the unquoted name.
fn object_name_string(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|p| match p {
            ObjectNamePart::Identifier(id) => id.value.clone(),
            ObjectNamePart::Function(f) => f.name.value.clone(),
        })
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `validate` is the single entry point we test through — any
    /// helper bug shows up as a wrong verdict on a real query.
    fn assert_ok(sql: &str) -> ValidatedSql {
        validate(sql, DEFAULT_MAX_LIMIT).unwrap_or_else(|e| {
            panic!("expected ok for `{sql}`, got error: {e}");
        })
    }

    fn assert_err(sql: &str) -> SqlError {
        validate(sql, DEFAULT_MAX_LIMIT)
            .err()
            .unwrap_or_else(|| panic!("expected error for `{sql}`, got Ok"))
    }

    #[test]
    fn simple_select_passes_and_gets_default_limit() {
        let v = assert_ok("SELECT a, b FROM current_dataset");
        // LIMIT was injected.
        assert!(
            v.as_str().to_ascii_uppercase().contains("LIMIT 10000"),
            "default LIMIT not injected: {}",
            v.as_str(),
        );
    }

    #[test]
    fn limit_within_cap_is_preserved() {
        let v = assert_ok("SELECT a FROM current_dataset LIMIT 50");
        assert!(v.as_str().contains("LIMIT 50"));
    }

    #[test]
    fn limit_over_cap_is_clamped() {
        let v = assert_ok("SELECT a FROM current_dataset LIMIT 999999");
        assert!(
            v.as_str().contains("LIMIT 10000"),
            "limit not clamped: {}",
            v.as_str()
        );
    }

    #[test]
    fn limit_must_be_a_literal_integer() {
        match assert_err("SELECT a FROM current_dataset LIMIT a") {
            SqlError::LimitNotLiteral(_) => {}
            other => panic!("expected LimitNotLiteral, got {other}"),
        }
    }

    #[test]
    fn second_statement_rejected() {
        match assert_err("SELECT a FROM current_dataset; DROP TABLE current_dataset") {
            SqlError::MultipleStatements(n) => assert_eq!(n, 2),
            other => panic!("expected MultipleStatements, got {other}"),
        }
    }

    #[test]
    fn ddl_dml_rejected() {
        for sql in &[
            "INSERT INTO current_dataset VALUES (1, 2)",
            "UPDATE current_dataset SET a = 1",
            "DELETE FROM current_dataset",
            "DROP TABLE current_dataset",
            "CREATE TABLE x (a int)",
            "TRUNCATE current_dataset",
            "ALTER TABLE current_dataset ADD COLUMN c int",
        ] {
            // Some of these (DROP/TRUNCATE) parse as non-Query
            // statements; others (INSERT) too. All must error.
            let err = validate(sql, DEFAULT_MAX_LIMIT).unwrap_err();
            assert!(
                matches!(err, SqlError::NotSelect | SqlError::Parse(_)),
                "expected non-SELECT rejection for `{sql}`, got {err}",
            );
        }
    }

    #[test]
    fn cte_rejected() {
        let sql = "WITH t AS (SELECT a FROM current_dataset) SELECT * FROM t";
        assert_eq!(assert_err(sql), SqlError::Cte);
    }

    #[test]
    fn union_rejected() {
        let sql = "SELECT a FROM current_dataset UNION SELECT a FROM current_dataset";
        assert_eq!(assert_err(sql), SqlError::SetOperation);
    }

    #[test]
    fn other_table_rejected() {
        let err = assert_err("SELECT 1 FROM pg_tables");
        assert!(
            matches!(err, SqlError::DisallowedTable(ref t) if t.eq_ignore_ascii_case("pg_tables")),
            "got {err}",
        );
    }

    #[test]
    fn join_to_other_table_rejected() {
        let err = assert_err(
            "SELECT a FROM current_dataset \
             JOIN pg_class ON current_dataset.id = pg_class.oid",
        );
        assert!(
            matches!(err, SqlError::DisallowedTable(ref t) if t.eq_ignore_ascii_case("pg_class")),
            "got {err}",
        );
    }

    #[test]
    fn subquery_rejected() {
        let err = assert_err(
            "SELECT a FROM current_dataset WHERE b > (SELECT max(c) FROM current_dataset)",
        );
        assert_eq!(err, SqlError::Subquery);
    }

    #[test]
    fn in_subquery_rejected() {
        let err =
            assert_err("SELECT a FROM current_dataset WHERE a IN (SELECT b FROM current_dataset)");
        assert_eq!(err, SqlError::Subquery);
    }

    #[test]
    fn exists_subquery_rejected() {
        let err = assert_err(
            "SELECT a FROM current_dataset WHERE EXISTS (SELECT 1 FROM current_dataset)",
        );
        assert_eq!(err, SqlError::Subquery);
    }

    #[test]
    fn disallowed_function_rejected() {
        // pg_sleep, current_user, version — none of these are in the
        // allow-list, so each is rejected by name.
        for sql in &[
            "SELECT pg_sleep(10) FROM current_dataset",
            "SELECT version() FROM current_dataset",
            "SELECT current_user FROM current_dataset",
        ] {
            // current_user can parse as a column reference (no parens)
            // depending on dialect; the test just asserts it doesn't
            // accidentally pass validation.
            let err = validate(sql, DEFAULT_MAX_LIMIT);
            assert!(err.is_err(), "expected error for `{sql}`, got {err:?}");
        }
    }

    #[test]
    fn allowed_functions_pass() {
        for sql in &[
            "SELECT count(*) FROM current_dataset",
            "SELECT sum(price), avg(price) FROM current_dataset",
            "SELECT lower(title), length(title) FROM current_dataset",
            "SELECT extract(year FROM ts) FROM current_dataset",
            "SELECT coalesce(a, b) FROM current_dataset",
            "SELECT cast(a AS integer) FROM current_dataset",
        ] {
            assert_ok(sql);
        }
    }

    #[test]
    fn quoted_table_name_accepted() {
        // Postgres allows quoting identifiers; the allow check must
        // strip quotes for the equality test.
        assert_ok("SELECT a FROM \"current_dataset\"");
    }

    #[test]
    fn select_into_rejected() {
        let err = assert_err("SELECT a INTO new_table FROM current_dataset");
        assert_eq!(err, SqlError::SelectInto);
    }

    /// Reject obviously injection-shaped payloads. Each must hit
    /// SOME error variant — the exact one isn't promised, just that
    /// the query never reaches Polars.
    #[test]
    fn injection_payloads_rejected() {
        for sql in &[
            "SELECT a FROM current_dataset; DELETE FROM current_dataset",
            "SELECT a FROM current_dataset WHERE 1=1 UNION SELECT password FROM users",
            "SELECT pg_read_file('/etc/passwd') FROM current_dataset",
            "COPY current_dataset TO '/tmp/exfil.csv'",
        ] {
            assert!(
                validate(sql, DEFAULT_MAX_LIMIT).is_err(),
                "expected rejection for `{sql}`",
            );
        }
    }

    /// LIMIT clause must end up as a literal positive integer ≤ cap.
    #[test]
    fn limit_clamp_handles_zero_and_huge() {
        // 0 is technically allowed by SQL and means "return nothing";
        // we keep it as 0 (it's not above the cap).
        let v = assert_ok("SELECT a FROM current_dataset LIMIT 0");
        assert!(v.as_str().contains("LIMIT 0"));
        // u64::MAX clamps to MAX_LIMIT.
        let huge = format!("SELECT a FROM current_dataset LIMIT {}", u64::MAX);
        let v = validate(&huge, DEFAULT_MAX_LIMIT).expect("ok after clamp");
        assert!(v.as_str().contains("LIMIT 10000"));
    }
}
