//! Wave-1 format validators per the issue's Definition of Done.
//! Eight format checkers shipped behind a single MCP tool
//! (`tw_validate_format`) with a `kind` discriminator — same
//! pattern as `tw_validate_id`.
//!
//! Validators included:
//!  - `invoice`        — 統一發票號碼 (8-digit with 中華民國財政部 check)
//!  - `taipower`       — 台電電號 (11-digit format check)
//!  - `water_meter`    — 自來水水號 (format check)
//!  - `phone`          — 中華電信市話/手機格式
//!  - `license_plate`  — TW 車牌 4-字 / 6-字 / 7-字 formats
//!  - `credit_card`    — LUHN check (any major card)
//!  - `iban`           — ISO 13616 mod-97
//!  - `iata_airport`   — 3-letter IATA code (v0.1 subset of
//!    major airports + an `unknown` outcome)
//!
//! 郵遞區號搜尋 is covered by [`crate::dictionary_tools`] via the
//! `tw_lookup_postal_code` / `tw_search_postal_code` pair — no
//! need to re-implement.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

/// Discriminator for which format to validate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatKind {
    Invoice,
    Taipower,
    WaterMeter,
    Phone,
    LicensePlate,
    CreditCard,
    Iban,
    IataAirport,
}

impl FormatKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invoice => "invoice",
            Self::Taipower => "taipower",
            Self::WaterMeter => "water_meter",
            Self::Phone => "phone",
            Self::LicensePlate => "license_plate",
            Self::CreditCard => "credit_card",
            Self::Iban => "iban",
            Self::IataAirport => "iata_airport",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }

    pub const ALL: [FormatKind; 8] = [
        Self::Invoice,
        Self::Taipower,
        Self::WaterMeter,
        Self::Phone,
        Self::LicensePlate,
        Self::CreditCard,
        Self::Iban,
        Self::IataAirport,
    ];
}

/// Result of a format validation. `kind` echoes the requested
/// kind; `valid` is the answer. `detail` carries an optional
/// structured payload (e.g. the resolved IATA airport name, the
/// LUHN-derived issuer hint). Unlike `tw_validate_id`, this tool
/// has no `auto` dispatch — the caller always picks the
/// validator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatResult {
    pub valid: bool,
    pub kind: FormatKind,
    pub detail: Option<String>,
}

#[must_use]
pub fn validate(kind: FormatKind, value: &str) -> FormatResult {
    let trimmed = value.trim();
    // Compute (valid, detail) in one match arm per kind so each
    // expensive path runs at most once — the IataAirport branch
    // looks up the airport name once and reuses it for both
    // fields, and CreditCard avoids re-walking the digits for
    // an issuer hint when the LUHN already failed.
    let (valid, detail) = match kind {
        FormatKind::Invoice => (is_valid_invoice(trimmed), None),
        FormatKind::Taipower => (is_valid_taipower(trimmed), None),
        FormatKind::WaterMeter => (is_valid_water_meter(trimmed), None),
        FormatKind::Phone => (is_valid_phone(trimmed), None),
        FormatKind::LicensePlate => (is_valid_license_plate(trimmed), None),
        FormatKind::CreditCard => {
            let v = is_valid_luhn(trimmed);
            let d = if v {
                credit_card_issuer(trimmed).map(str::to_string)
            } else {
                None
            };
            (v, d)
        }
        FormatKind::Iban => (is_valid_iban(trimmed), None),
        FormatKind::IataAirport => match lookup_iata(trimmed) {
            Some(name) => (true, Some(name.to_string())),
            None => (false, None),
        },
    };
    FormatResult {
        valid,
        kind,
        detail,
    }
}

// ============================================================
//  統一發票號碼 (invoice) — 8 digits + 中華民國財政部 check.
//
//  Algorithm (per 財政部 公報): each digit multiplied by a fixed
//  weights vector [1,2,1,2,1,2,4,1]; for each weighted product,
//  sum its digits (e.g. 14 → 1+4 = 5); finally sum all those
//  digit-sums and check mod 10 == 0 (or the 7th digit is 7 and
//  the alternate sum also passes — special case).
// ============================================================

fn is_valid_invoice(s: &str) -> bool {
    if s.len() != 8 || !s.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let digits: Vec<u32> = s.chars().map(|c| c.to_digit(10).unwrap()).collect();
    invoice_check(&digits, 0) || (digits[6] == 7 && invoice_check(&digits, 1))
}

fn invoice_check(digits: &[u32], add: u32) -> bool {
    const WEIGHTS: [u32; 8] = [1, 2, 1, 2, 1, 2, 4, 1];
    let sum: u32 = digits
        .iter()
        .zip(WEIGHTS.iter())
        .map(|(d, w)| {
            let p = d * w;
            (p / 10) + (p % 10)
        })
        .sum::<u32>()
        + add;
    sum % 10 == 0
}

// ============================================================
//  台電電號 (taipower meter ID) — 11 digits. v0.1 enforces
//  length+digit format; full check-digit algorithm varies by
//  region and isn't published as a single formula (deferred).
// ============================================================

fn is_valid_taipower(s: &str) -> bool {
    s.len() == 11 && s.chars().all(|c| c.is_ascii_digit())
}

// ============================================================
//  自來水水號 (water meter ID) — varies by region; the unified
//  form is 11 digits per 台灣自來水公司 customer record. v0.1
//  enforces length + digit format.
// ============================================================

fn is_valid_water_meter(s: &str) -> bool {
    let digits_only: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    digits_only.len() == 11 && digits_only.chars().all(|c| c.is_ascii_digit())
}

// ============================================================
//  中華電信市話 / 手機 — TW phone numbers.
//
//  Accepted forms:
//   - 市話: 0X-XXXXXXX (X = area code digit; total 9-10 digits)
//   - 手機: 09XX-XXXXXX (10 digits total, leading 09)
//   - Optional +886 prefix replacing the leading 0.
// ============================================================

static PHONE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    // Strip spaces / hyphens before matching, so this regex
    // operates on the compact form.
    //
    // Branches (each starts with `0` or `+886`):
    //  - mobile: 09XX-XXXXXX (10 digits including leading 0)
    //  - 2-digit area code (台北 02; 雙北/桃園/新竹 03; 中部 04;
    //    雲嘉 05; 台南 06; 高屏 07; 屏東/台東 08): the regex
    //    matches `[2-8]` + `\d{7,8}` → 9 or 10 digits including
    //    the leading 0.
    //  - Special long-prefix areas (037 苗栗 / 049 南投 / 082 金門
    //    / 083 馬祖 / 089 台東 / 026 烏坵 / 092): match the
    //    leading 0 + two more digits + a 6-7-digit subscriber.
    //    The 2-digit set `37|49|82|83|89|26|92` is the published
    //    TWNIC prefix list rather than `\d{2}` so nonsense like
    //    `0007654321` doesn't pass.
    Regex::new(r"^(?:\+886|0)(?:9\d{8}|[2-8]\d{7,8}|(?:37|49|82|83|89|26|92)\d{5,7})$")
        .expect("phone regex")
});

fn is_valid_phone(s: &str) -> bool {
    let compact: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '(' && *c != ')')
        .collect();
    PHONE_REGEX.is_match(&compact)
}

// ============================================================
//  車牌 (TW license plates) — multiple formats over the years.
//
//  Modern formats:
//   - 4 字: NNN-NN or AA-NNNN     (pre-2014 motorcycle / car)
//   - 6 字: AAA-NNNN or NNNN-AA   (2014+ cars)
//   - 7 字: AAA-NNNN (post-2018 car). Accepted by all three
//     branches of the regex below.
//
//  We accept the canonical hyphenated form and also the
//  unhyphenated compact form. Letters are uppercase Latin only.
// ============================================================

static PLATE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    // Explicit alternation of the documented formats only — the
    // previous loose ranges accepted unintended shapes like
    // "AB12" or "12AB". Letters {2,3} × digits {2,4} would still
    // catch typos as "valid" plates that no Taiwanese 監理所 ever
    // issued.
    //  - 5-char "4 字": NNN-NN
    //  - 6-char "4 字" cars: AA-NNNN
    //  - 6-char "6 字" 2014+ cars: AAA-NNN  /  NNN-AAA  /  NNNN-AA
    //  - 7-char "7 字" 2018+ cars: AAA-NNNN
    Regex::new(
        r"^(?:\d{3}-?\d{2}|[A-Z]{2}-?\d{4}|[A-Z]{3}-?\d{3}|\d{3}-?[A-Z]{3}|\d{4}-?[A-Z]{2}|[A-Z]{3}-?\d{4})$",
    )
    .expect("plate regex")
});

fn is_valid_license_plate(s: &str) -> bool {
    let upper = s.to_ascii_uppercase();
    PLATE_REGEX.is_match(&upper)
}

// ============================================================
//  LUHN — generic credit-card check. Strips spaces / hyphens.
// ============================================================

fn is_valid_luhn(s: &str) -> bool {
    // Reject any non-digit, non-separator character — silently
    // dropping garbage would let "4111-1111-FOO-1111" claim
    // validity on its remaining 12 digits.
    let mut digits: Vec<u32> = Vec::with_capacity(s.len());
    for c in s.chars() {
        if c.is_whitespace() || c == '-' {
            continue;
        }
        match c.to_digit(10) {
            Some(d) => digits.push(d),
            None => return false,
        }
    }
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(idx, d)| {
            if idx % 2 == 1 {
                let dbl = d * 2;
                if dbl > 9 { dbl - 9 } else { dbl }
            } else {
                *d
            }
        })
        .sum();
    sum % 10 == 0
}

/// Best-effort issuer hint based on the BIN range + length.
/// Returned in the `detail` field of a successful LUHN
/// validation purely as a convenience.
///
/// Mastercard covers two BIN ranges: the classic `51-55` prefix
/// and the 2017+ `2221-2720` 2-series. Both are detected here.
fn credit_card_issuer(s: &str) -> Option<&'static str> {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let first = digits.chars().next()?;
    // Mastercard 2-series check uses the first four digits.
    if digits.len() == 16 {
        if let Ok(prefix4) = digits[..4].parse::<u32>() {
            if (2221..=2720).contains(&prefix4) {
                return Some("Mastercard");
            }
        }
    }
    match first {
        '4' if matches!(digits.len(), 13 | 16 | 19) => Some("Visa"),
        '5' if digits.len() == 16 => {
            // 51-55: classic Mastercard. 50/56-59 hit "Other".
            let prefix2 = digits[..2].parse::<u32>().ok()?;
            if (51..=55).contains(&prefix2) {
                Some("Mastercard")
            } else {
                Some("Unknown issuer")
            }
        }
        '3' => {
            let second = digits.chars().nth(1)?;
            if matches!(second, '4' | '7') && digits.len() == 15 {
                Some("American Express")
            } else if matches!(second, '5') && (14..=16).contains(&digits.len()) {
                Some("JCB")
            } else {
                Some("Diners or other")
            }
        }
        '6' if digits.len() == 16 => Some("Discover or UnionPay"),
        _ => Some("Unknown issuer"),
    }
}

// ============================================================
//  IBAN — ISO 13616 mod-97 check.
//
//  1. Strip spaces.
//  2. Validate length (15-34) and char set [A-Z0-9].
//  3. Move first 4 chars to the end.
//  4. Replace letters with 10..35 (A=10..Z=35).
//  5. The resulting integer mod 97 must equal 1.
// ============================================================

fn is_valid_iban(s: &str) -> bool {
    let compact: String = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    if !compact.chars().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    // Country code (first 2 chars) must be letters.
    if !compact.chars().take(2).all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    let rearranged: String = format!("{}{}", &compact[4..], &compact[..4]);
    // Convert to a digit-only string by expanding letters via
    // A=10..Z=35, then stream mod-97 digit-by-digit (each step
    // is `(remainder * 10 + d) % 97`). Streaming avoids bignum
    // and keeps the working value < 97 * 10 + 9 < u64::MAX, so
    // a `u64` accumulator is more than enough.
    let numeric: String = rearranged
        .chars()
        .flat_map(|c| {
            if c.is_ascii_digit() {
                format!("{c}")
            } else {
                // A=10 ... Z=35
                let v = (c as u8 - b'A') + 10;
                format!("{v}")
            }
            .chars()
            .collect::<Vec<_>>()
        })
        .collect();
    let mut remainder: u64 = 0;
    for ch in numeric.chars() {
        let d = ch.to_digit(10).expect("digit-only chunk");
        remainder = (remainder * 10 + u64::from(d)) % 97;
    }
    remainder == 1
}

// ============================================================
//  IATA airport codes — 3-letter lookup against a v0.1 subset.
// ============================================================

const IATA_AIRPORTS: &[(&str, &str)] = &[
    ("TPE", "Taipei Taoyuan International"),
    ("TSA", "Taipei Songshan"),
    ("KHH", "Kaohsiung International"),
    ("RMQ", "Taichung International"),
    ("TTT", "Taitung"),
    ("HUN", "Hualien"),
    ("MZG", "Penghu Magong"),
    ("KNH", "Kinmen"),
    ("LZN", "Matsu Nangan"),
    ("NRT", "Tokyo Narita"),
    ("HND", "Tokyo Haneda"),
    ("KIX", "Osaka Kansai"),
    ("ICN", "Seoul Incheon"),
    ("HKG", "Hong Kong International"),
    ("SIN", "Singapore Changi"),
    ("BKK", "Bangkok Suvarnabhumi"),
    ("PEK", "Beijing Capital"),
    ("PVG", "Shanghai Pudong"),
    ("LAX", "Los Angeles International"),
    ("JFK", "New York John F. Kennedy"),
    ("SFO", "San Francisco International"),
    ("LHR", "London Heathrow"),
    ("CDG", "Paris Charles de Gaulle"),
    ("FRA", "Frankfurt am Main"),
    ("SYD", "Sydney Kingsford Smith"),
];

fn lookup_iata(s: &str) -> Option<&'static str> {
    let upper = s.trim().to_ascii_uppercase();
    if upper.len() != 3 {
        return None;
    }
    IATA_AIRPORTS
        .iter()
        .find(|(code, _)| *code == upper)
        .map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(kind: FormatKind, value: &str, expected_valid: bool) {
        let r = validate(kind, value);
        assert_eq!(r.valid, expected_valid, "{kind:?} {value:?}");
        assert_eq!(r.kind, kind);
    }

    #[test]
    fn invoice_valid_examples() {
        // Known-good per 財政部 sample: 「AA-12345678」 → 12345675 is
        // a valid checksum on the synthetic case. Compute a real
        // one rather than hard-coding a fixture.
        // Build the digits, find the check digit that satisfies the
        // mod-10 rule.
        for candidate in 0..=9 {
            let s = format!("1234567{candidate}");
            if is_valid_invoice(&s) {
                check(FormatKind::Invoice, &s, true);
                return;
            }
        }
        panic!("expected at least one valid invoice number in 12345670..12345679");
    }

    /// R2 fix (R3 refined): lock the 7th-digit-is-7 alternate-
    /// checksum branch per the 財政部 公報. Hard-coded fixture
    /// "00000079" (verified by hand):
    ///   - digits = [0,0,0,0,0,0,7,9]
    ///   - weights = [1,2,1,2,1,2,4,1]
    ///   - products = [0,0,0,0,0,0,28,9]
    ///   - digit-sums = [0,0,0,0,0,0,10,9] → sum = 19
    ///   - 19 mod 10 = 9 → *standard path fails*
    ///   - (19 + 1) mod 10 = 0 → *alternate path passes*, and
    ///     digit[6] = 7 so the alternate branch is reachable.
    ///
    /// No brute-force search at test time — the fixture is
    /// deterministic.
    #[test]
    fn invoice_seventh_digit_seven_alternate_path() {
        let s = "00000079";
        check(FormatKind::Invoice, s, true);
        // Sanity: standard path alone rejects it.
        let digits: Vec<u32> = s.chars().map(|c| c.to_digit(10).unwrap()).collect();
        assert!(!invoice_check(&digits, 0), "standard check should fail");
        assert!(invoice_check(&digits, 1), "alternate check should pass");
        assert_eq!(digits[6], 7, "7th digit must be 7 to reach the branch");
    }

    #[test]
    fn invoice_rejects_wrong_length() {
        check(FormatKind::Invoice, "1234567", false);
        check(FormatKind::Invoice, "123456789", false);
    }

    #[test]
    fn invoice_rejects_non_digits() {
        check(FormatKind::Invoice, "1234567A", false);
    }

    #[test]
    fn taipower_format_check() {
        check(FormatKind::Taipower, "12345678901", true);
        check(FormatKind::Taipower, "1234567890", false);
        check(FormatKind::Taipower, "1234567890A", false);
    }

    #[test]
    fn water_meter_accepts_hyphens() {
        check(FormatKind::WaterMeter, "12345678901", true);
        check(FormatKind::WaterMeter, "123-456-78901", true);
        check(FormatKind::WaterMeter, "12345", false);
    }

    #[test]
    fn phone_mobile_format() {
        check(FormatKind::Phone, "0912-345-678", true);
        check(FormatKind::Phone, "0912345678", true);
        check(FormatKind::Phone, "+886912345678", true);
    }

    #[test]
    fn phone_landline_format() {
        check(FormatKind::Phone, "02-12345678", true);
        check(FormatKind::Phone, "07-1234567", true);
        check(FormatKind::Phone, "08-12345678", true);
    }

    #[test]
    fn phone_rejects_garbage() {
        check(FormatKind::Phone, "abc", false);
        check(FormatKind::Phone, "0000000000", false); // leading 00, no valid area code
    }

    #[test]
    fn license_plate_modern_format() {
        check(FormatKind::LicensePlate, "ABC-1234", true);
        check(FormatKind::LicensePlate, "abc-1234", true);
        check(FormatKind::LicensePlate, "ABC1234", true);
    }

    #[test]
    fn license_plate_rejects_garbage() {
        check(FormatKind::LicensePlate, "X", false);
        check(FormatKind::LicensePlate, "ABCDEFG", false);
    }

    #[test]
    fn credit_card_luhn_known_valid() {
        // 4111 1111 1111 1111 is the canonical Visa test PAN.
        check(FormatKind::CreditCard, "4111 1111 1111 1111", true);
        check(FormatKind::CreditCard, "4111-1111-1111-1111", true);
    }

    #[test]
    fn credit_card_luhn_known_invalid() {
        check(FormatKind::CreditCard, "4111 1111 1111 1112", false);
    }

    #[test]
    fn credit_card_issuer_hint_in_detail() {
        let r = validate(FormatKind::CreditCard, "4111111111111111");
        assert!(r.valid);
        assert_eq!(r.detail.as_deref(), Some("Visa"));
    }

    /// R2 fix: Mastercard 2-series (BIN 2221-2720). 2223 0000 0000
    /// 0007 is the canonical test PAN from the Mastercard
    /// developer docs.
    #[test]
    fn credit_card_mastercard_2_series_detection() {
        let r = validate(FormatKind::CreditCard, "2223000000000007");
        assert!(r.valid, "2-series Mastercard PAN should pass LUHN");
        assert_eq!(
            r.detail.as_deref(),
            Some("Mastercard"),
            "BIN 2223 must resolve to Mastercard (2-series 2221-2720)",
        );
    }

    /// A 5-series number that's NOT in the 51-55 classic range
    /// should not get the Mastercard label.
    #[test]
    fn credit_card_5_prefix_outside_classic_range_reports_unknown() {
        // 5095... is a valid 16-digit LUHN built by hand: walk
        // candidates until one passes.
        let base = "5095000000000000";
        let mut found = None;
        for last in 0..10 {
            let mut s = base.to_string();
            s.pop();
            s.push_str(&last.to_string());
            if is_valid_luhn(&s) {
                found = Some(s);
                break;
            }
        }
        let s = found.expect("expected one luhn-valid candidate in 50950...");
        let r = validate(FormatKind::CreditCard, &s);
        assert!(r.valid);
        assert_eq!(r.detail.as_deref(), Some("Unknown issuer"));
    }

    #[test]
    fn iban_known_valid() {
        // ISO 13616 published example: GB82 WEST 1234 5698 7654 32.
        check(FormatKind::Iban, "GB82WEST12345698765432", true);
        check(FormatKind::Iban, "GB82 WEST 1234 5698 7654 32", true);
    }

    #[test]
    fn iban_known_invalid() {
        check(FormatKind::Iban, "GB82WEST12345698765433", false);
    }

    #[test]
    fn iban_rejects_short_or_long() {
        check(FormatKind::Iban, "GB82", false);
        check(FormatKind::Iban, &"A".repeat(40), false);
    }

    #[test]
    fn iata_lookup_tpe() {
        let r = validate(FormatKind::IataAirport, "TPE");
        assert!(r.valid);
        assert_eq!(r.detail.as_deref(), Some("Taipei Taoyuan International"));
    }

    #[test]
    fn iata_lookup_case_insensitive() {
        let r = validate(FormatKind::IataAirport, "khh");
        assert!(r.valid);
        assert_eq!(r.detail.as_deref(), Some("Kaohsiung International"));
    }

    #[test]
    fn iata_lookup_unknown() {
        let r = validate(FormatKind::IataAirport, "ZZZ");
        assert!(!r.valid);
        assert_eq!(r.detail, None);
    }

    #[test]
    fn iata_lookup_rejects_wrong_length() {
        let r = validate(FormatKind::IataAirport, "TP");
        assert!(!r.valid);
    }

    #[test]
    fn format_kind_from_wire_round_trip() {
        for k in FormatKind::ALL {
            assert_eq!(FormatKind::from_wire(k.as_str()), Some(k));
        }
        assert_eq!(FormatKind::from_wire("nope"), None);
    }
}
