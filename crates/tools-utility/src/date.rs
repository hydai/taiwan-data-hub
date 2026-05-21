//! ROC year, lunar date, solar-term, and national-holiday lookups
//! for Taiwan — pure-Rust, no third-party date crates.
//!
//! Per the #3.8 Definition of Done, this module exposes five
//! functions; each is surfaced as an MCP tool in
//! [`crate::date_tools`].
//!
//! ## ROC ↔ Gregorian
//!
//! Taiwan uses the 民國 (ROC) calendar where year 1 = 1912 CE.
//! Conversion is a simple offset:
//!
//! ```text
//! Gregorian = ROC + 1911
//! ROC       = Gregorian - 1911
//! ```
//!
//! No years pre-1912 are supported (ROC year 0 / negative make no
//! sense in the ROC system); months / days are validated for the
//! Gregorian-side year.
//!
//! ## Lunar dates, solar terms, holidays
//!
//! Lunar-calendar conversion + solar-term computation are
//! ephemeris-driven — proper math would mean hundreds of lines of
//! astronomy. v0.1 bakes static tables for a bounded year range
//! (currently 2024-2027 covering the project's near-future
//! horizon); out-of-range queries surface a clear
//! `UnsupportedYear` error so callers see "extend the table" not
//! "the lookup quietly drifted". Per CLAUDE.md, data source is
//! 內政部公開行事曆 + 中央氣象署 24節氣 ephemeris.

use serde::Serialize;
use thiserror::Error;

/// Result of a ROC ↔ Gregorian conversion. The non-year fields
/// pass through unchanged from the input but are echoed so the
/// caller doesn't have to re-thread them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DateConversion {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

/// Lunar date paired with leap-month info.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LunarDate {
    /// Lunar year (same numeric value as the Gregorian year that
    /// most of the lunar year falls within).
    pub year: i32,
    /// Lunar month [1, 12].
    pub month: u32,
    /// Lunar day [1, 30].
    pub day: u32,
    /// True when the month is a leap month (閏月).
    pub leap_month: bool,
}

/// One of the 24 solar terms (節氣).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SolarTerm {
    LiChun,      // 立春
    YuShui,      // 雨水
    JingZhe,     // 驚蟄
    ChunFen,     // 春分
    QingMing,    // 清明
    GuYu,        // 穀雨
    LiXia,       // 立夏
    XiaoMan,     // 小滿
    MangZhong,   // 芒種
    XiaZhi,      // 夏至
    XiaoShu,     // 小暑
    DaShu,       // 大暑
    LiQiu,       // 立秋
    ChuShu,      // 處暑
    BaiLu,       // 白露
    QiuFen,      // 秋分
    HanLu,       // 寒露
    ShuangJiang, // 霜降
    LiDong,      // 立冬
    XiaoXue,     // 小雪
    DaXue,       // 大雪
    DongZhi,     // 冬至
    XiaoHan,     // 小寒
    DaHan,       // 大寒
}

impl SolarTerm {
    /// Wire name in zh-TW (matches 中央氣象署 publication).
    pub fn name_zh(self) -> &'static str {
        match self {
            Self::LiChun => "立春",
            Self::YuShui => "雨水",
            Self::JingZhe => "驚蟄",
            Self::ChunFen => "春分",
            Self::QingMing => "清明",
            Self::GuYu => "穀雨",
            Self::LiXia => "立夏",
            Self::XiaoMan => "小滿",
            Self::MangZhong => "芒種",
            Self::XiaZhi => "夏至",
            Self::XiaoShu => "小暑",
            Self::DaShu => "大暑",
            Self::LiQiu => "立秋",
            Self::ChuShu => "處暑",
            Self::BaiLu => "白露",
            Self::QiuFen => "秋分",
            Self::HanLu => "寒露",
            Self::ShuangJiang => "霜降",
            Self::LiDong => "立冬",
            Self::XiaoXue => "小雪",
            Self::DaXue => "大雪",
            Self::DongZhi => "冬至",
            Self::XiaoHan => "小寒",
            Self::DaHan => "大寒",
        }
    }
}

/// Result of a national-holiday lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HolidayLookup {
    pub is_holiday: bool,
    /// Holiday name in zh-TW (e.g. "中華民國開國紀念日") when
    /// `is_holiday` is true.
    pub name: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DateError {
    #[error("year {0} is out of the supported range")]
    UnsupportedYear(i32),
    #[error("invalid date: year={year} month={month} day={day}")]
    InvalidDate { year: i32, month: u32, day: u32 },
    #[error("ROC year must be ≥ 1 (year 1 = 1912 CE)")]
    InvalidRocYear,
}

/// Bounds of the static table support. Inclusive on both ends.
pub const SUPPORTED_YEAR_MIN: i32 = 2024;
pub const SUPPORTED_YEAR_MAX: i32 = 2027;

/// Convert a ROC date to its Gregorian form. The month / day pass
/// through and are validated against the resulting Gregorian year
/// (民國 113-02-29 → 2024-02-29 is valid because 2024 is a leap
/// year; 民國 114-02-29 → 2025-02-29 surfaces as `InvalidDate`).
pub fn roc_to_gregorian(roc_year: i32, month: u32, day: u32) -> Result<DateConversion, DateError> {
    if roc_year < 1 {
        return Err(DateError::InvalidRocYear);
    }
    let year = roc_year + 1911;
    validate_gregorian(year, month, day)?;
    Ok(DateConversion { year, month, day })
}

/// Convert a Gregorian date to its ROC form (Gregorian minus
/// 1911). Pre-1912 dates surface as `InvalidRocYear` so callers
/// don't get a negative ROC year that no admin form would accept.
pub fn gregorian_to_roc(year: i32, month: u32, day: u32) -> Result<DateConversion, DateError> {
    if year < 1912 {
        return Err(DateError::InvalidRocYear);
    }
    validate_gregorian(year, month, day)?;
    Ok(DateConversion {
        year: year - 1911,
        month,
        day,
    })
}

/// Convert a Gregorian date to lunar. Table-based for the
/// supported year range.
pub fn gregorian_to_lunar(year: i32, month: u32, day: u32) -> Result<LunarDate, DateError> {
    validate_gregorian(year, month, day)?;
    if !(SUPPORTED_YEAR_MIN..=SUPPORTED_YEAR_MAX).contains(&year) {
        return Err(DateError::UnsupportedYear(year));
    }
    // Walk the baked lunar new-year anchors + month-length tables.
    // All day-of-year values fit easily in i32 (max 366), so we
    // type the intermediates as i32 from the start to avoid the
    // u32→i32 narrowing the clippy `cast_possible_wrap` lint
    // (rightly) refuses without a wider check.
    let info = lunar_year_info(year);
    let anchor_doy = i32::try_from(day_of_year(
        info.anchor_year,
        info.anchor_month,
        info.anchor_day,
    ))
    .expect("day-of-year ≤ 366 fits in i32");
    let target_doy =
        i32::try_from(day_of_year(year, month, day)).expect("day-of-year ≤ 366 fits in i32");
    // Days since the lunar new year of `year` (negative if before).
    let days_since_anchor = if year == info.anchor_year {
        target_doy - anchor_doy
    } else {
        // The lunar new year falls in the previous Gregorian year
        // → step from anchor to target via end of that year.
        let days_in_anchor_year: i32 = if is_leap_year(info.anchor_year) {
            366
        } else {
            365
        };
        (days_in_anchor_year - anchor_doy) + target_doy
    };
    if days_since_anchor < 0 {
        // Pre-lunar-new-year date → belongs to the previous lunar
        // year. v0.1 returns `UnsupportedYear`; supporting prev-year
        // tail requires keeping (year-1)'s table too.
        return Err(DateError::UnsupportedYear(year - 1));
    }
    // Walk months: each entry is (length, is_leap). Sum lengths
    // until we cross days_since_anchor. The cast back to u32 is
    // safe — the < 0 check above is the upper bound on negativity.
    let mut remaining =
        u32::try_from(days_since_anchor).expect("non-negative days_since_anchor fits in u32");
    let mut next_month_number: u32 = 1;
    for (length, is_leap) in info.month_lengths {
        if remaining < *length {
            // The current entry is the hit. A leap entry repeats
            // the *previous* month number with the leap flag set;
            // a non-leap entry uses `next_month_number` directly.
            let reported_month = if *is_leap {
                next_month_number - 1
            } else {
                next_month_number
            };
            return Ok(LunarDate {
                year,
                month: reported_month,
                day: remaining + 1,
                leap_month: *is_leap,
            });
        }
        remaining -= *length;
        // Non-leap entries advance the count; leap entries hold it
        // steady (so the next non-leap entry can take the same
        // number as the most recent advance + 1).
        if !*is_leap {
            next_month_number += 1;
        }
    }
    // Off the end of the year's table — fell into the next lunar
    // new year. Caller should re-query with year+1.
    Err(DateError::UnsupportedYear(year + 1))
}

/// Look up the solar term that begins on the given Gregorian date,
/// or `None` if no term falls on that date.
pub fn solar_term_for_date(
    year: i32,
    month: u32,
    day: u32,
) -> Result<Option<SolarTerm>, DateError> {
    validate_gregorian(year, month, day)?;
    if !(SUPPORTED_YEAR_MIN..=SUPPORTED_YEAR_MAX).contains(&year) {
        return Err(DateError::UnsupportedYear(year));
    }
    for (term, term_month, term_day) in solar_terms_for_year(year) {
        if *term_month == month && *term_day == day {
            return Ok(Some(*term));
        }
    }
    Ok(None)
}

/// Look up whether a Gregorian date is a Taiwan national holiday.
pub fn is_national_holiday(year: i32, month: u32, day: u32) -> Result<HolidayLookup, DateError> {
    validate_gregorian(year, month, day)?;
    if !(SUPPORTED_YEAR_MIN..=SUPPORTED_YEAR_MAX).contains(&year) {
        return Err(DateError::UnsupportedYear(year));
    }
    for (h_month, h_day, name) in holidays_for_year(year) {
        if *h_month == month && *h_day == day {
            return Ok(HolidayLookup {
                is_holiday: true,
                name: Some((*name).to_string()),
            });
        }
    }
    Ok(HolidayLookup {
        is_holiday: false,
        name: None,
    })
}

fn validate_gregorian(year: i32, month: u32, day: u32) -> Result<(), DateError> {
    if !(1..=12).contains(&month) || day == 0 {
        return Err(DateError::InvalidDate { year, month, day });
    }
    let max_day = days_in_month(year, month);
    if day > max_day {
        return Err(DateError::InvalidDate { year, month, day });
    }
    Ok(())
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Day-of-year (1-based) for a given Gregorian date. Caller must
/// have already validated the date via [`validate_gregorian`].
fn day_of_year(year: i32, month: u32, day: u32) -> u32 {
    let mut total = day;
    for m in 1..month {
        total += days_in_month(year, m);
    }
    total
}

/// Static lunar-year metadata: a Gregorian (year, month, day) for
/// the lunar new year (春節), plus month lengths + leap-month
/// flags walking forward through the lunar year.
struct LunarYearInfo {
    anchor_year: i32,
    anchor_month: u32,
    anchor_day: u32,
    /// 12 or 13 entries (an extra entry when there's a leap
    /// month). Each entry: `(length_in_days, is_leap_month)`.
    month_lengths: &'static [(u32, bool)],
}

/// Per-year lunar tables. Source: 中央氣象署 民國農曆年表.
/// Each entry covers the lunar year that *starts* in the given
/// Gregorian year — months past Dec 31 fall into the next
/// Gregorian year and are accounted for by the caller asking the
/// right `year` (the anchor year).
fn lunar_year_info(year: i32) -> LunarYearInfo {
    match year {
        2024 => LunarYearInfo {
            anchor_year: 2024,
            anchor_month: 2,
            anchor_day: 10,
            // 2024 lunar year (甲辰): no leap month; 12 months.
            // Lengths verified against 中央氣象署 民國農曆年表:
            // 29 30 29 29 30 29 30 30 29 30 30 29 = 354 days,
            // anchor 2024-02-10 → next new year 2025-01-29.
            // (端午 = lunar 5/5 = 2024-06-10 is the canonical
            // cross-check.)
            month_lengths: &[
                (29, false), // 正月  Feb 10 – Mar  9
                (30, false), // 二月  Mar 10 – Apr  8
                (29, false), // 三月  Apr  9 – May  7
                (29, false), // 四月  May  8 – Jun  5
                (30, false), // 五月  Jun  6 – Jul  5
                (29, false), // 六月  Jul  6 – Aug  3
                (30, false), // 七月  Aug  4 – Sep  2
                (30, false), // 八月  Sep  3 – Oct  2
                (29, false), // 九月  Oct  3 – Oct 31
                (30, false), // 十月  Nov  1 – Nov 30
                (30, false), // 十一月 Dec  1 – Dec 30
                (29, false), // 十二月 Dec 31 – Jan 28
            ],
        },
        2025 => LunarYearInfo {
            anchor_year: 2025,
            anchor_month: 1,
            anchor_day: 29,
            // 2025 (乙巳): leap 六月. 384 days.
            // 30 29 30 29 30 29 (leap 29) 30 30 29 30 29 30 = 384
            month_lengths: &[
                (30, false),
                (29, false),
                (30, false),
                (29, false),
                (30, false),
                (29, false),
                (29, true), // 閏六月
                (30, false),
                (30, false),
                (29, false),
                (30, false),
                (29, false),
                (30, false),
            ],
        },
        2026 => LunarYearInfo {
            anchor_year: 2026,
            anchor_month: 2,
            anchor_day: 17,
            // 2026 (丙午): no leap. 355 days.
            // 30 29 30 29 30 29 30 29 30 29 30 30 = 355
            month_lengths: &[
                (30, false),
                (29, false),
                (30, false),
                (29, false),
                (30, false),
                (29, false),
                (30, false),
                (29, false),
                (30, false),
                (29, false),
                (30, false),
                (30, false),
            ],
        },
        2027 => LunarYearInfo {
            anchor_year: 2027,
            anchor_month: 2,
            anchor_day: 6,
            // 2027 (丁未): no leap. 354 days.
            month_lengths: &[
                (30, false),
                (29, false),
                (29, false),
                (30, false),
                (29, false),
                (30, false),
                (29, false),
                (30, false),
                (29, false),
                (30, false),
                (30, false),
                (29, false),
            ],
        },
        _ => unreachable!(
            "lunar_year_info called for unsupported year — caller must range-check first"
        ),
    }
}

/// 24 solar terms for each supported year. Source: 中央氣象署 24
/// 節氣表 (2024-2027 published values; minute-level precision
/// rounded to the day).
fn solar_terms_for_year(year: i32) -> &'static [(SolarTerm, u32, u32)] {
    match year {
        2024 => &SOLAR_TERMS_2024,
        2025 => &SOLAR_TERMS_2025,
        2026 => &SOLAR_TERMS_2026,
        2027 => &SOLAR_TERMS_2027,
        _ => unreachable!("solar_terms_for_year called for unsupported year"),
    }
}

const SOLAR_TERMS_2024: [(SolarTerm, u32, u32); 24] = [
    (SolarTerm::XiaoHan, 1, 6),
    (SolarTerm::DaHan, 1, 20),
    (SolarTerm::LiChun, 2, 4),
    (SolarTerm::YuShui, 2, 19),
    (SolarTerm::JingZhe, 3, 5),
    (SolarTerm::ChunFen, 3, 20),
    (SolarTerm::QingMing, 4, 4),
    (SolarTerm::GuYu, 4, 19),
    (SolarTerm::LiXia, 5, 5),
    (SolarTerm::XiaoMan, 5, 20),
    (SolarTerm::MangZhong, 6, 5),
    (SolarTerm::XiaZhi, 6, 21),
    (SolarTerm::XiaoShu, 7, 6),
    (SolarTerm::DaShu, 7, 22),
    (SolarTerm::LiQiu, 8, 7),
    (SolarTerm::ChuShu, 8, 22),
    (SolarTerm::BaiLu, 9, 7),
    (SolarTerm::QiuFen, 9, 22),
    (SolarTerm::HanLu, 10, 8),
    (SolarTerm::ShuangJiang, 10, 23),
    (SolarTerm::LiDong, 11, 7),
    (SolarTerm::XiaoXue, 11, 22),
    (SolarTerm::DaXue, 12, 6),
    (SolarTerm::DongZhi, 12, 21),
];

const SOLAR_TERMS_2025: [(SolarTerm, u32, u32); 24] = [
    (SolarTerm::XiaoHan, 1, 5),
    (SolarTerm::DaHan, 1, 20),
    (SolarTerm::LiChun, 2, 3),
    (SolarTerm::YuShui, 2, 18),
    (SolarTerm::JingZhe, 3, 5),
    (SolarTerm::ChunFen, 3, 20),
    (SolarTerm::QingMing, 4, 4),
    (SolarTerm::GuYu, 4, 20),
    (SolarTerm::LiXia, 5, 5),
    (SolarTerm::XiaoMan, 5, 21),
    (SolarTerm::MangZhong, 6, 5),
    (SolarTerm::XiaZhi, 6, 21),
    (SolarTerm::XiaoShu, 7, 7),
    (SolarTerm::DaShu, 7, 22),
    (SolarTerm::LiQiu, 8, 7),
    (SolarTerm::ChuShu, 8, 23),
    (SolarTerm::BaiLu, 9, 7),
    (SolarTerm::QiuFen, 9, 23),
    (SolarTerm::HanLu, 10, 8),
    (SolarTerm::ShuangJiang, 10, 23),
    (SolarTerm::LiDong, 11, 7),
    (SolarTerm::XiaoXue, 11, 22),
    (SolarTerm::DaXue, 12, 7),
    (SolarTerm::DongZhi, 12, 21),
];

const SOLAR_TERMS_2026: [(SolarTerm, u32, u32); 24] = [
    (SolarTerm::XiaoHan, 1, 5),
    (SolarTerm::DaHan, 1, 20),
    (SolarTerm::LiChun, 2, 4),
    (SolarTerm::YuShui, 2, 18),
    (SolarTerm::JingZhe, 3, 5),
    (SolarTerm::ChunFen, 3, 20),
    (SolarTerm::QingMing, 4, 5),
    (SolarTerm::GuYu, 4, 20),
    (SolarTerm::LiXia, 5, 5),
    (SolarTerm::XiaoMan, 5, 21),
    (SolarTerm::MangZhong, 6, 5),
    (SolarTerm::XiaZhi, 6, 21),
    (SolarTerm::XiaoShu, 7, 7),
    (SolarTerm::DaShu, 7, 23),
    (SolarTerm::LiQiu, 8, 7),
    (SolarTerm::ChuShu, 8, 23),
    (SolarTerm::BaiLu, 9, 7),
    (SolarTerm::QiuFen, 9, 23),
    (SolarTerm::HanLu, 10, 8),
    (SolarTerm::ShuangJiang, 10, 23),
    (SolarTerm::LiDong, 11, 7),
    (SolarTerm::XiaoXue, 11, 22),
    (SolarTerm::DaXue, 12, 7),
    (SolarTerm::DongZhi, 12, 21),
];

const SOLAR_TERMS_2027: [(SolarTerm, u32, u32); 24] = [
    (SolarTerm::XiaoHan, 1, 5),
    (SolarTerm::DaHan, 1, 20),
    (SolarTerm::LiChun, 2, 4),
    (SolarTerm::YuShui, 2, 19),
    (SolarTerm::JingZhe, 3, 6),
    (SolarTerm::ChunFen, 3, 21),
    (SolarTerm::QingMing, 4, 5),
    (SolarTerm::GuYu, 4, 20),
    (SolarTerm::LiXia, 5, 6),
    (SolarTerm::XiaoMan, 5, 21),
    (SolarTerm::MangZhong, 6, 6),
    (SolarTerm::XiaZhi, 6, 21),
    (SolarTerm::XiaoShu, 7, 7),
    (SolarTerm::DaShu, 7, 23),
    (SolarTerm::LiQiu, 8, 8),
    (SolarTerm::ChuShu, 8, 23),
    (SolarTerm::BaiLu, 9, 8),
    (SolarTerm::QiuFen, 9, 23),
    (SolarTerm::HanLu, 10, 8),
    (SolarTerm::ShuangJiang, 10, 23),
    (SolarTerm::LiDong, 11, 7),
    (SolarTerm::XiaoXue, 11, 22),
    (SolarTerm::DaXue, 12, 7),
    (SolarTerm::DongZhi, 12, 22),
];

/// Taiwan national holidays per year. Source: 行政院人事行政總處
/// 公務人員一般辦公日曆 + 內政部行事曆 (fixed-date holidays plus
/// observed dates for lunar holidays).
///
/// Per the v0.1 scope, this is the *observed-date* table — we do
/// not bake make-up-day / 補假 logic. Callers needing fully
/// reflowed work-day calendars should consult the published 行事
/// 曆 directly.
fn holidays_for_year(year: i32) -> &'static [(u32, u32, &'static str)] {
    match year {
        2024 => &HOLIDAYS_2024,
        2025 => &HOLIDAYS_2025,
        2026 => &HOLIDAYS_2026,
        2027 => &HOLIDAYS_2027,
        _ => unreachable!("holidays_for_year called for unsupported year"),
    }
}

const HOLIDAYS_2024: [(u32, u32, &str); 8] = [
    (1, 1, "中華民國開國紀念日"),
    (2, 10, "春節"),
    (2, 28, "和平紀念日"),
    (4, 4, "兒童節"),
    (4, 5, "清明節"),
    (5, 1, "勞動節"),
    (6, 10, "端午節"),
    (10, 10, "國慶日"),
];

const HOLIDAYS_2025: [(u32, u32, &str); 7] = [
    (1, 1, "中華民國開國紀念日"),
    (1, 29, "春節"),
    (2, 28, "和平紀念日"),
    // 兒童節 (always 4/4) and 清明節 happen to fall on the same
    // calendar day in 2025 — `is_national_holiday` returns the
    // first match, so we combine the names rather than burying
    // the second behind an unreachable entry.
    (4, 4, "兒童節 / 清明節"),
    (5, 1, "勞動節"),
    (5, 31, "端午節"),
    (10, 10, "國慶日"),
];

const HOLIDAYS_2026: [(u32, u32, &str); 9] = [
    (1, 1, "中華民國開國紀念日"),
    (2, 17, "春節"),
    (2, 28, "和平紀念日"),
    (4, 4, "兒童節"),
    (4, 5, "清明節"),
    (5, 1, "勞動節"),
    (6, 19, "端午節"),
    (9, 25, "中秋節"),
    (10, 10, "國慶日"),
];

const HOLIDAYS_2027: [(u32, u32, &str); 9] = [
    (1, 1, "中華民國開國紀念日"),
    (2, 6, "春節"),
    (2, 28, "和平紀念日"),
    (4, 4, "兒童節"),
    (4, 5, "清明節"),
    (5, 1, "勞動節"),
    (6, 9, "端午節"),
    (9, 15, "中秋節"),
    (10, 10, "國慶日"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roc_to_gregorian_known_dates() {
        assert_eq!(
            roc_to_gregorian(113, 1, 1).unwrap(),
            DateConversion {
                year: 2024,
                month: 1,
                day: 1
            }
        );
        assert_eq!(
            roc_to_gregorian(114, 12, 31).unwrap(),
            DateConversion {
                year: 2025,
                month: 12,
                day: 31
            }
        );
        // 民國 1 = 1912 CE.
        assert_eq!(roc_to_gregorian(1, 1, 1).unwrap().year, 1912);
    }

    #[test]
    fn gregorian_to_roc_known_dates() {
        assert_eq!(
            gregorian_to_roc(2024, 1, 1).unwrap(),
            DateConversion {
                year: 113,
                month: 1,
                day: 1
            }
        );
        assert_eq!(gregorian_to_roc(2025, 6, 15).unwrap().year, 114);
        assert_eq!(gregorian_to_roc(1912, 1, 1).unwrap().year, 1);
    }

    #[test]
    fn roc_year_zero_rejected() {
        assert_eq!(roc_to_gregorian(0, 1, 1), Err(DateError::InvalidRocYear));
        assert_eq!(roc_to_gregorian(-1, 1, 1), Err(DateError::InvalidRocYear));
    }

    #[test]
    fn pre_1912_gregorian_rejected_for_roc() {
        assert_eq!(
            gregorian_to_roc(1911, 12, 31),
            Err(DateError::InvalidRocYear)
        );
    }

    #[test]
    fn invalid_month_rejected() {
        assert!(matches!(
            roc_to_gregorian(113, 13, 1),
            Err(DateError::InvalidDate { .. })
        ));
        assert!(matches!(
            roc_to_gregorian(113, 0, 1),
            Err(DateError::InvalidDate { .. })
        ));
    }

    #[test]
    fn invalid_day_rejected() {
        // 2025 is not a leap year → Feb 29 invalid
        assert!(matches!(
            gregorian_to_roc(2025, 2, 29),
            Err(DateError::InvalidDate { .. })
        ));
        // 2024 is a leap year → Feb 29 valid
        assert!(gregorian_to_roc(2024, 2, 29).is_ok());
    }

    #[test]
    fn lunar_new_year_2024() {
        let lunar = gregorian_to_lunar(2024, 2, 10).unwrap();
        assert_eq!(lunar.year, 2024);
        assert_eq!(lunar.month, 1);
        assert_eq!(lunar.day, 1);
        assert!(!lunar.leap_month);
    }

    #[test]
    fn lunar_new_year_2025() {
        let lunar = gregorian_to_lunar(2025, 1, 29).unwrap();
        assert_eq!(lunar.month, 1);
        assert_eq!(lunar.day, 1);
    }

    #[test]
    fn lunar_dragon_boat_2024() {
        // 端午 (lunar 5/5) in 2024 = Gregorian 2024-06-10.
        let lunar = gregorian_to_lunar(2024, 6, 10).unwrap();
        assert_eq!(lunar.month, 5);
        assert_eq!(lunar.day, 5);
    }

    /// R1 fix: 2025 has 閏六月 (leap 6th month). A date inside the
    /// leap month must report `month=6` with `leap_month=true`,
    /// **not** `month=7`. 2025 閏六月 spans Gregorian 2025-07-25
    /// through 2025-08-22 inclusive (per 中央氣象署 民國農曆年表).
    #[test]
    fn lunar_leap_month_2025_reports_previous_month_number() {
        // Inside the leap month → month 6, leap=true.
        let leap_day = gregorian_to_lunar(2025, 7, 30).unwrap();
        assert_eq!(leap_day.month, 6);
        assert!(leap_day.leap_month);
        // Just after the leap month → 七月 1, leap=false.
        let after_leap = gregorian_to_lunar(2025, 8, 23).unwrap();
        assert_eq!(after_leap.month, 7);
        assert!(!after_leap.leap_month);
        assert_eq!(after_leap.day, 1);
    }

    #[test]
    fn lunar_out_of_table_range() {
        assert_eq!(
            gregorian_to_lunar(2030, 1, 1),
            Err(DateError::UnsupportedYear(2030))
        );
    }

    #[test]
    fn solar_term_qingming_2024() {
        let term = solar_term_for_date(2024, 4, 4).unwrap();
        assert_eq!(term, Some(SolarTerm::QingMing));
    }

    #[test]
    fn solar_term_lichun_2025() {
        let term = solar_term_for_date(2025, 2, 3).unwrap();
        assert_eq!(term, Some(SolarTerm::LiChun));
    }

    #[test]
    fn solar_term_non_term_day() {
        let term = solar_term_for_date(2024, 4, 3).unwrap();
        assert_eq!(term, None);
    }

    #[test]
    fn solar_term_out_of_range() {
        assert_eq!(
            solar_term_for_date(2030, 1, 1),
            Err(DateError::UnsupportedYear(2030))
        );
    }

    #[test]
    fn solar_term_zh_names() {
        assert_eq!(SolarTerm::QingMing.name_zh(), "清明");
        assert_eq!(SolarTerm::DongZhi.name_zh(), "冬至");
    }

    #[test]
    fn holiday_new_year() {
        let h = is_national_holiday(2025, 1, 1).unwrap();
        assert!(h.is_holiday);
        assert_eq!(h.name.as_deref(), Some("中華民國開國紀念日"));
    }

    #[test]
    fn holiday_double_ten() {
        let h = is_national_holiday(2025, 10, 10).unwrap();
        assert!(h.is_holiday);
        assert_eq!(h.name.as_deref(), Some("國慶日"));
    }

    #[test]
    fn holiday_lunar_new_year_2025() {
        let h = is_national_holiday(2025, 1, 29).unwrap();
        assert!(h.is_holiday);
        assert_eq!(h.name.as_deref(), Some("春節"));
    }

    #[test]
    fn holiday_non_holiday_returns_false() {
        let h = is_national_holiday(2025, 3, 15).unwrap();
        assert!(!h.is_holiday);
        assert_eq!(h.name, None);
    }

    #[test]
    fn holiday_out_of_range() {
        assert_eq!(
            is_national_holiday(2030, 1, 1),
            Err(DateError::UnsupportedYear(2030))
        );
    }
}
