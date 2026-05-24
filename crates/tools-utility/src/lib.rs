//! Taiwan-specific utility tools: ID validation (this milestone),
//! address normalizer (#3.7), ROC date utilities (#3.8), canonicalizers
//! and code dictionaries (#3.10–#3.12). Each tool implements
//! [`mcp_core::ToolHandler`] and is registered with the dispatcher via
//! [`register_utility_tools`].
//!
//! All utilities are state-free pure functions exposed both as a
//! native Rust API (for direct use by other Rust crates / future REST
//! handlers) and as MCP tools (via the wrapper modules in this
//! crate). Keeping the two layers separate means MCP wiring can be
//! unit-tested without spinning up rmcp, and Rust callers don't pay
//! for `serde_json::Value` round-trips.

pub mod address;
pub mod anomaly;
pub mod canonical;
pub mod canonical_tool;
pub mod date;
pub mod date_tools;
pub mod dictionaries;
pub mod dictionary_tools;
pub mod formats;
pub mod formats_tool;
pub mod geo;
pub mod geo_distance_tool;
pub mod geo_geocode_tool;
pub mod geo_nominatim;
pub mod geo_polygon_tool;
pub mod geo_reverse_geocode_tool;
pub mod json_helpers;
pub mod national_id;
pub mod normalize_address_tool;
pub mod passport;
pub mod stats;
pub mod stats_tools;
pub mod tax_id;
pub mod validate_id_tool;

pub use canonical_tool::{
    CanonicalCityDistrictTool, TOOL_NAME as TW_CANONICAL_CITY_DISTRICT_TOOL_NAME,
};
pub use date_tools::{
    GregorianToLunarTool, GregorianToRocTool, IsNationalHolidayTool, RocToGregorianTool,
    SolarTermForDateTool, TOOL_GREGORIAN_TO_LUNAR as TW_GREGORIAN_TO_LUNAR_TOOL_NAME,
    TOOL_GREGORIAN_TO_ROC as TW_GREGORIAN_TO_ROC_TOOL_NAME,
    TOOL_IS_NATIONAL_HOLIDAY as TW_IS_NATIONAL_HOLIDAY_TOOL_NAME,
    TOOL_ROC_TO_GREGORIAN as TW_ROC_TO_GREGORIAN_TOOL_NAME,
    TOOL_SOLAR_TERM_FOR_DATE as TW_SOLAR_TERM_FOR_DATE_TOOL_NAME,
};
pub use dictionary_tools::{
    TOOL_ADMIN_LOOKUP as TW_LOOKUP_ADMIN_CODE_TOOL_NAME,
    TOOL_ADMIN_SEARCH as TW_SEARCH_ADMIN_CODE_TOOL_NAME,
    TOOL_BANK_LOOKUP as TW_LOOKUP_BANK_CODE_TOOL_NAME,
    TOOL_BANK_SEARCH as TW_SEARCH_BANK_CODE_TOOL_NAME,
    TOOL_COUNTY_LOOKUP as TW_LOOKUP_COUNTY_CODE_TOOL_NAME,
    TOOL_COUNTY_SEARCH as TW_SEARCH_COUNTY_CODE_TOOL_NAME,
    TOOL_MRT_LOOKUP as TW_LOOKUP_MRT_STATION_TOOL_NAME,
    TOOL_MRT_SEARCH as TW_SEARCH_MRT_STATION_TOOL_NAME,
    TOOL_POSTAL_LOOKUP as TW_LOOKUP_POSTAL_CODE_TOOL_NAME,
    TOOL_POSTAL_SEARCH as TW_SEARCH_POSTAL_CODE_TOOL_NAME,
};
pub use formats_tool::{TOOL_NAME as TW_VALIDATE_FORMAT_TOOL_NAME, ValidateFormatTool};
pub use normalize_address_tool::{
    NormalizeAddressTool, TOOL_NAME as TW_NORMALIZE_ADDRESS_TOOL_NAME,
};
pub use validate_id_tool::{TOOL_NAME as TW_VALIDATE_ID_TOOL_NAME, ValidateIdTool};

// #6.9 — wave-2 generic tool exports (geo + stats + time-series +
// anomaly). Naming convention: `TOOL_<NAME>` constant + `<NAME>Tool`
// struct so a future round of registry refactoring can lift them
// into a macro without renaming.
pub use anomaly::isolation_scores;
pub use geo::{distance_haversine_m, point_in_polygon};
pub use geo_distance_tool::{DistanceHaversineTool, TOOL_NAME as GEO_DISTANCE_HAVERSINE_TOOL_NAME};
pub use geo_geocode_tool::{GeocodeTool, TOOL_NAME as GEO_GEOCODE_TOOL_NAME};
pub use geo_polygon_tool::{PointInPolygonTool, TOOL_NAME as GEO_POINT_IN_POLYGON_TOOL_NAME};
pub use geo_reverse_geocode_tool::{
    ReverseGeocodeTool, TOOL_NAME as GEO_REVERSE_GEOCODE_TOOL_NAME,
};
pub use stats::{
    Histogram, LinearFit, SeasonalDecomposition, Summary, autocorrelation,
    decompose_seasonal_additive, histogram, linear_regression, moving_average, pearson_correlation,
    percentile, summary,
};
pub use stats_tools::{
    AnomalyIsolationTool, AutocorrelationTool, CorrelationTool, DecomposeSeasonalTool,
    HistogramTool, LinearRegressionTool, MovingAverageTool, PercentileTool, SummaryStatisticsTool,
    TOOL_ANOMALY as ANOMALY_ISOLATION_SCORE_TOOL_NAME,
    TOOL_AUTOCORRELATION as SERIES_AUTOCORRELATION_TOOL_NAME,
    TOOL_CORRELATION as STATS_CORRELATION_TOOL_NAME,
    TOOL_DECOMPOSE_SEASONAL as SERIES_DECOMPOSE_SEASONAL_TOOL_NAME,
    TOOL_HISTOGRAM as STATS_HISTOGRAM_TOOL_NAME,
    TOOL_LINEAR_REGRESSION as STATS_LINEAR_REGRESSION_TOOL_NAME,
    TOOL_MOVING_AVERAGE as SERIES_MOVING_AVERAGE_TOOL_NAME,
    TOOL_PERCENTILE as STATS_PERCENTILE_TOOL_NAME, TOOL_SUMMARY as STATS_SUMMARY_TOOL_NAME,
};

use mcp_core::DispatcherBuilder;

/// Register every utility tool with the supplied dispatcher builder.
///
/// Adding a new utility tool means appending one line to this
/// function — call sites in `mcp-stdio` and `gateway` don't need to
/// change.
pub fn register_utility_tools(builder: DispatcherBuilder) -> DispatcherBuilder {
    builder
        .register(ValidateIdTool::new())
        .register(NormalizeAddressTool::new())
        .register(RocToGregorianTool)
        .register(GregorianToRocTool)
        .register(GregorianToLunarTool)
        .register(SolarTermForDateTool)
        .register(IsNationalHolidayTool)
        .register(CanonicalCityDistrictTool)
        .register(dictionary_tools::ADMIN_DIVISION_GET_TOOL)
        .register(dictionary_tools::ADMIN_DIVISION_SEARCH_TOOL)
        .register(dictionary_tools::MRT_STATION_GET_TOOL)
        .register(dictionary_tools::MRT_STATION_SEARCH_TOOL)
        .register(dictionary_tools::BANK_CODE_GET_TOOL)
        .register(dictionary_tools::BANK_CODE_SEARCH_TOOL)
        .register(dictionary_tools::POSTAL_CODE_GET_TOOL)
        .register(dictionary_tools::POSTAL_CODE_SEARCH_TOOL)
        .register(dictionary_tools::COUNTY_CODE_GET_TOOL)
        .register(dictionary_tools::COUNTY_CODE_SEARCH_TOOL)
        .register(ValidateFormatTool)
        // #6.9 — wave-2 generic tools (geo / stats / time-series /
        // anomaly). 13 tools per the issue's Definition of Done.
        .register(DistanceHaversineTool::new())
        .register(PointInPolygonTool::new())
        .register(GeocodeTool::new())
        .register(ReverseGeocodeTool::new())
        .register(SummaryStatisticsTool::new())
        .register(PercentileTool::new())
        .register(HistogramTool::new())
        .register(CorrelationTool::new())
        .register(LinearRegressionTool::new())
        .register(MovingAverageTool::new())
        .register(AutocorrelationTool::new())
        .register(DecomposeSeasonalTool::new())
        .register(AnomalyIsolationTool::new())
}
