//! Taiwan national-ID validator covering three formats that share the
//! same 10-character shape `L D D D D D D D D D` and a single checksum
//! algorithm:
//!
//! 1. **Citizen 身分證**: `L g d d d d d d d c` where `g` ∈ {`1`, `2`}.
//! 2. **Modern foreigner 統一證號 / 居留證 (2021+)**: `L g d d d d d d d c`
//!    where `g` ∈ {`8`, `9`}. MOI unified the format so a single
//!    checksum implementation covers both citizens and foreigners.
//! 3. **Legacy 2-letter 統一證號 / 居留證 (pre-2021)**: `L L d d d d d d d d`.
//!    The legacy format uses a different (and historically
//!    underspecified) checksum and is being phased out as old cards
//!    expire. We currently *recognize* the shape and report it as
//!    `legacy_resident`, but flag the result as invalid pending a
//!    follow-up to wire the legacy checksum once we have an
//!    authoritative test-vector source. See the TODO at the bottom of
//!    this file.
//!
//! References:
//! - 內政部戶政司「國民身分證統一編號」檢核規則
//! - 移民署「外來人口統一證號」配賦原則 (2021-01-02 啟用)

use serde::Serialize;

/// The detected ID category. Kept as a string-typed enum so the MCP
/// JSON payload renders cleanly without an adapter layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NationalIdKind {
    /// 本國人身分證 — second char is `1` (male) or `2` (female).
    Citizen,
    /// Modern unified 統一證號 / 居留證 (2021+) — second char is
    /// `8` (male) or `9` (female). Same checksum as `Citizen`.
    Resident,
    /// Legacy 2-letter 統一證號 / 居留證. Shape recognized; checksum
    /// validation not yet implemented (see module docs).
    LegacyResident,
}

impl NationalIdKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Citizen => "citizen",
            Self::Resident => "resident",
            Self::LegacyResident => "legacy_resident",
        }
    }
}

/// Inferred gender from the second character.
///
/// `Unknown` only appears for [`NationalIdKind::LegacyResident`], whose
/// second-letter encoding we don't decode here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Gender {
    Male,
    Female,
    Unknown,
}

/// Structured result of a successful (or "format-matched") parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedNationalId {
    /// Canonical upper-case form of the input.
    pub canonical: String,
    pub kind: NationalIdKind,
    pub gender: Gender,
    /// Two-letter county code derived from the first character —
    /// `"TPE"` for Taipei City, `"NTC"` for New Taipei, etc. `None`
    /// when the first letter isn't in the standard table (only Z is
    /// reserved as "其他 / 未填" so this stays `Some` for all valid
    /// letters).
    pub county_code: Option<&'static str>,
}

/// Top-level validator. Returns `(valid, Some(parsed))` for any input
/// matching one of the three formats; `valid` is false when the shape
/// matches but the checksum fails (or when shape matches legacy 2-letter
/// — see module docs). Returns `(false, None)` for anything that
/// doesn't even match a known shape.
#[must_use]
pub fn validate(input: &str) -> (bool, Option<ParsedNationalId>) {
    let canonical = canonicalize(input);
    let bytes = canonical.as_bytes();
    if bytes.len() != 10 {
        return (false, None);
    }

    let c0 = bytes[0] as char;
    let c1 = bytes[1] as char;

    // Position 0 must always be A-Z.
    if !c0.is_ascii_uppercase() {
        return (false, None);
    }

    if c1.is_ascii_digit() {
        // Modern format: L g d d d d d d d c
        if !bytes[2..].iter().all(u8::is_ascii_digit) {
            return (false, None);
        }
        let kind = match c1 {
            '1' | '2' => NationalIdKind::Citizen,
            '8' | '9' => NationalIdKind::Resident,
            _ => return (false, None),
        };
        let gender = match c1 {
            '1' | '8' => Gender::Male,
            '2' | '9' => Gender::Female,
            _ => unreachable!(),
        };
        let valid = verify_modern_checksum(bytes);
        let parsed = ParsedNationalId {
            canonical: canonical.clone(),
            kind,
            gender,
            county_code: county_code_for(c0),
        };
        (valid, Some(parsed))
    } else if c1.is_ascii_uppercase() {
        // Legacy 2-letter format: L L d d d d d d d d
        if !bytes[2..].iter().all(u8::is_ascii_digit) {
            return (false, None);
        }
        // Shape recognized but legacy checksum not implemented. We
        // surface the parse so callers can present a meaningful "this
        // looks like a legacy resident ID — verify against MOI for
        // checksum" message instead of a bare `unknown`.
        let parsed = ParsedNationalId {
            canonical: canonical.clone(),
            kind: NationalIdKind::LegacyResident,
            gender: Gender::Unknown,
            county_code: county_code_for(c0),
        };
        (false, Some(parsed))
    } else {
        (false, None)
    }
}

/// Trim ASCII whitespace and uppercase letters in place. The hot path
/// for trusted callers is a no-op (input already canonical) — this
/// allocates once on the slow path.
fn canonicalize(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.bytes().all(|b| !b.is_ascii_lowercase()) {
        trimmed.to_string()
    } else {
        trimmed.to_ascii_uppercase()
    }
}

/// Modern-format checksum:
/// - Letter contributes a 2-digit county code (e.g. A→10).
/// - `weighted_sum` = `code_tens` × 1 + `code_units` × 9
///   + d1 × 8 + d2 × 7 + d3 × 6 + d4 × 5 + d5 × 4 + d6 × 3 + d7 × 2 + d8 × 1
///   + `check_digit` × 1
/// - valid iff `weighted_sum mod 10 == 0`.
fn verify_modern_checksum(bytes: &[u8]) -> bool {
    // Weights for the 9 numeric positions (8 body digits + check digit).
    // Declared as a const so the indexing reads obviously and there's
    // no `usize as u32` cast in the hot loop.
    const WEIGHTS: [u32; 9] = [8, 7, 6, 5, 4, 3, 2, 1, 1];

    let Some(code) = letter_to_code(bytes[0] as char) else {
        return false;
    };
    let mut sum = (code / 10) + (code % 10) * 9;
    for (b, w) in bytes[1..10].iter().zip(WEIGHTS.iter()) {
        sum += u32::from(*b - b'0') * w;
    }
    sum % 10 == 0
}

/// MOI letter-to-county-code mapping for the first character of a
/// national ID. Values are the 2-digit codes used in the checksum.
#[allow(clippy::match_same_arms)] // explicit per-letter rows aid auditability
fn letter_to_code(c: char) -> Option<u32> {
    Some(match c {
        'A' => 10,
        'B' => 11,
        'C' => 12,
        'D' => 13,
        'E' => 14,
        'F' => 15,
        'G' => 16,
        'H' => 17,
        'I' => 34, // Chiayi City — non-sequential per MOI table
        'J' => 18,
        'K' => 19,
        'L' => 20,
        'M' => 21,
        'N' => 22,
        'O' => 35, // Hsinchu City — non-sequential
        'P' => 23,
        'Q' => 24,
        'R' => 25,
        'S' => 26,
        'T' => 27,
        'U' => 28,
        'V' => 29,
        'W' => 32, // Kinmen — non-sequential (skips 30, 31)
        'X' => 30,
        'Y' => 31,
        'Z' => 33,
        _ => return None,
    })
}

/// Short county code mnemonic for the first letter. Maps to the
/// English short form used by `pyethonics`-style libraries so external
/// callers get a stable string. Returns `None` only for letters
/// outside the table (which never happens once `letter_to_code`
/// succeeds, since the two tables share keys).
fn county_code_for(c: char) -> Option<&'static str> {
    Some(match c {
        'A' => "TPE",
        'B' => "TXG-OLD",
        'C' => "KEL",
        'D' => "TNN-OLD",
        'E' => "KHH-OLD",
        'F' => "NTC",
        'G' => "ILA",
        'H' => "TAO",
        'I' => "CYI",
        'J' => "HSQ",
        'K' => "MIA",
        'L' => "TXQ-OLD",
        'M' => "NAN",
        'N' => "CHA",
        'O' => "HSZ",
        'P' => "YUN",
        'Q' => "CYQ",
        'R' => "TNQ-OLD",
        'S' => "KHQ-OLD",
        'T' => "PIF",
        'U' => "HUA",
        'V' => "TTT",
        'W' => "KIN",
        'X' => "PEN",
        'Y' => "LIE",
        'Z' => "OTH",
        _ => return None,
    })
}

// TODO(#3.9 follow-up): wire the legacy 2-letter checksum once we have
// an authoritative test-vector source from MOI. The community-
// documented algorithm (code1_tens*1 + code1_units*9 + code2_units*8
// + d1*7 + ... + d7*1 + d8*1) is plausible but unverified — shipping
// an unverified checksum risks false-rejecting legitimate cards still
// in circulation.

#[cfg(test)]
mod tests {
    use super::*;

    /// A123456789 — the canonical valid test ID. Widely used in
    /// gov/community examples; not assigned to any real person.
    #[test]
    fn citizen_a123456789_is_valid() {
        let (ok, parsed) = validate("A123456789");
        assert!(ok);
        let p = parsed.unwrap();
        assert_eq!(p.canonical, "A123456789");
        assert_eq!(p.kind, NationalIdKind::Citizen);
        assert_eq!(p.gender, Gender::Male);
        assert_eq!(p.county_code, Some("TPE"));
    }

    #[test]
    fn citizen_female_variant_is_valid() {
        // Computed: A2 + 23456789 → adjust last digit so sum mod 10 == 0
        // letter A → 10 → 1*1 + 0*9 = 1
        // 2*8 + 2*7 + 3*6 + 4*5 + 5*4 + 6*3 + 7*2 + 8*1 = 16+14+18+20+20+18+14+8 = 128
        // 1 + 128 = 129; need c such that (129 + c) mod 10 == 0 → c = 1
        let (ok, parsed) = validate("A223456781");
        assert!(ok, "expected valid; sum check: see comment in test");
        assert_eq!(parsed.unwrap().gender, Gender::Female);
    }

    #[test]
    fn citizen_wrong_check_digit_is_invalid_but_recognized() {
        let (ok, parsed) = validate("A123456788");
        assert!(!ok);
        let p = parsed.unwrap();
        assert_eq!(p.kind, NationalIdKind::Citizen);
    }

    #[test]
    fn lowercase_first_letter_is_canonicalized() {
        let (ok, parsed) = validate("a123456789");
        assert!(ok);
        assert_eq!(parsed.unwrap().canonical, "A123456789");
    }

    #[test]
    fn whitespace_is_stripped_before_validation() {
        let (ok, _) = validate("  A123456789  ");
        assert!(ok);
    }

    #[test]
    fn too_short_returns_none() {
        let (ok, parsed) = validate("A12345678");
        assert!(!ok);
        assert!(parsed.is_none());
    }

    #[test]
    fn too_long_returns_none() {
        let (ok, parsed) = validate("A1234567890");
        assert!(!ok);
        assert!(parsed.is_none());
    }

    #[test]
    fn second_char_3_through_7_is_rejected_as_unknown_shape() {
        // 3/4/5/6/7 in position 2 are not assigned to citizen, resident,
        // or legacy formats — these should not even be recognized.
        for d in ['3', '4', '5', '6', '7'] {
            let candidate = format!("A{d}23456789");
            let (ok, parsed) = validate(&candidate);
            assert!(!ok, "{candidate} should be invalid");
            assert!(parsed.is_none(), "{candidate} should not match any shape");
        }
    }

    #[test]
    fn empty_string_returns_none() {
        let (ok, parsed) = validate("");
        assert!(!ok);
        assert!(parsed.is_none());
    }

    #[test]
    fn special_chars_return_none() {
        let (ok, parsed) = validate("A12-456789");
        assert!(!ok);
        assert!(parsed.is_none());
    }

    #[test]
    fn modern_resident_male_with_known_letter_validates() {
        // Resident format: L 8 d d d d d d d c.
        // Letter A → 10 → 1*1 + 0*9 = 1
        // 8*8 + 1*7 + 2*6 + 3*5 + 4*4 + 5*3 + 6*2 + 7*1 = 64+7+12+15+16+15+12+7 = 148
        // 1 + 148 = 149; need c such that (149 + c) mod 10 == 0 → c = 1
        let (ok, parsed) = validate("A812345671");
        assert!(ok);
        let p = parsed.unwrap();
        assert_eq!(p.kind, NationalIdKind::Resident);
        assert_eq!(p.gender, Gender::Male);
    }

    #[test]
    fn modern_resident_female_validates() {
        // L 9 d d d d d d d c — same algorithm, different gender bit.
        // Letter A → 1
        // 9*8 + 1*7 + 2*6 + 3*5 + 4*4 + 5*3 + 6*2 + 7*1 = 72+7+12+15+16+15+12+7 = 156
        // 1 + 156 = 157; c = 3
        let (ok, parsed) = validate("A912345673");
        assert!(ok);
        let p = parsed.unwrap();
        assert_eq!(p.kind, NationalIdKind::Resident);
        assert_eq!(p.gender, Gender::Female);
    }

    #[test]
    fn legacy_2_letter_format_is_recognized_but_not_validated() {
        // Shape matches "L L d d d d d d d d" but we don't implement
        // the legacy checksum — return kind so the caller can suggest
        // re-issuance, but mark invalid.
        let (ok, parsed) = validate("AB12345678");
        assert!(!ok, "legacy checksum not yet implemented");
        let p = parsed.unwrap();
        assert_eq!(p.kind, NationalIdKind::LegacyResident);
        assert_eq!(p.gender, Gender::Unknown);
    }

    #[test]
    fn letter_with_non_sequential_code_validates_correctly() {
        // Letter I → code 34 (skips 30-33 region per MOI table).
        // 3*1 + 4*9 = 39; pick gender 1 male.
        // 1*8 + 0*7 + 0*6 + 0*5 + 0*4 + 0*3 + 0*2 + 0*1 = 8
        // 39 + 8 = 47; check digit c = 3 (47 + 3 = 50, mod 10 = 0)
        let (ok, parsed) = validate("I100000003");
        assert!(ok, "letter I → code 34 path");
        assert_eq!(parsed.unwrap().county_code, Some("CYI"));
    }

    #[test]
    fn letter_z_validates_special_other_county() {
        // Letter Z → code 33.
        // 3*1 + 3*9 = 30
        // 1*8 + 0*7 + 0*6 + 0*5 + 0*4 + 0*3 + 0*2 + 0*1 = 8
        // 30 + 8 = 38; check digit c = 2 (38 + 2 = 40)
        let (ok, parsed) = validate("Z100000002");
        assert!(ok);
        assert_eq!(parsed.unwrap().county_code, Some("OTH"));
    }

    #[test]
    fn all_letters_produce_a_code_when_alphabetic() {
        // Sanity: every uppercase letter A-Z maps to a code. If any
        // entry is missing from `letter_to_code` we get a `None`
        // return and the validator rejects the prefix even when the
        // numeric portion is well-formed.
        for letter in 'A'..='Z' {
            assert!(letter_to_code(letter).is_some(), "missing: {letter}");
            assert!(county_code_for(letter).is_some(), "missing: {letter}");
        }
    }
}
