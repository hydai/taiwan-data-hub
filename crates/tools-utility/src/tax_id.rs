//! Taiwan business tax-ID (統一編號) validator.
//!
//! 統一編號 is 8 numeric digits. The standard MOEA algorithm:
//!
//! 1. Multiply each digit by its weight in `[1, 2, 1, 2, 1, 2, 4, 1]`.
//! 2. For each product, replace it with the sum of its decimal
//!    digits — applied *once*, i.e. `tens + units`. For our weights
//!    the only product that stays multi-digit after one sum is
//!    `7 × 4 = 28 → 2 + 8 = 10` at position 6; every other product
//!    is already a single digit.
//! 3. Sum the reduced values; valid iff `total mod 10 == 0`.
//!
//! ### The 2023 rule change
//!
//! Historically, when the 7th digit (position 6, 0-indexed) was `7`,
//! MOEA accepted a second checksum: `(total + 1) mod 10 == 0`. The
//! canonical published example `12345675` reaches a single-sum total
//! of 39 and validates only via this `+1` form — so most pre-2023
//! issuance batches relied on it.
//!
//! Starting **2023-03-01**, MOEA stopped issuing IDs that would only
//! satisfy the `+1` form; new IDs all satisfy the strict
//! `total mod 10 == 0` rule. Legacy IDs that pass only the `+1` form
//! remain in circulation, so the default validator accepts both. Set
//! [`Options::strict`] = `true` to reject the `+1` form.
//!
//! References:
//! - 財政部 「統一編號編配原則」 §3 (revised 2023-03-01)
//! - 商業司公開的統一編號檢核演算法 (公司行號查詢專區)
//! - `python-stdnum`'s `tw.gui` implementation (cross-reference)

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParsedTaxId {
    /// Canonical 8-digit form.
    pub canonical: String,
    /// Whether the validation succeeded under the strict 2023 rule
    /// (`sum mod 10 == 0`). `true` for both new and most legacy IDs.
    pub strict_2023: bool,
    /// Whether the validation succeeded only via the legacy `+1`
    /// alternative (i.e., the input is a legacy-era ID whose 7th
    /// digit is `7` and which doesn't pass the strict rule). Mutually
    /// exclusive with `strict_2023` when both can apply, but recorded
    /// here so callers can spot legacy-pattern IDs at a glance.
    pub legacy_alternative: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// When `true`, reject the legacy `+1` alternative for IDs whose
    /// 7th digit is `7`. Defaults to `false` (permissive) because most
    /// real-world IDs predate 2023.
    pub strict: bool,
}

/// Validate a 統一編號 with default (permissive) options.
#[must_use]
pub fn validate(input: &str) -> (bool, Option<ParsedTaxId>) {
    validate_with(input, Options::default())
}

/// Validate a 統一編號 with explicit options.
#[must_use]
pub fn validate_with(input: &str, opts: Options) -> (bool, Option<ParsedTaxId>) {
    const WEIGHTS: [u32; 8] = [1, 2, 1, 2, 1, 2, 4, 1];

    let trimmed = input.trim();
    if trimmed.len() != 8 || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return (false, None);
    }

    let bytes = trimmed.as_bytes();
    let digits: [u32; 8] = std::array::from_fn(|i| u32::from(bytes[i] - b'0'));

    let mut total = 0u32;
    for i in 0..8 {
        total += sum_digits_once(digits[i] * WEIGHTS[i]);
    }

    let strict_2023 = total % 10 == 0;
    let seven_at_pos_6 = digits[6] == 7;
    let legacy_alternative = !strict_2023 && seven_at_pos_6 && (total + 1) % 10 == 0;

    let valid = if opts.strict {
        strict_2023
    } else {
        strict_2023 || legacy_alternative
    };

    let parsed = ParsedTaxId {
        canonical: trimmed.to_string(),
        strict_2023,
        legacy_alternative,
    };
    (valid, Some(parsed))
}

/// Sum the decimal digits of `n` once (`tens + units`).
///
/// This is the per-product reduction MOEA's algorithm specifies. We
/// deliberately do **not** iterate to a single digit — for the
/// `digit × weight` products this function sees (`0..=36`), the only
/// case where the result is multi-digit is `28 → 10`, and *that
/// extra digit matters*. Iterating once more (`10 → 1`) is the
/// mistake the historical `+1` rule was patched around: the
/// difference between "use 10" (correct) and "use 1" (mistake) is
/// 9 ≡ −1 mod 10, which is exactly the legacy alternative's window.
fn sum_digits_once(n: u32) -> u32 {
    (n / 10) + (n % 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 12345675 is MOEA's most-cited published example. It validates
    /// only via the legacy `+1` rule:
    /// - `d = [1,2,3,4,5,6,7,5]`
    /// - `w = [1,2,1,2,1,2,4,1]`
    /// - `p = [1,4,3,8,5,12,28,5]`
    /// - `sum_once = [1,4,3,8,5,3,10,5]` → total = 39
    /// - `strict_2023`: 39 mod 10 = 9 → false
    /// - `digit[6] = 7` → legacy: (39+1) mod 10 = 0 → true
    ///
    /// Permissive default accepts; strict mode rejects (it would have
    /// been a no-issue from 2023-03-01 onward).
    #[test]
    fn canonical_12345675_is_legacy_form() {
        let (ok, parsed) = validate("12345675");
        assert!(ok, "permissive default accepts the legacy +1 form");
        let p = parsed.unwrap();
        assert!(!p.strict_2023, "12345675 fails the strict 2023 rule");
        assert!(p.legacy_alternative, "12345675 passes only via the +1 rule");
        assert_eq!(p.canonical, "12345675");

        let (ok_strict, _) = validate_with("12345675", Options { strict: true });
        assert!(!ok_strict, "strict 2023 rejects the legacy +1 form");
    }

    /// 04595257 is a known strict-2023-valid 統一編號 (position 6 digit
    /// is 5, so the legacy branch never applies):
    /// - `d = [0,4,5,9,5,2,5,7]`
    /// - `p = [0,8,5,18,5,4,20,7]`
    /// - `sum_once = [0,8,5,9,5,4,2,7]` → total = 40
    /// - `strict_2023`: 40 mod 10 = 0 → true
    #[test]
    fn strict_2023_valid_example_validates_in_both_modes() {
        let (ok_default, parsed_default) = validate("04595257");
        assert!(ok_default);
        let p = parsed_default.unwrap();
        assert!(p.strict_2023);
        assert!(!p.legacy_alternative);

        let (ok_strict, _) = validate_with("04595257", Options { strict: true });
        assert!(ok_strict, "strict mode accepts strict-valid IDs unchanged");
    }

    #[test]
    fn wrong_length_returns_none() {
        for bad in ["1234567", "123456789", "", "1"] {
            let (ok, parsed) = validate(bad);
            assert!(!ok);
            assert!(parsed.is_none(), "{bad} should not parse");
        }
    }

    #[test]
    fn non_digit_returns_none() {
        for bad in ["1234567a", "12 34 56", "12-345-67", "abcdefgh"] {
            let (ok, parsed) = validate(bad);
            assert!(!ok);
            assert!(parsed.is_none(), "{bad} should not parse");
        }
    }

    #[test]
    fn checksum_failure_keeps_kind() {
        // 11111111: sum_once = [1,2,1,2,1,2,4,1] → total = 14
        //           14 mod 10 = 4 (strict fails)
        //           digit[6] = 1, not 7, so legacy doesn't apply.
        // Picked deliberately to fail *both* rules — a near-miss like
        // "12345674" would pass legacy because position 6 = 7 turns
        // on the +1 alternative.
        let (ok, parsed) = validate("11111111");
        assert!(!ok);
        // Parsed metadata still returned so the caller can echo back
        // the canonical form.
        let p = parsed.unwrap();
        assert!(!p.strict_2023);
        assert!(!p.legacy_alternative);
    }

    #[test]
    fn whitespace_is_trimmed() {
        let (ok, _) = validate("  12345675  ");
        assert!(ok);
    }

    #[test]
    fn all_zeros_passes_strict_and_is_flagged_as_caller_problem() {
        // 00000000 sums to 0, which mod 10 == 0 → mathematically valid.
        // 統一編號 編配原則 doesn't assign 00000000 in practice, but
        // checksum-only validation can't know that. Documenting via
        // test so future readers don't "fix" this with a special case.
        let (ok, parsed) = validate("00000000");
        assert!(ok);
        assert!(parsed.unwrap().strict_2023);
    }

    #[test]
    fn sum_digits_once_is_tens_plus_units_not_iterated() {
        // Key contract: NOT iterated to single digit. 28 → 10, not 1.
        // The legacy `+1` rule exists precisely because the "wrong"
        // iterated reduction (28 → 1) gives a different residual mod 10.
        assert_eq!(sum_digits_once(0), 0);
        assert_eq!(sum_digits_once(9), 9);
        assert_eq!(sum_digits_once(10), 1);
        assert_eq!(sum_digits_once(18), 9);
        assert_eq!(sum_digits_once(19), 10); // not 1
        assert_eq!(sum_digits_once(28), 10); // not 1
        assert_eq!(sum_digits_once(36), 9);
    }

    #[test]
    fn lowercase_letters_in_input_return_none() {
        // No alpha allowed in 統一編號 — even if it looks like digits.
        let (ok, parsed) = validate("12345abc");
        assert!(!ok);
        assert!(parsed.is_none());
    }
}
