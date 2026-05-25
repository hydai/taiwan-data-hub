//! Pure Big5 ↔ UTF-8 transcoding helpers shared by the
//! `transcode_big5_utf8` MCP tool.
//!
//! Taiwan government datasets are commonly authored in Big5
//! (or HKSCS-augmented Big5) — many CSV / TSV files served from
//! `data.gov.tw` only ship in that legacy encoding. Modern
//! tooling expects UTF-8, so the gateway exposes a transcoder
//! both ways.
//!
//! `encoding_rs` is Mozilla's implementation of the WHATWG
//! Encoding Standard (the same code Firefox uses); we use the
//! "Big5" label, which under WHATWG aliases the legacy + HKSCS
//! supplementary set. That choice maximises round-trip coverage
//! of real-world Taiwan-origin files at the cost of accepting a
//! few HKSCS code points the strict ROCA-Big5 reference would
//! reject — a reasonable trade because the alternative is
//! silently failing on the broader corpus.
//!
//! Both functions report `had_replacements: true` when the
//! encoder substituted a numeric character reference for an
//! unmappable code point (UTF-8 → Big5) or `U+FFFD` for an
//! invalid byte sequence (Big5 → UTF-8). The tool wrapper
//! surfaces this flag so callers can tell a lossy round-trip
//! from a clean one without diffing the result byte-for-byte.

use encoding_rs::BIG5;

/// Outcome of a transcoding operation. `had_replacements` is the
/// signal callers care most about — `false` means the input
/// fully round-trips, `true` means the encoder swapped in
/// substitutes (NCRs for UTF-8 → Big5; `U+FFFD` for Big5 → UTF-8)
/// and the result is information-lossy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeResult<T> {
    pub output: T,
    pub had_replacements: bool,
}

/// Decode Big5 bytes into a UTF-8 `String`. Invalid byte
/// sequences are replaced with `U+FFFD` (the standard "REPLACEMENT
/// CHARACTER") and `had_replacements` is set so the caller can
/// flag the lossy decode without scanning the output.
///
/// `encoding_rs::Decoder` would let us stream this in chunks, but
/// the MCP tool ingests a single in-memory payload — capped at
/// 4 MiB at the wrapper boundary — so the one-shot `decode`
/// helper is the right shape here.
#[must_use]
pub fn decode_big5_to_utf8(bytes: &[u8]) -> TranscodeResult<String> {
    // `BIG5.decode(bytes)` returns `(Cow<str>, Encoding, bool)`
    // where the third element is `had_replacements`. We never
    // need the resolved encoding (it's always BIG5 here) but the
    // bool is exactly the signal we want.
    let (cow, _encoding, had_replacements) = BIG5.decode(bytes);
    TranscodeResult {
        output: cow.into_owned(),
        had_replacements,
    }
}

/// Encode a UTF-8 string into Big5 bytes. Code points outside
/// the Big5 repertoire are emitted as numeric character
/// references (`&#NNN;` per WHATWG) so the output stays
/// well-formed ASCII for unmappable cases; `had_replacements`
/// flags that this happened.
#[must_use]
pub fn encode_utf8_to_big5(text: &str) -> TranscodeResult<Vec<u8>> {
    let (cow, _encoding, had_replacements) = BIG5.encode(text);
    TranscodeResult {
        output: cow.into_owned(),
        had_replacements,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Canonical Big5 sequences. Pin these byte-for-byte so a
    // future `encoding_rs` revision that changed the mapping
    // would fail the test instead of silently corrupting real
    // Taiwan data files.
    //
    // `你` = U+4F60, Big5 `A7 41`
    // `好` = U+597D, Big5 `A6 6E`
    //   (verified against the canonical Big5 ↔ Unicode mapping
    //   table from `BIG5.TXT` in the Unicode consortium archive)
    const HELLO_BIG5: &[u8] = &[0xA7, 0x41, 0xA6, 0x6E];
    const HELLO_UTF8: &str = "你好";

    #[test]
    fn decode_big5_round_trips_through_utf8() {
        let result = decode_big5_to_utf8(HELLO_BIG5);
        assert_eq!(result.output, HELLO_UTF8);
        assert!(!result.had_replacements, "clean decode must not flag");
    }

    #[test]
    fn encode_utf8_round_trips_back_to_big5() {
        let result = encode_utf8_to_big5(HELLO_UTF8);
        assert_eq!(result.output, HELLO_BIG5);
        assert!(!result.had_replacements);
    }

    #[test]
    fn ascii_passes_through_both_directions_unchanged() {
        // The ASCII subset (0x00–0x7F) is identical between Big5
        // and UTF-8. Any drift here would break every CSV header
        // and column delimiter; pin the equality explicitly.
        let ascii = "Year,Month,Value\n2026,05,42\n";
        let enc = encode_utf8_to_big5(ascii);
        assert_eq!(enc.output, ascii.as_bytes());
        assert!(!enc.had_replacements);

        let dec = decode_big5_to_utf8(ascii.as_bytes());
        assert_eq!(dec.output, ascii);
        assert!(!dec.had_replacements);
    }

    #[test]
    fn invalid_big5_bytes_become_u_fffd_with_flag() {
        // 0xFE / 0xFF are reserved in Big5; a lone 0xFE in the
        // input slot can't form a valid lead-trail pair so the
        // decoder emits U+FFFD. The flag must be true so the
        // wrapper can tell callers the decode was lossy.
        let result = decode_big5_to_utf8(&[0xFE]);
        assert!(result.output.contains('\u{FFFD}'));
        assert!(result.had_replacements);
    }

    #[test]
    fn unmappable_utf8_to_big5_emits_ncr_with_flag() {
        // Emoji code points (U+1F600+) aren't in Big5. The
        // encoder substitutes a numeric character reference
        // (`&#128512;` for 😀) per the WHATWG Encoding Standard
        // — well-formed ASCII, but information-lossy on the
        // round-trip. The flag distinguishes that case from a
        // clean encode.
        let emoji = "hello 😀";
        let result = encode_utf8_to_big5(emoji);
        let as_ascii = std::str::from_utf8(&result.output).expect("NCR output is ASCII");
        assert!(
            as_ascii.contains("&#128512;"),
            "expected NCR substitution, got {as_ascii:?}",
        );
        assert!(
            result.had_replacements,
            "lossy encode must flag replacements"
        );
    }

    #[test]
    fn empty_input_is_a_clean_empty_output() {
        let dec = decode_big5_to_utf8(&[]);
        assert_eq!(dec.output, "");
        assert!(!dec.had_replacements);
        let enc = encode_utf8_to_big5("");
        assert_eq!(enc.output, b"");
        assert!(!enc.had_replacements);
    }

    #[test]
    fn long_mixed_text_round_trips() {
        // Stress-test the encoder over a mixed CJK + ASCII
        // sequence long enough to exercise the internal chunk
        // boundaries `encoding_rs` uses (~4 KiB). Pin that the
        // round-trip is bitwise-identical for fully-Big5-mappable
        // content — the foundation the tool's contract rests on.
        let mut mixed = String::from("Year,城市,人口\n");
        for i in 0..200 {
            use std::fmt::Write as _;
            writeln!(mixed, "2026,臺北市,{i}").unwrap();
        }
        mixed.push_str("\nNotes: 觀光客成長率持續上揚。");
        let encoded = encode_utf8_to_big5(&mixed);
        assert!(!encoded.had_replacements);
        let decoded = decode_big5_to_utf8(&encoded.output);
        assert!(!decoded.had_replacements);
        assert_eq!(decoded.output, mixed);
    }
}
