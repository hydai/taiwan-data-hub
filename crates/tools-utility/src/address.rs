//! Taiwan address normalizer.
//!
//! Splits a free-form Taiwan address into:
//!  - `county` (縣 / 市) — normalised through the 改制 mapping so
//!    `台中縣` collapses to `台中市`, etc.
//!  - `district` (鄉 / 鎮 / 市 / 區)
//!  - `road` (路 / 街 / 道) including the final suffix character
//!  - `section` (`段`) — the numeric portion only, without the
//!    suffix character (e.g. `"一"` or `"2"`, not `"一段"`)
//!  - `lane` (`巷`) — numeric portion only
//!  - `alley` (`弄`) — numeric portion only
//!  - `number` (`號`) — numeric portion only; handles `123`,
//!    `123-1`, `123之5`
//!  - `floor` (`樓` / `F` / `B1F`) — best-effort; finds the *first*
//!    floor-like token in the remaining cursor (typically the only
//!    one, since floors appear after the number suffix). Returns
//!    raw digits or `B<n>` form, without the suffix character.
//!
//! The normalised result is intentionally tolerant: any field the
//! input doesn't have is `None`. This is a *segmentation*
//! pre-processor, not a strict syntax check — pure-junk input
//! returns an `AddressParts` where every field is `None`. Callers
//! that need a "valid address" signal should check whether at
//! least `county` and `district` were filled.
//!
//! ## 改制 normalisation
//!
//! Taiwan municipally restructured several counties into 直轄市 /
//! 改制 cities over 2010-2014:
//!  - 台中縣 + 台中市 → 台中市 (2010-12-25)
//!  - 台南縣 + 台南市 → 台南市 (2010-12-25)
//!  - 高雄縣 + 高雄市 → 高雄市 (2010-12-25)
//!  - 桃園縣 → 桃園市 (2014-12-25)
//!
//! Old-form inputs are mapped to the new form so downstream code
//! never has to handle both. `台北縣` likewise maps to `新北市`
//! (改制 2010-12-25). We do **not** normalise district names (e.g.
//! 三重市 → 三重區): that's a separate utility (#3.10).

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

// Pre-compile the per-suffix regexes once at first access. Each
// pattern is static; compiling on every call (which Copilot R1
// caught) is wasted work on a per-request hot path.
static SECTION_REGEX: LazyLock<Regex> = LazyLock::new(|| numeric_suffix_regex('段'));
static LANE_REGEX: LazyLock<Regex> = LazyLock::new(|| numeric_suffix_regex('巷'));
static ALLEY_REGEX: LazyLock<Regex> = LazyLock::new(|| numeric_suffix_regex('弄'));
static NUMBER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^.*?(\d+(?:-\d+)?(?:之\d+)?)號").expect("number regex"));
static FLOOR_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^.*?(B?\d+)(?:樓|F|f)").expect("floor regex"));

/// Structured form of a normalised Taiwan address. Every field is
/// optional — junk inputs surface as a struct of `None`s rather
/// than an error.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct AddressParts {
    pub county: Option<String>,
    pub district: Option<String>,
    pub road: Option<String>,
    pub section: Option<String>,
    pub lane: Option<String>,
    pub alley: Option<String>,
    pub number: Option<String>,
    pub floor: Option<String>,
}

/// Normalise a free-form Taiwan address. Always returns a struct;
/// inability to parse a given field maps to `None`. Whitespace is
/// stripped before matching; commas and 全形 spaces are treated as
/// soft separators.
#[must_use]
pub fn normalize_address(input: &str) -> AddressParts {
    // Drop ASCII + 全形 whitespace and commas — Taiwan addresses
    // commonly use them as visual separators between road / number
    // / floor, but they're never part of any token.
    let stripped: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',' && *c != '，')
        .collect();
    if stripped.is_empty() {
        return AddressParts::default();
    }

    let mut cursor = stripped.as_str();
    let mut out = AddressParts::default();

    // 1. County. Longest-prefix match from COUNTIES so "新北市" wins
    // over "新北" (which isn't a real prefix but defends against
    // ambiguity if the list grows). Apply 改制 mapping so e.g. the
    // input "台中縣..." normalises to "台中市".
    // `_matched_raw` would be the input prefix before normalisation
    // (e.g. "臺東縣"); unused at the call site today but kept in the
    // tuple so a future test / caller can compare pre- vs post-
    // normalisation forms without re-running the matcher.
    if let Some((_matched_raw, normalised, rest)) = strip_county_prefix(cursor) {
        out.county = Some(normalised.to_string());
        cursor = rest;
    }

    // 2. District. Greedy scan to the next 鄉/鎮/市/區 character.
    // Note: 市 is also a county suffix, but we already consumed the
    // county prefix, so a remaining 市 must be 縣轄市 (e.g. 苗栗市).
    //
    // **Only attempted when a county anchored the parse.** Without
    // a county prefix the input can be a road like "市府路" — the
    // district scan would otherwise eat the leading 市 and corrupt
    // the road token. Callers needing district-only parsing should
    // prepend a placeholder county on input.
    if out.county.is_some() {
        // Reject a suffix at index 0 — that means there's no
        // district *body*, just the suffix character (e.g.
        // "台北市市府路45號" leaves cursor="市府路45號" after the
        // county, and matching 市 at index 0 would steal the
        // road's leading character). At least one non-suffix
        // character must precede a real district suffix.
        if let Some((district, rest)) =
            take_until_suffix_after(cursor, &['鄉', '鎮', '市', '區'], 1)
        {
            out.district = Some(district);
            cursor = rest;
        }
    }

    // 3. Road (路/街/道). Greedy up to the first road suffix we
    // find. We don't pre-scan for stray 段/巷/弄/號 in front of the
    // road suffix — in well-formed Taiwan addresses they appear
    // *after* the road, so the take-up-to-suffix scan naturally
    // captures the road body. If a caller threads upstream noise
    // that looks like a section/lane marker, the road token will
    // include it; rejecting such inputs is the caller's problem.
    if let Some((road, rest)) = take_until_suffix(cursor, &['路', '街', '道']) {
        out.road = Some(road);
        cursor = rest;
    }

    // 4-8. Section / lane / alley / number / floor. Each is a
    // numeric / Chinese-numeral token immediately followed by its
    // suffix character.
    out.section = take_numeric_token(&mut cursor, &SECTION_REGEX);
    out.lane = take_numeric_token(&mut cursor, &LANE_REGEX);
    out.alley = take_numeric_token(&mut cursor, &ALLEY_REGEX);
    out.number = take_regex_token(&mut cursor, &NUMBER_REGEX);
    out.floor = take_regex_token(&mut cursor, &FLOOR_REGEX);

    out
}

/// Counties / 直轄市 in their canonical (post-改制) form. The
/// longest entry first so prefix-matching is deterministic when a
/// shorter prefix could otherwise win (e.g. 新北市 vs hypothetical
/// 新北). Crate-private — the canonicalizer (`crate::canonical`)
/// reads it through the explicit accessor below; nothing else in
/// the public API references it.
pub(crate) const COUNTIES: &[&str] = &[
    "台北市",
    "新北市",
    "桃園市",
    "台中市",
    "台南市",
    "高雄市",
    "基隆市",
    "新竹市",
    "嘉義市",
    "新竹縣",
    "苗栗縣",
    "彰化縣",
    "南投縣",
    "雲林縣",
    "嘉義縣",
    "屏東縣",
    "宜蘭縣",
    "花蓮縣",
    "台東縣",
    "澎湖縣",
    "金門縣",
    "連江縣",
];

/// Pre-改制 county names that map to the post-改制 canonical form.
/// Keyed on the input form, valued on the canonical form.
/// Crate-private — see [`COUNTIES`].
pub(crate) const COUNTY_ALIASES: &[(&str, &str)] = &[
    ("台中縣", "台中市"),
    ("台南縣", "台南市"),
    ("高雄縣", "高雄市"),
    ("桃園縣", "桃園市"),
    // 台北縣 → 新北市 (改制 2010-12-25).
    ("台北縣", "新北市"),
    // Common variant: 臺 (traditional) vs 台 (simplified-by-usage)
    // — accept both forms on input, emit the 台 form (matches the
    // government's own administrative usage in most public APIs).
    ("臺北市", "台北市"),
    ("臺中市", "台中市"),
    ("臺中縣", "台中市"),
    ("臺南市", "台南市"),
    ("臺南縣", "台南市"),
    ("臺東縣", "台東縣"),
    ("臺北縣", "新北市"),
];

/// Crate-private alias for [`strip_county_prefix`] used by
/// sibling modules (e.g. `canonical`). Renamed `*_exact` to flag
/// that the function is sensitive to the exact prefix bytes (no
/// district peeling). Not part of the public API.
pub(crate) fn strip_county_prefix_exact(s: &str) -> Option<(&str, &'static str, &str)> {
    strip_county_prefix(s)
}

/// Try to match a county prefix on `s` (longest-first). On hit,
/// returns the raw matched string, the canonical normalised form,
/// and the rest of the input after the prefix.
fn strip_county_prefix(s: &str) -> Option<(&str, &'static str, &str)> {
    // Aliases first — they're the historical forms callers are
    // most likely to typo. Then the canonical list.
    for (alias, canonical) in COUNTY_ALIASES {
        if let Some(rest) = s.strip_prefix(alias) {
            return Some((&s[..alias.len()], canonical, rest));
        }
    }
    for county in COUNTIES {
        if let Some(rest) = s.strip_prefix(county) {
            return Some((&s[..county.len()], county, rest));
        }
    }
    None
}

/// Scan `s` for the first occurrence of any character in `suffixes`,
/// return the substring up to **and including** the suffix as one
/// token plus the rest of the input. Returns `None` if no suffix
/// appears.
fn take_until_suffix<'a>(s: &'a str, suffixes: &[char]) -> Option<(String, &'a str)> {
    take_until_suffix_after(s, suffixes, 0)
}

/// Same as [`take_until_suffix`] but requires at least `min_body_bytes`
/// of input before the suffix character. Used by the district scan
/// to reject a suffix at position 0 (which would mean the "district"
/// is just the suffix character, e.g. matching the `市` in `市府路`
/// when the actual district is missing).
fn take_until_suffix_after<'a>(
    s: &'a str,
    suffixes: &[char],
    min_body_bytes: usize,
) -> Option<(String, &'a str)> {
    let (idx, ch) = s
        .char_indices()
        .find(|(idx, c)| *idx >= min_body_bytes && suffixes.contains(c))?;
    let end = idx + ch.len_utf8();
    Some((s[..end].to_string(), &s[end..]))
}

/// Take a `<digits-or-Chinese-numeral>+<suffix>` token using a
/// pre-compiled regex. Returns capture group 1 (the numeric portion
/// without the suffix) and advances `cursor` past the match.
fn take_numeric_token(cursor: &mut &str, re: &Regex) -> Option<String> {
    take_regex_token(cursor, re)
}

/// Generic helper: run `re` against `cursor`, take capture group 1,
/// advance `cursor` past the full match. The regexes used here are
/// expected to anchor with `^.*?` and capture exactly one group.
fn take_regex_token(cursor: &mut &str, re: &Regex) -> Option<String> {
    let captures = re.captures(cursor)?;
    let value = captures.get(1)?.as_str().to_string();
    let end = captures.get(0)?.end();
    *cursor = &cursor[end..];
    Some(value)
}

/// Build the regex that matches a `<digits-or-Chinese-numeral>+<suffix>`
/// token. Called once per suffix at module init via `LazyLock`.
fn numeric_suffix_regex(suffix: char) -> Regex {
    // The group is digits OR Chinese numerals. Chinese numerals
    // for sections / lanes / alleys typically use the single
    // characters 一二三四五六七八九十; we accept all of them plus
    // 兩 (variant of 二) for robustness.
    let pattern = format!(r"^.*?(\d+|[一二三四五六七八九十兩]+){suffix}");
    Regex::new(&pattern).expect("static pattern compiles")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build an `AddressParts` more concisely than the full
    /// `AddressParts { county: Some("..".into()), ... }` form. The
    /// arg count matches the struct field count by design;
    /// `clippy::too_many_arguments` is allowed because the only
    /// alternative — a builder struct — would obscure the test
    /// data at every call site.
    #[allow(clippy::too_many_arguments)]
    fn parts(
        county: Option<&str>,
        district: Option<&str>,
        road: Option<&str>,
        section: Option<&str>,
        lane: Option<&str>,
        alley: Option<&str>,
        number: Option<&str>,
        floor: Option<&str>,
    ) -> AddressParts {
        AddressParts {
            county: county.map(str::to_string),
            district: district.map(str::to_string),
            road: road.map(str::to_string),
            section: section.map(str::to_string),
            lane: lane.map(str::to_string),
            alley: alley.map(str::to_string),
            number: number.map(str::to_string),
            floor: floor.map(str::to_string),
        }
    }

    #[test]
    fn full_address_with_all_components_parses() {
        let out = normalize_address("台北市信義區市府路45號5樓");
        assert_eq!(
            out,
            parts(
                Some("台北市"),
                Some("信義區"),
                Some("市府路"),
                None,
                None,
                None,
                Some("45"),
                Some("5"),
            )
        );
    }

    #[test]
    fn address_with_section_lane_alley_number() {
        let out = normalize_address("台北市大安區忠孝東路四段153巷5弄12號");
        assert_eq!(out.county.as_deref(), Some("台北市"));
        assert_eq!(out.district.as_deref(), Some("大安區"));
        assert_eq!(out.road.as_deref(), Some("忠孝東路"));
        assert_eq!(out.section.as_deref(), Some("四"));
        assert_eq!(out.lane.as_deref(), Some("153"));
        assert_eq!(out.alley.as_deref(), Some("5"));
        assert_eq!(out.number.as_deref(), Some("12"));
    }

    #[test]
    fn arabic_and_chinese_section_numerals_both_parse() {
        assert_eq!(
            normalize_address("台北市中山區中山北路二段45號")
                .section
                .as_deref(),
            Some("二"),
        );
        assert_eq!(
            normalize_address("台北市中山區中山北路2段45號")
                .section
                .as_deref(),
            Some("2"),
        );
    }

    #[test]
    fn pre_reorg_taichung_county_maps_to_taichung_city() {
        let out = normalize_address("台中縣豐原區中正路100號");
        assert_eq!(out.county.as_deref(), Some("台中市"));
        assert_eq!(out.district.as_deref(), Some("豐原區"));
    }

    #[test]
    fn pre_reorg_taipei_county_maps_to_new_taipei_city() {
        let out = normalize_address("台北縣板橋市文化路一段50號");
        assert_eq!(out.county.as_deref(), Some("新北市"));
    }

    #[test]
    fn pre_reorg_kaohsiung_county_maps_to_kaohsiung_city() {
        let out = normalize_address("高雄縣鳳山市中山路一段100號");
        assert_eq!(out.county.as_deref(), Some("高雄市"));
    }

    #[test]
    fn pre_reorg_tainan_county_maps_to_tainan_city() {
        let out = normalize_address("台南縣新營市中正路1號");
        assert_eq!(out.county.as_deref(), Some("台南市"));
    }

    #[test]
    fn pre_reorg_taoyuan_county_maps_to_taoyuan_city() {
        let out = normalize_address("桃園縣中壢市中正路100號");
        assert_eq!(out.county.as_deref(), Some("桃園市"));
    }

    #[test]
    fn traditional_form_maps_to_simplified_form() {
        let out = normalize_address("臺北市信義區市府路45號");
        assert_eq!(out.county.as_deref(), Some("台北市"));
        assert_eq!(out.district.as_deref(), Some("信義區"));
    }

    #[test]
    fn taitung_county_normalises_traditional_to_simplified() {
        // 臺東縣 → 台東縣 via the COUNTY_ALIASES map: we normalise
        // the 臺 prefix to 台 consistently with the rest of the
        // canonical-form output, so downstream callers don't have
        // to handle both glyph forms. The county itself stays as
        // a 縣 (not a 直轄市) — the 改制 mapping only applies to
        // the five reorganised entities.
        let out = normalize_address("臺東縣台東市中華路一段1號");
        assert_eq!(out.county.as_deref(), Some("台東縣"));
    }

    #[test]
    fn number_with_hyphen_suffix() {
        let out = normalize_address("台北市信義區市府路45-1號");
        assert_eq!(out.number.as_deref(), Some("45-1"));
    }

    #[test]
    fn number_with_chinese_zhi_suffix() {
        let out = normalize_address("台北市信義區市府路45之2號");
        assert_eq!(out.number.as_deref(), Some("45之2"));
    }

    #[test]
    fn floor_with_chinese_lou() {
        let out = normalize_address("台北市信義區市府路45號5樓");
        assert_eq!(out.floor.as_deref(), Some("5"));
    }

    #[test]
    fn floor_with_ascii_f() {
        let out = normalize_address("台北市信義區市府路45號5F");
        assert_eq!(out.floor.as_deref(), Some("5"));
    }

    #[test]
    fn basement_floor_b1() {
        let out = normalize_address("台北市信義區市府路45號B1F");
        assert_eq!(out.floor.as_deref(), Some("B1"));
    }

    #[test]
    fn district_xiang_suffix() {
        let out = normalize_address("南投縣魚池鄉中山路1號");
        assert_eq!(out.district.as_deref(), Some("魚池鄉"));
    }

    #[test]
    fn district_zhen_suffix() {
        let out = normalize_address("彰化縣鹿港鎮中山路1號");
        assert_eq!(out.district.as_deref(), Some("鹿港鎮"));
    }

    #[test]
    fn road_jie_suffix() {
        let out = normalize_address("台北市中山區中山北路一段40巷5號");
        assert_eq!(out.road.as_deref(), Some("中山北路"));
        let out2 = normalize_address("台北市大同區迪化街一段100號");
        assert_eq!(out2.road.as_deref(), Some("迪化街"));
    }

    #[test]
    fn road_dao_suffix() {
        let out = normalize_address("台北市信義區基隆路四段大道5號");
        // First road-suffix wins (路 here), but verifying 道 also
        // parses in isolation:
        let out2 = normalize_address("台北市信義區仁愛大道5號");
        assert_eq!(out2.road.as_deref(), Some("仁愛大道"));
        let _ = out;
    }

    #[test]
    fn whitespace_and_commas_stripped() {
        let out = normalize_address("  台北市 信義區, 市府路 45 號 ");
        assert_eq!(out.county.as_deref(), Some("台北市"));
        assert_eq!(out.district.as_deref(), Some("信義區"));
        assert_eq!(out.number.as_deref(), Some("45"));
    }

    #[test]
    fn fullwidth_comma_stripped() {
        let out = normalize_address("台北市，信義區，市府路45號");
        assert_eq!(out.county.as_deref(), Some("台北市"));
    }

    #[test]
    fn empty_input_returns_all_none() {
        let out = normalize_address("");
        assert_eq!(out, AddressParts::default());
    }

    #[test]
    fn pure_junk_returns_all_none() {
        let out = normalize_address("hello world");
        // No address suffix characters → everything None.
        assert_eq!(out.county, None);
        assert_eq!(out.district, None);
        assert_eq!(out.road, None);
        assert_eq!(out.number, None);
    }

    /// Locks the R1 fix for district scan stealing a road suffix
    /// when the district is omitted. Input has a real county
    /// ("台北市") but no district — the next character after the
    /// county is the start of the road ("市府路"). The district
    /// scan must not consume that leading "市".
    #[test]
    fn district_scan_rejects_suffix_at_index_zero() {
        let out = normalize_address("台北市市府路45號");
        assert_eq!(out.county.as_deref(), Some("台北市"));
        assert_eq!(out.district, None, "district was empty in input");
        assert_eq!(
            out.road.as_deref(),
            Some("市府路"),
            "road must keep its leading 市, not have it eaten by the district scan",
        );
        assert_eq!(out.number.as_deref(), Some("45"));
    }

    #[test]
    fn input_without_county_still_parses_remaining_fields() {
        // Sometimes addresses come without county/district (e.g. an
        // internal form filled in by a TPE-only system). The parser
        // shouldn't refuse; it just leaves county/district None.
        let out = normalize_address("市府路45號");
        assert_eq!(out.county, None);
        assert_eq!(out.road.as_deref(), Some("市府路"));
        assert_eq!(out.number.as_deref(), Some("45"));
    }

    /// Canonical-county corpus: 22 well-formed addresses, one per
    /// county / 直轄市. Combined with the targeted single-purpose
    /// tests above, the alias mapping corpus, and the
    /// number/floor/section permutation matrix below, the total
    /// assertion count clears the 100+ bar from the #3.7 issue's
    /// Definition of Done.
    #[test]
    fn canonical_county_corpus() {
        let cases: &[(&str, &str)] = &[
            ("台北市信義區市府路45號", "台北市"),
            ("新北市板橋區文化路一段5號", "新北市"),
            ("桃園市桃園區中正路1號", "桃園市"),
            ("台中市西屯區台灣大道二段100號", "台中市"),
            ("台南市中西區忠義路二段1號", "台南市"),
            ("高雄市鼓山區美術館路80號", "高雄市"),
            ("基隆市中正區義一路1號", "基隆市"),
            ("新竹市東區光復路一段1號", "新竹市"),
            ("嘉義市東區中山路100號", "嘉義市"),
            ("新竹縣竹北市光明六路1號", "新竹縣"),
            ("苗栗縣苗栗市府前路100號", "苗栗縣"),
            ("彰化縣彰化市中山路一段1號", "彰化縣"),
            ("南投縣南投市府前路100號", "南投縣"),
            ("雲林縣斗六市中山路1號", "雲林縣"),
            ("嘉義縣朴子市中正路1號", "嘉義縣"),
            ("屏東縣屏東市自由路1號", "屏東縣"),
            ("宜蘭縣宜蘭市中山路一段1號", "宜蘭縣"),
            ("花蓮縣花蓮市中山路1號", "花蓮縣"),
            ("台東縣台東市中華路一段1號", "台東縣"),
            ("澎湖縣馬公市民權路1號", "澎湖縣"),
            ("金門縣金城鎮民生路60號", "金門縣"),
            ("連江縣南竿鄉復興村1號", "連江縣"),
        ];
        for (input, expected_county) in cases {
            let out = normalize_address(input);
            assert_eq!(
                out.county.as_deref(),
                Some(*expected_county),
                "input `{input}` should parse county = `{expected_county}`",
            );
            assert!(
                out.district.is_some(),
                "input `{input}` should parse a district, got {:?}",
                out.district,
            );
        }
    }

    #[test]
    fn pre_reorg_county_corpus() {
        // 改制 mappings exhaustively listed in COUNTY_ALIASES.
        // Cover each pair to confirm the normalisation actually
        // happens.
        let cases: &[(&str, &str)] = &[
            ("台中縣豐原區中正路100號", "台中市"),
            ("台中市西屯區台灣大道100號", "台中市"),
            ("台南縣新營市中正路1號", "台南市"),
            ("台南市中西區忠義路1號", "台南市"),
            ("高雄縣鳳山市中山路100號", "高雄市"),
            ("高雄市鼓山區美術館路80號", "高雄市"),
            ("桃園縣中壢市中正路100號", "桃園市"),
            ("桃園市中壢區中正路100號", "桃園市"),
            ("台北縣板橋市文化路50號", "新北市"),
            ("新北市板橋區文化路50號", "新北市"),
            ("臺北市信義區市府路45號", "台北市"),
            ("臺中縣豐原區中正路100號", "台中市"),
        ];
        for (input, expected_county) in cases {
            let out = normalize_address(input);
            assert_eq!(
                out.county.as_deref(),
                Some(*expected_county),
                "input `{input}` should normalise to `{expected_county}`",
            );
        }
    }

    /// Number/floor/section permutation matrix. Generates well over
    /// 60 distinct parses by Cartesian-ing 4 number forms × 4 floor
    /// forms × 4 section forms × 2 base addresses. Each assertion
    /// hits an independent input string, so this isn't redundant
    /// with the canonical / pre-reorg corpora — they cover
    /// county/district variation; this matrix covers everything
    /// after the road.
    #[test]
    fn permutation_matrix_numbers_floors_sections() {
        // (number_token, expected_number_field)
        let numbers: &[(&str, &str)] = &[
            ("45", "45"),
            ("100", "100"),
            ("45-1", "45-1"),
            ("123之5", "123之5"),
        ];
        // (floor_token_with_suffix, expected_floor_field).
        // Last empty entry exercises the "no floor" path.
        let floors: &[(&str, Option<&str>)] = &[
            ("5樓", Some("5")),
            ("12F", Some("12")),
            ("B1F", Some("B1")),
            ("", None),
        ];
        // (section_text_with_suffix, expected_section_field). Last
        // empty entry exercises the "no section" path.
        let sections: &[(&str, Option<&str>)] = &[
            ("一段", Some("一")),
            ("二段", Some("二")),
            ("3段", Some("3")),
            ("", None),
        ];
        // (county_prefix + district + road, expected_county).
        let bases: &[(&str, &str)] = &[
            ("台北市信義區市府路", "台北市"),
            ("台中市西屯區台灣大道", "台中市"),
        ];

        let mut total = 0_usize;
        for (base, expected_county) in bases {
            for (sec_in, sec_out) in sections {
                for (num_in, num_out) in numbers {
                    for (floor_in, floor_out) in floors {
                        let input = format!("{base}{sec_in}{num_in}號{floor_in}");
                        let out = normalize_address(&input);
                        assert_eq!(
                            out.county.as_deref(),
                            Some(*expected_county),
                            "input `{input}` should parse county",
                        );
                        assert_eq!(
                            out.number.as_deref(),
                            Some(*num_out),
                            "input `{input}` should parse number = `{num_out}`",
                        );
                        assert_eq!(
                            out.section.as_deref(),
                            *sec_out,
                            "input `{input}` should parse section = {sec_out:?}",
                        );
                        assert_eq!(
                            out.floor.as_deref(),
                            *floor_out,
                            "input `{input}` should parse floor = {floor_out:?}",
                        );
                        total += 1;
                    }
                }
            }
        }
        // 2 bases × 4 sections × 4 numbers × 4 floors = 128 cases.
        assert_eq!(total, 128, "permutation count drifted");
    }
}
