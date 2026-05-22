//! Five MCP tools wrapping [`crate::date`]:
//!
//! - `tw_roc_to_gregorian`
//! - `tw_gregorian_to_roc`
//! - `tw_gregorian_to_lunar`
//! - `tw_solar_term_for_date`
//! - `tw_is_national_holiday`
//!
//! The three table-driven tools (`tw_gregorian_to_lunar`,
//! `tw_solar_term_for_date`, `tw_is_national_holiday`) share a
//! bounded year-range contract (see
//! [`crate::date::SUPPORTED_YEAR_MIN`] /
//! [`crate::date::SUPPORTED_YEAR_MAX`]) — out-of-range queries
//! surface as `ToolError::InvalidArguments` with a clear "extend
//! the table" message. The two ROC↔Gregorian tools are math-only
//! but enforce an upper-bound year of 9999 in the JSON schema as
//! a sanity cap (the underlying functions also guard against
//! `i32::MAX` overflow via `checked_add`).

use async_trait::async_trait;
use mcp_core::{ToolDescriptor, ToolError, ToolHandler};
use serde_json::{Map, Value, json};

use crate::date::{
    DateError, SUPPORTED_YEAR_MAX, SUPPORTED_YEAR_MIN, SolarTerm, gregorian_to_lunar,
    gregorian_to_roc, is_national_holiday, roc_to_gregorian, solar_term_for_date,
};

pub const TOOL_ROC_TO_GREGORIAN: &str = "tw_roc_to_gregorian";
pub const TOOL_GREGORIAN_TO_ROC: &str = "tw_gregorian_to_roc";
pub const TOOL_GREGORIAN_TO_LUNAR: &str = "tw_gregorian_to_lunar";
pub const TOOL_SOLAR_TERM_FOR_DATE: &str = "tw_solar_term_for_date";
pub const TOOL_IS_NATIONAL_HOLIDAY: &str = "tw_is_national_holiday";

#[derive(Debug, Default, Clone)]
pub struct RocToGregorianTool;

#[async_trait]
impl ToolHandler for RocToGregorianTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_ROC_TO_GREGORIAN.to_string(),
            description: "Convert a ROC (民國) date to its Gregorian equivalent. \
                          ROC year 1 = 1912 CE. Validates the resulting Gregorian date \
                          (e.g. 民國 113-02-29 is valid because 2024 is a leap year, but \
                          民國 114-02-29 is not)."
                .to_string(),
            input_schema: roc_input_schema(),
            output_schema: Some(date_output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let (year, month, day) = parse_ymd(&args, "ROC")?;
        let conv = roc_to_gregorian(year, month, day).map_err(|e| map_date_error(&e))?;
        Ok(json!({ "year": conv.year, "month": conv.month, "day": conv.day }))
    }
}

#[derive(Debug, Default, Clone)]
pub struct GregorianToRocTool;

#[async_trait]
impl ToolHandler for GregorianToRocTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_GREGORIAN_TO_ROC.to_string(),
            description: "Convert a Gregorian date to its ROC (民國) equivalent. \
                          Pre-1912 dates are rejected (no negative ROC years)."
                .to_string(),
            input_schema: gregorian_input_schema(),
            output_schema: Some(date_output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let (year, month, day) = parse_ymd(&args, "Gregorian")?;
        let conv = gregorian_to_roc(year, month, day).map_err(|e| map_date_error(&e))?;
        Ok(json!({ "year": conv.year, "month": conv.month, "day": conv.day }))
    }
}

#[derive(Debug, Default, Clone)]
pub struct GregorianToLunarTool;

#[async_trait]
impl ToolHandler for GregorianToLunarTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_GREGORIAN_TO_LUNAR.to_string(),
            description: format!(
                "Convert a Gregorian date to lunar (農曆). Supports Gregorian years {SUPPORTED_YEAR_MIN}-{SUPPORTED_YEAR_MAX}, but note: lunar years are offset — a Gregorian date between Jan 1 and lunar new year falls in the *previous* lunar year. v0.1 does not bake the lunar table for {SUPPORTED_YEAR_MIN}-1, so a small slice of {SUPPORTED_YEAR_MIN} Gregorian dates (before lunar new year of that year) surfaces an InvalidArguments error naming the missing lunar year. Result includes a leap-month flag."
            ),
            input_schema: gregorian_input_schema(),
            output_schema: Some(lunar_output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let (year, month, day) = parse_ymd(&args, "Gregorian")?;
        let lunar = gregorian_to_lunar(year, month, day).map_err(|e| map_date_error(&e))?;
        Ok(json!({
            "year": lunar.year,
            "month": lunar.month,
            "day": lunar.day,
            "leap_month": lunar.leap_month,
        }))
    }
}

#[derive(Debug, Default, Clone)]
pub struct SolarTermForDateTool;

#[async_trait]
impl ToolHandler for SolarTermForDateTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_SOLAR_TERM_FOR_DATE.to_string(),
            description: format!(
                "Look up the solar term (節氣) that starts on the given Gregorian date. Supports years {SUPPORTED_YEAR_MIN}-{SUPPORTED_YEAR_MAX}. Returns null when no term starts on the date."
            ),
            input_schema: gregorian_input_schema(),
            output_schema: Some(solar_term_output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let (year, month, day) = parse_ymd(&args, "Gregorian")?;
        let term = solar_term_for_date(year, month, day).map_err(|e| map_date_error(&e))?;
        Ok(json!({
            "term": term.map(SolarTerm::name_zh),
        }))
    }
}

#[derive(Debug, Default, Clone)]
pub struct IsNationalHolidayTool;

#[async_trait]
impl ToolHandler for IsNationalHolidayTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: TOOL_IS_NATIONAL_HOLIDAY.to_string(),
            description: format!(
                "Check whether a Gregorian date is a Taiwan national holiday. Source: 行政院人事行政總處 + 內政部行事曆. Supports years {SUPPORTED_YEAR_MIN}-{SUPPORTED_YEAR_MAX}. v0.1 does not implement 補假 (make-up day) logic."
            ),
            input_schema: gregorian_input_schema(),
            output_schema: Some(holiday_output_schema()),
        }
    }

    async fn call(&self, args: Value) -> Result<Value, ToolError> {
        let (year, month, day) = parse_ymd(&args, "Gregorian")?;
        let h = is_national_holiday(year, month, day).map_err(|e| map_date_error(&e))?;
        Ok(json!({
            "is_holiday": h.is_holiday,
            "name": h.name,
        }))
    }
}

fn parse_ymd(args: &Value, kind: &str) -> Result<(i32, u32, u32), ToolError> {
    let obj = args
        .as_object()
        .ok_or_else(|| ToolError::InvalidArguments("arguments must be a JSON object".into()))?;
    // Year is range-checked here too (matching the schema's
    // upper bound of 9999) so a caller bypassing schema
    // validation still hits the same contract; the lower bound
    // varies per tool so we use i32::MIN here and rely on the
    // function's own InvalidRocYear / PreRocGregorian guards.
    let year = parse_integer_field(
        obj,
        "year",
        &format!("{kind} year as integer (max 9999)"),
        i64::from(i32::MIN)..=9999,
    )?;
    let month = parse_integer_field(obj, "month", "month (1-12)", 1..=12)?;
    let day = parse_integer_field(obj, "day", "day (1-31)", 1..=31)?;
    let year_i32 = i32::try_from(year)
        .map_err(|_| ToolError::InvalidArguments(format!("`year` must fit in i32, got {year}")))?;
    let month_u32 = u32::try_from(month).expect("month bounded 1..=12 fits in u32");
    let day_u32 = u32::try_from(day).expect("day bounded 1..=31 fits in u32");
    Ok((year_i32, month_u32, day_u32))
}

/// Parse a required integer field with explicit error-shape
/// reporting: missing, wrong JSON type, or out-of-range all
/// surface as separate `InvalidArguments` messages so an agent
/// can tell what to fix.
fn parse_integer_field(
    obj: &Map<String, Value>,
    key: &str,
    description: &str,
    range: std::ops::RangeInclusive<i64>,
) -> Result<i64, ToolError> {
    let value = obj.get(key).ok_or_else(|| {
        ToolError::InvalidArguments(format!("`{key}` is required ({description})"))
    })?;
    let num = value.as_i64().ok_or_else(|| {
        ToolError::InvalidArguments(format!(
            "`{key}` must be an integer, got {} ({description})",
            kind_of(value)
        ))
    })?;
    if !range.contains(&num) {
        return Err(ToolError::InvalidArguments(format!(
            "`{key}` must be in range [{start}, {end}], got {num}",
            start = range.start(),
            end = range.end(),
        )));
    }
    Ok(num)
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

/// Map a `DateError` into the public `ToolError::InvalidArguments`
/// surface. Matches on the reference (relying on Rust's match
/// ergonomics) rather than dereferencing — `DateError` is not
/// `Copy` (its variants would be Copy individually but the enum
/// isn't derived as such), and matching on `err` avoids any
/// confusion about move semantics.
fn map_date_error(err: &DateError) -> ToolError {
    match err {
        DateError::UnsupportedYear(unsupported) => ToolError::InvalidArguments(format!(
            "year {unsupported} is outside the supported {SUPPORTED_YEAR_MIN}-\
             {SUPPORTED_YEAR_MAX} range — extend the static table in \
             tools-utility/src/date.rs to add more years"
        )),
        DateError::UnsupportedLunarYear {
            input_gregorian_year,
            input_month,
            input_day,
            needed_lunar_year,
        } => ToolError::InvalidArguments(format!(
            "Gregorian {input_gregorian_year}-{input_month:02}-{input_day:02} needs the \
             lunar table for year {needed_lunar_year}, which is outside the supported \
             {SUPPORTED_YEAR_MIN}-{SUPPORTED_YEAR_MAX} range. Gregorian dates between \
             Jan 1 and lunar new year fall in the *previous* lunar year. Extend the \
             static table in tools-utility/src/date.rs to add the missing year."
        )),
        DateError::InvalidDate { year, month, day } => ToolError::InvalidArguments(format!(
            "invalid date: year={year} month={month} day={day}"
        )),
        DateError::RocOverflow { roc_year } => ToolError::InvalidArguments(format!(
            "ROC year {roc_year} overflows i32 when converted to Gregorian (roc_year + 1911); \
             use a more reasonable year ≤ {}",
            i32::MAX - 1911,
        )),
        DateError::InvalidRocYear => {
            ToolError::InvalidArguments("ROC year must be ≥ 1 (year 1 = 1912 CE)".to_string())
        }
        DateError::PreRocGregorian => ToolError::InvalidArguments(
            "Gregorian year must be ≥ 1912 for ROC conversion (year 1912 = ROC year 1)".to_string(),
        ),
    }
}

fn roc_input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["year", "month", "day"],
        "properties": {
            // Upper bound is generous (good for ~10k years into the
            // future) but stops i32::MAX nonsense at the schema
            // layer. The `roc_to_gregorian` function itself uses
            // checked_add for callers going around the schema.
            "year": { "type": "integer", "minimum": 1, "maximum": 9999, "description": "ROC year (民國). 1 = 1912 CE." },
            "month": { "type": "integer", "minimum": 1, "maximum": 12 },
            "day": { "type": "integer", "minimum": 1, "maximum": 31 },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn gregorian_input_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["year", "month", "day"],
        "properties": {
            // Sanity-cap year at 9999 to match parse_ymd's runtime
            // bound. The ROC tools also share this cap.
            "year": { "type": "integer", "maximum": 9999, "description": "Gregorian year (CE), capped at 9999." },
            "month": { "type": "integer", "minimum": 1, "maximum": 12 },
            "day": { "type": "integer", "minimum": 1, "maximum": 31 },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled input schema must be an object")
}

fn date_output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["year", "month", "day"],
        "properties": {
            "year": { "type": "integer" },
            "month": { "type": "integer", "minimum": 1, "maximum": 12 },
            "day": { "type": "integer", "minimum": 1, "maximum": 31 },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled output schema must be an object")
}

fn lunar_output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["year", "month", "day", "leap_month"],
        "properties": {
            "year": { "type": "integer" },
            "month": { "type": "integer", "minimum": 1, "maximum": 12 },
            "day": { "type": "integer", "minimum": 1, "maximum": 30 },
            "leap_month": { "type": "boolean", "description": "True when the month is a leap month (閏月)." },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled output schema must be an object")
}

fn solar_term_output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["term"],
        "properties": {
            "term": { "type": ["string", "null"], "description": "zh-TW solar term name (e.g. \"清明\") or null if no term starts on this date." },
        },
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .expect("hand-rolled output schema must be an object")
}

fn holiday_output_schema() -> Map<String, Value> {
    json!({
        "type": "object",
        "required": ["is_holiday", "name"],
        "properties": {
            "is_holiday": { "type": "boolean" },
            "name": { "type": ["string", "null"], "description": "zh-TW holiday name when is_holiday is true." },
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

    fn invoke<T: ToolHandler>(tool: &T, args: Value) -> Value {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(tool.call(args)).expect("call ok")
    }

    fn invoke_err<T: ToolHandler>(tool: &T, args: Value) -> ToolError {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(tool.call(args)).expect_err("call should error")
    }

    #[test]
    fn roc_to_gregorian_113_to_2024() {
        let out = invoke(
            &RocToGregorianTool,
            json!({"year": 113, "month": 1, "day": 1}),
        );
        assert_eq!(out["year"], 2024);
    }

    #[test]
    fn gregorian_to_roc_2025_to_114() {
        let out = invoke(
            &GregorianToRocTool,
            json!({"year": 2025, "month": 6, "day": 15}),
        );
        assert_eq!(out["year"], 114);
    }

    #[test]
    fn lunar_2024_02_10_is_new_year() {
        let out = invoke(
            &GregorianToLunarTool,
            json!({"year": 2024, "month": 2, "day": 10}),
        );
        assert_eq!(out["month"], 1);
        assert_eq!(out["day"], 1);
        assert_eq!(out["leap_month"], false);
    }

    #[test]
    fn solar_term_2025_lichun() {
        let out = invoke(
            &SolarTermForDateTool,
            json!({"year": 2025, "month": 2, "day": 3}),
        );
        assert_eq!(out["term"], "立春");
    }

    #[test]
    fn solar_term_non_term_day_is_null() {
        let out = invoke(
            &SolarTermForDateTool,
            json!({"year": 2025, "month": 3, "day": 15}),
        );
        assert!(out["term"].is_null());
    }

    #[test]
    fn holiday_2025_double_ten() {
        let out = invoke(
            &IsNationalHolidayTool,
            json!({"year": 2025, "month": 10, "day": 10}),
        );
        assert_eq!(out["is_holiday"], true);
        assert_eq!(out["name"], "國慶日");
    }

    #[test]
    fn holiday_non_holiday() {
        let out = invoke(
            &IsNationalHolidayTool,
            json!({"year": 2025, "month": 3, "day": 15}),
        );
        assert_eq!(out["is_holiday"], false);
        assert!(out["name"].is_null());
    }

    #[test]
    fn roc_year_zero_rejected() {
        let err = invoke_err(
            &RocToGregorianTool,
            json!({"year": 0, "month": 1, "day": 1}),
        );
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn invalid_date_rejected() {
        let err = invoke_err(
            &GregorianToRocTool,
            json!({"year": 2025, "month": 2, "day": 29}),
        );
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn out_of_range_year_rejected() {
        let err = invoke_err(
            &GregorianToLunarTool,
            json!({"year": 2030, "month": 1, "day": 1}),
        );
        match err {
            ToolError::InvalidArguments(m) => assert!(m.contains("2024-2027"), "msg: {m}"),
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    /// R4 fix: pre-1912 Gregorian for ROC conversion has its own
    /// error message distinct from "ROC year < 1". Splits the
    /// previously-overloaded `InvalidRocYear` so users know what
    /// to fix.
    #[test]
    fn pre_1912_gregorian_for_roc_has_distinct_error() {
        let err = invoke_err(
            &GregorianToRocTool,
            json!({"year": 1900, "month": 1, "day": 1}),
        );
        match err {
            ToolError::InvalidArguments(m) => {
                assert!(m.contains("Gregorian"), "expected Gregorian-specific: {m}");
                assert!(m.contains("1912"), "expected 1912 boundary: {m}");
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    /// R4 fix: out-of-range year for the non-lunar table-driven
    /// tools must NOT mention "lunar" — that wording only fits
    /// the `gregorian_to_lunar` prev-year fallback.
    #[test]
    fn solar_term_out_of_range_has_no_lunar_wording() {
        let err = invoke_err(
            &SolarTermForDateTool,
            json!({"year": 2030, "month": 3, "day": 15}),
        );
        match err {
            ToolError::InvalidArguments(m) => {
                assert!(!m.contains("lunar"), "must not mention lunar: {m}");
                assert!(m.contains("2030"), "must name 2030: {m}");
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    /// R3 fix: when a Gregorian date in 2024 (in-range) needs the
    /// 2023 lunar table (out-of-range), the error must name 2023
    /// specifically — not the user-supplied 2024 — so the caller
    /// knows what to extend.
    #[test]
    fn pre_lunar_new_year_2024_names_2023_in_error() {
        let err = invoke_err(
            &GregorianToLunarTool,
            json!({"year": 2024, "month": 1, "day": 15}),
        );
        match err {
            ToolError::InvalidArguments(m) => {
                assert!(m.contains("2023"), "must name 2023: {m}");
                assert!(m.contains("lunar"), "must mention lunar: {m}");
                // Full date appears (zero-padded) so callers can
                // copy-paste it without re-checking inputs.
                assert!(
                    m.contains("2024-01-15"),
                    "must include the input date verbatim: {m}",
                );
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[test]
    fn missing_field_rejected() {
        let err = invoke_err(&GregorianToRocTool, json!({"year": 2025}));
        assert!(matches!(err, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn descriptors_advertise_schemas() {
        for d in [
            RocToGregorianTool.descriptor(),
            GregorianToRocTool.descriptor(),
            GregorianToLunarTool.descriptor(),
            SolarTermForDateTool.descriptor(),
            IsNationalHolidayTool.descriptor(),
        ] {
            assert!(d.name.starts_with("tw_"), "tool name: {}", d.name);
            assert!(d.output_schema.is_some());
        }
    }
}
