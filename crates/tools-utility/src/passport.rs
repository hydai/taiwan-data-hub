//! Republic of China (Taiwan) passport-number validator.
//!
//! TW passports use a 9-digit number printed on the bio page and
//! encoded in the MRZ. There is **no publicly-published checksum
//! algorithm** for the visible 9-digit number itself — the MRZ check
//! digits use ICAO 9303's standard algorithm against the full MRZ
//! line, not against the passport number in isolation.
//!
//! Therefore this validator does *format-only* checks: 9 ASCII digits
//! after trimming whitespace. We intentionally do *not* enforce a
//! leading-digit rule: while currently issued passports don't begin
//! with `0`, older books and special-purpose passports may, and a
//! false-reject here would block legitimate look-ups.
//!
//! References:
//! - 外交部「中華民國護照」格式說明（公開資料）
//! - ICAO Document 9303, Part 4 (MRZ check digit lives there, not here)

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedPassport {
    /// Canonical 9-digit form.
    pub canonical: String,
}

/// Validate a TW passport number. Returns `(true, Some(parsed))` on
/// matching format. The `valid` bit reflects format-only correctness;
/// callers needing identity binding must consult the issuing
/// authority.
#[must_use]
pub fn validate(input: &str) -> (bool, Option<ParsedPassport>) {
    let trimmed = input.trim();
    if trimmed.len() == 9 && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        (
            true,
            Some(ParsedPassport {
                canonical: trimmed.to_string(),
            }),
        )
    } else {
        (false, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nine_digits_is_valid() {
        let (ok, parsed) = validate("123456789");
        assert!(ok);
        assert_eq!(parsed.unwrap().canonical, "123456789");
    }

    #[test]
    fn nine_digits_with_leading_zero_is_valid_per_module_doc() {
        // Documented permissive choice — older books may have started
        // with 0; we don't false-reject.
        let (ok, _) = validate("012345678");
        assert!(ok);
    }

    #[test]
    fn eight_digits_is_invalid() {
        let (ok, parsed) = validate("12345678");
        assert!(!ok);
        assert!(parsed.is_none());
    }

    #[test]
    fn ten_digits_is_invalid() {
        let (ok, _) = validate("1234567890");
        assert!(!ok);
    }

    #[test]
    fn whitespace_is_trimmed() {
        let (ok, _) = validate("  123456789  ");
        assert!(ok);
    }

    #[test]
    fn letters_are_rejected() {
        for bad in ["12345678A", "A12345678", "ABCDEFGHI"] {
            let (ok, _) = validate(bad);
            assert!(!ok, "{bad} should reject");
        }
    }

    #[test]
    fn empty_is_invalid() {
        let (ok, parsed) = validate("");
        assert!(!ok);
        assert!(parsed.is_none());
    }

    #[test]
    fn internal_whitespace_is_rejected() {
        let (ok, _) = validate("123 456 789");
        assert!(!ok);
    }
}
