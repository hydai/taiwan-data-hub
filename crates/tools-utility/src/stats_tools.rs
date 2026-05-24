//! MCP tool wrappers for the seven `stats_*` + two `series_*` tools.
//!
//! Each wrapper is a thin facade over a pure function in `crate::stats`:
//!
//!   - `stats_summary`          → [`stats::summary`]
//!   - `stats_percentile`       → [`stats::percentile`]
//!   - `stats_histogram`        → [`stats::histogram`]
//!   - `stats_correlation`      → [`stats::pearson_correlation`]
//!   - `stats_linear_regression`→ [`stats::linear_regression`]
//!   - `series_moving_average`  → [`stats::moving_average`]
//!   - `series_autocorrelation` → [`stats::autocorrelation`]
//!   - `series_decompose_seasonal` → [`stats::decompose_seasonal_additive`]
//!   - `anomaly_isolation_score`   → [`anomaly::isolation_scores`]
//!
//! Kept in one file because the wrappers are mechanically similar
//! (parse a `values: number[]` array, call the pure function, shape
//! the result into JSON, set an output schema). Splitting them into
//! nine files would duplicate `parse_values_array` nine times.
//!
//! Numeric-cast allow: `parse_required_uint` returns `u64` (the
//! widest type `serde_json::Number::as_u64` produces), and every
//! call site immediately casts to `usize` for indexing. The values
//! are bounded by the per-tool input schema (`window` ≤
//! values.length, `max_lag` < values.length, etc.) so the cast
//! cannot truncate in practice — but the lint can't see that, so
//! we silence it module-wide.
#![allow(clippy::cast_possible_truncation)]

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::anomaly;
use crate::json_helpers::kind_of;
use crate::stats;

// =====================================================================
// Shared input helpers — all wrappers parse one or two `values: number[]`
// arrays plus a handful of scalar options.
// =====================================================================

const MAX_VALUES: usize = 100_000;

fn parse_values_array(args: &Value, key: &str) -> Result<Vec<f64>, ToolError> {
    let arr = args.get(key).and_then(Value::as_array).ok_or_else(|| {
        ToolError::InvalidArguments(format!("`{key}` must be an array of numbers"))
    })?;
    if arr.is_empty() {
        return Err(ToolError::InvalidArguments(format!(
            "`{key}` must be non-empty"
        )));
    }
    if arr.len() > MAX_VALUES {
        return Err(ToolError::InvalidArguments(format!(
            "`{key}` length {} exceeds maximum {MAX_VALUES}",
            arr.len()
        )));
    }
    let mut out = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        let n = v.as_f64().ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "`{key}[{i}]` must be a number, got {}",
                kind_of(v)
            ))
        })?;
        if !n.is_finite() {
            return Err(ToolError::InvalidArguments(format!(
                "`{key}[{i}]` must be a finite number, got {n}"
            )));
        }
        out.push(n);
    }
    Ok(out)
}

fn parse_required_uint(args: &Value, key: &str, min: u64, max: u64) -> Result<u64, ToolError> {
    let v = args
        .get(key)
        .ok_or_else(|| ToolError::InvalidArguments(format!("`{key}` is required")))?;
    let n = v.as_u64().ok_or_else(|| {
        ToolError::InvalidArguments(format!(
            "`{key}` must be a non-negative integer, got {}",
            kind_of(v)
        ))
    })?;
    if n < min || n > max {
        return Err(ToolError::InvalidArguments(format!(
            "`{key}` must be in {min}..={max}, got {n}"
        )));
    }
    Ok(n)
}

fn parse_optional_bool(args: &Value, key: &str, default: bool) -> Result<bool, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a boolean, got {}",
            kind_of(other)
        ))),
    }
}

fn parse_required_number(args: &Value, key: &str, min: f64, max: f64) -> Result<f64, ToolError> {
    let v = args
        .get(key)
        .ok_or_else(|| ToolError::InvalidArguments(format!("`{key}` is required")))?;
    let n = v.as_f64().ok_or_else(|| {
        ToolError::InvalidArguments(format!("`{key}` must be a number, got {}", kind_of(v)))
    })?;
    if !n.is_finite() || n < min || n > max {
        return Err(ToolError::InvalidArguments(format!(
            "`{key}` must be a finite number in [{min}, {max}], got {n}"
        )));
    }
    Ok(n)
}

// =====================================================================
// stats_summary — count / mean / median / variance / std_dev / min / max / sum
// =====================================================================

pub const TOOL_SUMMARY: &str = "stats_summary";

#[derive(Debug, Default, Clone)]
pub struct SummaryStatisticsTool;
impl SummaryStatisticsTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for SummaryStatisticsTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_SUMMARY.to_string(),
            description: "Population summary statistics (count, mean, \
                          median, variance, std_dev, min, max, sum) for \
                          a non-empty array of finite numbers."
                .to_string(),
            input_schema: values_only_schema(),
            output_schema: Some(
                json!({
                    "type": "object",
                    "required": ["count","mean","median","variance","std_dev","min","max","sum"],
                    "properties": {
                        "count":{"type":"integer"},
                        "mean":{"type":"number"},
                        "median":{"type":"number"},
                        "variance":{"type":"number"},
                        "std_dev":{"type":"number"},
                        "min":{"type":"number"},
                        "max":{"type":"number"},
                        "sum":{"type":"number"}
                    },
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let values = parse_values_array(&args, "values")?;
        let s = stats::summary(&values)
            .ok_or_else(|| ToolError::InvalidArguments("`values` was empty".into()))?;
        Ok(json!({
            "count": s.count,
            "mean": s.mean,
            "median": s.median,
            "variance": s.variance,
            "std_dev": s.std_dev,
            "min": s.min,
            "max": s.max,
            "sum": s.sum,
        }))
    }
}

// =====================================================================
// stats_percentile
// =====================================================================

pub const TOOL_PERCENTILE: &str = "stats_percentile";

#[derive(Debug, Default, Clone)]
pub struct PercentileTool;
impl PercentileTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for PercentileTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_PERCENTILE.to_string(),
            description: "Linear-interpolation percentile (NumPy default \
                          mode). `p` ∈ [0, 100]. Returns the interpolated \
                          value at that rank."
                .to_string(),
            input_schema: {
                let mut m = values_only_schema();
                m.get_mut("properties")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(
                        "p".into(),
                        json!({"type":"number","minimum":0,"maximum":100,
                           "description":"Percentile rank in [0, 100]"}),
                    );
                m.get_mut("required")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .push(json!("p"));
                m
            },
            output_schema: Some(
                json!({
                    "type":"object",
                    "required":["value","p"],
                    "properties": {"value":{"type":"number"},"p":{"type":"number"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let values = parse_values_array(&args, "values")?;
        let p = parse_required_number(&args, "p", 0.0, 100.0)?;
        let v = stats::percentile(&values, p)
            .ok_or_else(|| ToolError::InvalidArguments("percentile not computable".into()))?;
        Ok(json!({"value": v, "p": p}))
    }
}

// =====================================================================
// stats_histogram
// =====================================================================

pub const TOOL_HISTOGRAM: &str = "stats_histogram";

#[derive(Debug, Default, Clone)]
pub struct HistogramTool;
impl HistogramTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for HistogramTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_HISTOGRAM.to_string(),
            description: "Equal-width histogram with `bins` buckets. \
                          Values equal to the maximum fall in the last \
                          bucket (consistent with np.histogram). For \
                          degenerate input (all values equal) returns a \
                          single bin covering the value."
                .to_string(),
            input_schema: {
                let mut m = values_only_schema();
                m.get_mut("properties")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(
                        "bins".into(),
                        json!({"type":"integer","minimum":1,"maximum":1000,
                           "default":10,"description":"Bucket count (default 10, max 1000)"}),
                    );
                m
            },
            output_schema: Some(
                json!({
                    "type":"object",
                    "required":["counts","edges"],
                    "properties": {
                        "counts":{"type":"array","items":{"type":"integer"}},
                        "edges":{"type":"array","items":{"type":"number"}},
                    },
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let values = parse_values_array(&args, "values")?;
        let bins = match args.get("bins") {
            None | Some(Value::Null) => 10,
            _ => parse_required_uint(&args, "bins", 1, 1000)? as usize,
        };
        let h = stats::histogram(&values, bins)
            .ok_or_else(|| ToolError::InvalidArguments("histogram not computable".into()))?;
        Ok(json!({"counts": h.counts, "edges": h.edges}))
    }
}

// =====================================================================
// stats_correlation
// =====================================================================

pub const TOOL_CORRELATION: &str = "stats_correlation";

#[derive(Debug, Default, Clone)]
pub struct CorrelationTool;
impl CorrelationTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for CorrelationTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_CORRELATION.to_string(),
            description: "Pearson correlation coefficient between two \
                          equal-length series of finite numbers. Returns \
                          null when either series has zero variance."
                .to_string(),
            input_schema: two_series_schema(),
            output_schema: Some(
                json!({
                    "type":"object",
                    "required":["r"],
                    "properties":{"r":{"type":["number","null"],
                                          "description":"Pearson r in [-1, 1] or null on zero variance"}},
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let xs = parse_values_array(&args, "xs")?;
        let ys = parse_values_array(&args, "ys")?;
        if xs.len() != ys.len() {
            return Err(ToolError::InvalidArguments(
                "`xs` and `ys` must have the same length".into(),
            ));
        }
        let r = stats::pearson_correlation(&xs, &ys);
        Ok(json!({"r": r}))
    }
}

// =====================================================================
// stats_linear_regression
// =====================================================================

pub const TOOL_LINEAR_REGRESSION: &str = "stats_linear_regression";

#[derive(Debug, Default, Clone)]
pub struct LinearRegressionTool;
impl LinearRegressionTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for LinearRegressionTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_LINEAR_REGRESSION.to_string(),
            description: "Simple OLS linear regression `y = slope · x + \
                          intercept` over equal-length finite-number \
                          series. Reports the fit + R²."
                .to_string(),
            input_schema: two_series_schema(),
            output_schema: Some(
                json!({
                    "type":"object",
                    "required":["slope","intercept","r_squared"],
                    "properties":{
                        "slope":{"type":"number"},
                        "intercept":{"type":"number"},
                        "r_squared":{"type":"number"},
                    },
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let xs = parse_values_array(&args, "xs")?;
        let ys = parse_values_array(&args, "ys")?;
        if xs.len() != ys.len() {
            return Err(ToolError::InvalidArguments(
                "`xs` and `ys` must have the same length".into(),
            ));
        }
        let fit = stats::linear_regression(&xs, &ys).ok_or_else(|| {
            ToolError::InvalidArguments("`xs` has zero variance — slope undefined".into())
        })?;
        Ok(json!({
            "slope": fit.slope,
            "intercept": fit.intercept,
            "r_squared": fit.r_squared,
        }))
    }
}

// =====================================================================
// series_moving_average
// =====================================================================

pub const TOOL_MOVING_AVERAGE: &str = "series_moving_average";

#[derive(Debug, Default, Clone)]
pub struct MovingAverageTool;
impl MovingAverageTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for MovingAverageTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_MOVING_AVERAGE.to_string(),
            description: "Rolling-window arithmetic mean. With \
                          `center=false` returns `n - window + 1` \
                          trailing means; with `center=true` returns a \
                          same-length series padded with null at the \
                          endpoints where the window doesn't fit."
                .to_string(),
            input_schema: {
                let mut m = values_only_schema();
                let props = m.get_mut("properties").unwrap().as_object_mut().unwrap();
                props.insert(
                    "window".into(),
                    json!({"type":"integer","minimum":1,"description":"Window size (must be ≤ values.length)"}),
                );
                props.insert(
                    "center".into(),
                    json!({"type":"boolean","default":false,
                           "description":"If true, pad endpoints with null so the output length matches values.length"}),
                );
                m.get_mut("required")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .push(json!("window"));
                m
            },
            output_schema: Some(
                json!({
                    "type":"object",
                    "required":["means"],
                    "properties":{
                        "means":{"type":"array","items":{"type":["number","null"]}},
                    },
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let values = parse_values_array(&args, "values")?;
        let window = parse_required_uint(&args, "window", 1, values.len() as u64)? as usize;
        let center = parse_optional_bool(&args, "center", false)?;
        let means = stats::moving_average(&values, window, center)
            .ok_or_else(|| ToolError::InvalidArguments("moving_average inputs invalid".into()))?;
        let json_means: Vec<Value> = means
            .into_iter()
            .map(|m| if m.is_nan() { Value::Null } else { json!(m) })
            .collect();
        Ok(json!({"means": json_means}))
    }
}

// =====================================================================
// series_autocorrelation
// =====================================================================

pub const TOOL_AUTOCORRELATION: &str = "series_autocorrelation";

#[derive(Debug, Default, Clone)]
pub struct AutocorrelationTool;
impl AutocorrelationTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for AutocorrelationTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_AUTOCORRELATION.to_string(),
            description: "Sample autocorrelation function (ACF) at lags \
                          0..=max_lag. Lag 0 is always 1.0. Uses \
                          population variance so all values are bounded \
                          to [-1, 1] even for short series. Returns \
                          InvalidArguments when the series has zero \
                          variance."
                .to_string(),
            input_schema: {
                let mut m = values_only_schema();
                m.get_mut("properties")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(
                        "max_lag".into(),
                        json!({"type":"integer","minimum":1,
                           "description":"Maximum lag (must be < values.length)"}),
                    );
                m.get_mut("required")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .push(json!("max_lag"));
                m
            },
            output_schema: Some(
                json!({
                    "type":"object",
                    "required":["acf"],
                    "properties":{
                        "acf":{"type":"array","items":{"type":"number"}}
                    },
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let values = parse_values_array(&args, "values")?;
        let max_lag =
            parse_required_uint(&args, "max_lag", 1, (values.len() as u64).saturating_sub(1))?
                as usize;
        let acf = stats::autocorrelation(&values, max_lag)
            .ok_or_else(|| ToolError::InvalidArguments("`values` has zero variance".into()))?;
        Ok(json!({"acf": acf}))
    }
}

// =====================================================================
// series_decompose_seasonal
// =====================================================================

pub const TOOL_DECOMPOSE_SEASONAL: &str = "series_decompose_seasonal";

#[derive(Debug, Default, Clone)]
pub struct DecomposeSeasonalTool;
impl DecomposeSeasonalTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for DecomposeSeasonalTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_DECOMPOSE_SEASONAL.to_string(),
            description: "Classical additive seasonal decomposition. \
                          Returns trend (centred moving average), \
                          seasonal (mean-centred per-phase index), and \
                          residual (input − trend − seasonal). Trend \
                          endpoints where the moving-average window \
                          doesn't fit are returned as null. `period` \
                          must be ≥ 2 and ≤ values.length / 2."
                .to_string(),
            input_schema: {
                let mut m = values_only_schema();
                m.get_mut("properties").unwrap().as_object_mut().unwrap().insert(
                    "period".into(),
                    json!({"type":"integer","minimum":2,
                           "description":"Seasonal period (e.g. 12 for monthly, 7 for daily-of-week)"}),
                );
                m.get_mut("required")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .push(json!("period"));
                m
            },
            output_schema: Some(
                json!({
                    "type":"object",
                    "required":["trend","seasonal","residual","period"],
                    "properties":{
                        "trend":{"type":"array","items":{"type":["number","null"]}},
                        "seasonal":{"type":"array","items":{"type":"number"}},
                        "residual":{"type":"array","items":{"type":["number","null"]}},
                        "period":{"type":"integer"},
                    },
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let values = parse_values_array(&args, "values")?;
        let max_period = (values.len() / 2) as u64;
        if max_period < 2 {
            return Err(ToolError::InvalidArguments(
                "`values` must have length ≥ 4 (need values.length / 2 ≥ 2 for decomposition)"
                    .into(),
            ));
        }
        let period = parse_required_uint(&args, "period", 2, max_period)? as usize;
        let d = stats::decompose_seasonal_additive(&values, period)
            .ok_or_else(|| ToolError::InvalidArguments("decomposition inputs invalid".into()))?;
        Ok(json!({
            "trend": nan_to_null(&d.trend),
            "seasonal": d.seasonal,
            "residual": nan_to_null(&d.residual),
            "period": d.period,
        }))
    }
}

// =====================================================================
// anomaly_isolation_score
// =====================================================================

pub const TOOL_ANOMALY: &str = "anomaly_isolation_score";

#[derive(Debug, Default, Clone)]
pub struct AnomalyIsolationTool;
impl AnomalyIsolationTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for AnomalyIsolationTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_ANOMALY.to_string(),
            description: "Single-tree isolation anomaly score per point. \
                          Score in [0, 1]; higher ⇒ more anomalous. \
                          Returns null when all values are equal (no \
                          anomalies to detect). Simplified univariate \
                          variant — for production multi-variate use a \
                          dedicated ML library (linfa-trees)."
                .to_string(),
            input_schema: values_only_schema(),
            output_schema: Some(
                json!({
                    "type":"object",
                    "required":["scores"],
                    "properties":{
                        "scores":{"type":["array","null"],
                                  "items":{"type":"number","minimum":0,"maximum":1}}
                    },
                    "additionalProperties": false,
                })
                .as_object()
                .cloned()
                .expect("hand-rolled schema must be an object"),
            ),
        }
    }
    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let values = parse_values_array(&args, "values")?;
        let scores = anomaly::isolation_scores(&values);
        Ok(json!({"scores": scores}))
    }
}

// =====================================================================
// shared schemas
// =====================================================================

fn values_only_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["values"],
        "properties": {
            "values": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_VALUES,
                "items": {"type": "number"},
                "description": "Array of finite numbers (no NaN / ±∞)."
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

fn two_series_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["xs", "ys"],
        "properties": {
            "xs": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_VALUES,
                "items": {"type": "number"},
            },
            "ys": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_VALUES,
                "items": {"type": "number"},
            }
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled schema must be an object")
}

fn nan_to_null(values: &[f64]) -> Vec<Value> {
    values
        .iter()
        .map(|v| if v.is_nan() { Value::Null } else { json!(v) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run<T: ToolHandler>(tool: &T, args: Value) -> Result<Value, ToolError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(tool.call(args))
    }

    #[test]
    fn summary_happy_path() {
        let out = run(&SummaryStatisticsTool::new(), json!({"values":[1,2,3,4,5]})).unwrap();
        assert_eq!(out["count"], 5);
        assert_eq!(out["sum"], 15.0);
    }

    #[test]
    fn summary_rejects_empty() {
        let err = run(&SummaryStatisticsTool::new(), json!({"values":[]})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn summary_rejects_nan_values() {
        // NaN can't appear via the standard JSON serializer (it's not
        // valid JSON), but a caller using extended JSON might try; we
        // reject finite-check violations at the array boundary.
        let err = run(
            &SummaryStatisticsTool::new(),
            json!({"values":[1, "oops", 3]}),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn percentile_p50_matches_median() {
        let out = run(
            &PercentileTool::new(),
            json!({"values":[1,2,3,4,5], "p": 50}),
        )
        .unwrap();
        assert_eq!(out["value"], 3.0);
    }

    #[test]
    fn histogram_default_bins() {
        let out = run(&HistogramTool::new(), json!({"values":[1,2,3,4,5]})).unwrap();
        let counts = out["counts"].as_array().unwrap();
        assert_eq!(counts.len(), 10); // default bins
    }

    #[test]
    fn correlation_perfect() {
        let out = run(&CorrelationTool::new(), json!({"xs":[1,2,3],"ys":[2,4,6]})).unwrap();
        assert!((out["r"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn correlation_length_mismatch_is_error() {
        let err = run(&CorrelationTool::new(), json!({"xs":[1,2,3],"ys":[1,2]})).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn linreg_perfect_fit() {
        let out = run(
            &LinearRegressionTool::new(),
            json!({"xs":[1,2,3,4],"ys":[2,4,6,8]}),
        )
        .unwrap();
        assert!((out["slope"].as_f64().unwrap() - 2.0).abs() < 1e-9);
        assert!((out["intercept"].as_f64().unwrap()).abs() < 1e-9);
        assert!((out["r_squared"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn moving_average_trailing() {
        let out = run(
            &MovingAverageTool::new(),
            json!({"values":[1,2,3,4,5], "window": 3}),
        )
        .unwrap();
        let means = out["means"].as_array().unwrap();
        assert_eq!(means.len(), 3);
        assert!((means[0].as_f64().unwrap() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn autocorrelation_lag_one() {
        let out = run(
            &AutocorrelationTool::new(),
            json!({"values":[1,2,3,4,5], "max_lag": 1}),
        )
        .unwrap();
        let acf = out["acf"].as_array().unwrap();
        assert_eq!(acf.len(), 2);
        assert!((acf[0].as_f64().unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn decompose_seasonal_minimum_length() {
        // values.length = 4, period = 2 → ok
        let out = run(
            &DecomposeSeasonalTool::new(),
            json!({"values":[1,3,2,4],"period":2}),
        )
        .unwrap();
        assert_eq!(out["period"], 2);
    }

    #[test]
    fn decompose_seasonal_too_short() {
        let err = run(
            &DecomposeSeasonalTool::new(),
            json!({"values":[1,3,2],"period":2}),
        )
        .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn anomaly_single_outlier_scored_highest() {
        let mut v = vec![json!(0); 9];
        v.push(json!(100));
        let out = run(
            &AnomalyIsolationTool::new(),
            json!({"values": Value::Array(v)}),
        )
        .unwrap();
        let scores = out["scores"].as_array().unwrap();
        let max_idx = scores
            .iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.as_f64()
                    .unwrap()
                    .partial_cmp(&b.1.as_f64().unwrap())
                    .unwrap()
            })
            .unwrap()
            .0;
        assert_eq!(max_idx, 9);
    }

    #[test]
    fn anomaly_all_equal_returns_null() {
        let out = run(&AnomalyIsolationTool::new(), json!({"values":[3,3,3]})).unwrap();
        assert!(out["scores"].is_null());
    }
}
