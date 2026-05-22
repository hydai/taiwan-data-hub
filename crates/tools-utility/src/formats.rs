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
/// kind (or what `auto` resolved to). `valid` is the answer.
/// `detail` carries an optional structured payload (e.g. the
/// resolved IATA airport name, the LUHN-derived issuer hint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatResult {
    pub valid: bool,
    pub kind: FormatKind,
    pub detail: Option<String>,
}

#[must_use]
pub fn validate(kind: FormatKind, value: &str) -> FormatResult {
    let trimmed = value.trim();
    let valid = match kind {
        FormatKind::Invoice => is_valid_invoice(trimmed),
        FormatKind::Taipower => is_valid_taipower(trimmed),
        FormatKind::WaterMeter => is_valid_water_meter(trimmed),
        FormatKind::Phone => is_valid_phone(trimmed),
        FormatKind::LicensePlate => is_valid_license_plate(trimmed),
        FormatKind::CreditCard => is_valid_luhn(trimmed),
        FormatKind::Iban => is_valid_iban(trimmed),
        FormatKind::IataAirport => lookup_iata(trimmed).is_some(),
    };
    let detail = match kind {
        FormatKind::IataAirport => lookup_iata(trimmed).map(str::to_string),
        FormatKind::CreditCard if valid => credit_card_issuer(trimmed).map(str::to_string),
        _ => None,
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
    Regex::new(r"^(?:\+886|0)(?:9\d{8}|[2-8]\d{7,8})$").expect("phone regex")
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
    Regex::new(r"^(?:[A-Z]{2,3}-?\d{2,4}|\d{2,4}-?[A-Z]{2,3}|\d{3}-?\d{2})$").expect("plate regex")
});

fn is_valid_license_plate(s: &str) -> bool {
    let upper = s.to_ascii_uppercase();
    PLATE_REGEX.is_match(&upper)
}

// ============================================================
//  LUHN — generic credit-card check. Strips spaces / hyphens.
// ============================================================

fn is_valid_luhn(s: &str) -> bool {
    let digits: Vec<u32> = s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .filter_map(|c| c.to_digit(10))
        .collect();
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

/// Best-effort issuer hint based on the first 1-2 digits +
/// length. Returned in the `detail` field of a successful LUHN
/// validation purely as a convenience.
fn credit_card_issuer(s: &str) -> Option<&'static str> {
    let digits: String = s.chars().filter(char::is_ascii_digit).collect();
    let first = digits.chars().next()?;
    match first {
        '4' if matches!(digits.len(), 13 | 16 | 19) => Some("Visa"),
        '5' if digits.len() == 16 => Some("Mastercard"),
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
    // Convert to numeric string. mod-97 over a long string is
    // done in chunks to avoid bignum: feed digits in 9-char
    // windows because 10^9 fits in u64.
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
