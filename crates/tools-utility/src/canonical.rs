//! Canonicalize free-form 縣市 / 鄉鎮市區 strings into stable
//! identifier codes (e.g. `ROC_CITY_NEW_TAIPEI`,
//! `DIST_NTPE_BANQIAO`). District codes carry a county-specific
//! prefix to disambiguate identically-named districts across
//! counties (e.g. 中山區 appears in 台北 / 基隆 / 高雄).
//!
//! Per the #3.10 Definition of Done:
//!  - Counties: every input form (post-改制 or pre-改制 alias,
//!    traditional 臺 or simplified 台) collapses to one code from
//!    [`CountyCode`].
//!  - Districts: looked up per (canonical county, raw district)
//!    against a static dictionary. v0.1 bakes the 6 直轄市
//!    (台北 / 新北 / 桃園 / 台中 / 台南 / 高雄) — together they
//!    cover ≈70% of Taiwan's population. Other counties resolve
//!    the county code but return `DistrictCode::Unknown` until
//!    v0.2 fills in the rest of the table.
//!  - 改制 mappings re-use the [`crate::address`] tables so the
//!    address normalizer and the canonicalizer never disagree.

use crate::address::{COUNTY_ALIASES, strip_county_prefix_exact};

/// Stable identifier for every 直轄市 / 縣 (22 total). The
/// `as_code()` strings are the wire form.
///
/// Intentionally does **not** derive `Serialize` — the default
/// enum representation would emit variant names ("Taipei") or,
/// with `rename_all = "SCREAMING_SNAKE_CASE"`, "TAIPEI", neither
/// of which matches the documented stable identifier strings
/// from `as_code()` (e.g. `ROC_CITY_TAIPEI`). Callers serialising
/// to JSON should go through `as_code()` so the wire form is
/// always the canonical string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountyCode {
    Taipei,     // 台北市
    NewTaipei,  // 新北市
    Taoyuan,    // 桃園市
    Taichung,   // 台中市
    Tainan,     // 台南市
    Kaohsiung,  // 高雄市
    Keelung,    // 基隆市
    Hsinchu,    // 新竹市
    Chiayi,     // 嘉義市
    HsinchuCo,  // 新竹縣
    Miaoli,     // 苗栗縣
    Changhua,   // 彰化縣
    Nantou,     // 南投縣
    Yunlin,     // 雲林縣
    ChiayiCo,   // 嘉義縣
    Pingtung,   // 屏東縣
    Yilan,      // 宜蘭縣
    Hualien,    // 花蓮縣
    Taitung,    // 台東縣
    Penghu,     // 澎湖縣
    Kinmen,     // 金門縣
    Lienchiang, // 連江縣
}

impl CountyCode {
    pub fn as_code(self) -> &'static str {
        match self {
            Self::Taipei => "ROC_CITY_TAIPEI",
            Self::NewTaipei => "ROC_CITY_NEW_TAIPEI",
            Self::Taoyuan => "ROC_CITY_TAOYUAN",
            Self::Taichung => "ROC_CITY_TAICHUNG",
            Self::Tainan => "ROC_CITY_TAINAN",
            Self::Kaohsiung => "ROC_CITY_KAOHSIUNG",
            Self::Keelung => "ROC_CITY_KEELUNG",
            Self::Hsinchu => "ROC_CITY_HSINCHU",
            Self::Chiayi => "ROC_CITY_CHIAYI",
            Self::HsinchuCo => "ROC_COUNTY_HSINCHU",
            Self::Miaoli => "ROC_COUNTY_MIAOLI",
            Self::Changhua => "ROC_COUNTY_CHANGHUA",
            Self::Nantou => "ROC_COUNTY_NANTOU",
            Self::Yunlin => "ROC_COUNTY_YUNLIN",
            Self::ChiayiCo => "ROC_COUNTY_CHIAYI",
            Self::Pingtung => "ROC_COUNTY_PINGTUNG",
            Self::Yilan => "ROC_COUNTY_YILAN",
            Self::Hualien => "ROC_COUNTY_HUALIEN",
            Self::Taitung => "ROC_COUNTY_TAITUNG",
            Self::Penghu => "ROC_COUNTY_PENGHU",
            Self::Kinmen => "ROC_COUNTY_KINMEN",
            Self::Lienchiang => "ROC_COUNTY_LIENCHIANG",
        }
    }

    /// Canonical zh-TW name (post-改制 simplified form).
    pub fn name_zh(self) -> &'static str {
        match self {
            Self::Taipei => "台北市",
            Self::NewTaipei => "新北市",
            Self::Taoyuan => "桃園市",
            Self::Taichung => "台中市",
            Self::Tainan => "台南市",
            Self::Kaohsiung => "高雄市",
            Self::Keelung => "基隆市",
            Self::Hsinchu => "新竹市",
            Self::Chiayi => "嘉義市",
            Self::HsinchuCo => "新竹縣",
            Self::Miaoli => "苗栗縣",
            Self::Changhua => "彰化縣",
            Self::Nantou => "南投縣",
            Self::Yunlin => "雲林縣",
            Self::ChiayiCo => "嘉義縣",
            Self::Pingtung => "屏東縣",
            Self::Yilan => "宜蘭縣",
            Self::Hualien => "花蓮縣",
            Self::Taitung => "台東縣",
            Self::Penghu => "澎湖縣",
            Self::Kinmen => "金門縣",
            Self::Lienchiang => "連江縣",
        }
    }
}

/// District canonical code. For v0.1 we bake the 6 直轄市 fully
/// and surface other counties' districts as `Unknown` (the
/// county code still resolves correctly).
///
/// No `Serialize` derive for the same reason as `CountyCode`:
/// the default tagged-enum encoding (`{"Known": "DIST_..."}`)
/// would diverge from the MCP wire form. The MCP wrapper
/// converts via `as_code()` → string-or-null.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistrictCode {
    /// District resolved to one of the baked codes (e.g.
    /// `"DIST_NTPE_BANQIAO"`, `"DIST_TPE_XINYI"`). The
    /// county-specific prefix avoids cross-county collisions.
    Known(&'static str),
    /// District wasn't in our v0.1 table — county still resolved
    /// correctly. Caller has the raw input via the
    /// [`Canonical::district_raw`] field for further handling.
    Unknown,
}

impl DistrictCode {
    pub fn as_code(&self) -> Option<&'static str> {
        match self {
            Self::Known(c) => Some(*c),
            Self::Unknown => None,
        }
    }
}

/// Result of [`canonicalize`]. The raw fields are preserved so a
/// caller can fall back when canonicalisation finds the county
/// but not the district.
///
/// Not `Serialize`-derived for the same reason as the codes
/// (the wire form needs `as_code()` translation); the MCP
/// wrapper does that conversion explicitly in `render`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canonical {
    pub county_code: Option<CountyCode>,
    pub county_name_zh: Option<&'static str>,
    pub district_code: DistrictCode,
    pub district_raw: Option<String>,
}

/// Look up a county name (any form: post-改制, pre-改制 alias,
/// 臺 or 台 variant) and return its [`CountyCode`]. Returns
/// `None` if no match.
#[must_use]
pub fn lookup_county(name: &str) -> Option<CountyCode> {
    let stripped = strip_whitespace(name);
    // 1. Try the canonical post-改制 form via address-module
    //    helpers so address normalisation and canonicalisation
    //    use the same source of truth.
    let canonical_form = if let Some((_raw, canonical, rest)) = strip_county_prefix_exact(&stripped)
    {
        if rest.is_empty() {
            Some(canonical)
        } else {
            // The input had trailing characters past the county
            // suffix — treat it as a mismatch (caller should use
            // address::normalize_address first if they want to
            // peel a district off).
            return None;
        }
    } else {
        None
    };
    let canonical_form = canonical_form?;
    county_from_canonical_name(canonical_form)
}

/// Canonicalize a `(county, district)` pair. `district` is
/// optional; when omitted the result carries `DistrictCode::Unknown`
/// with `district_raw` = None.
#[must_use]
pub fn canonicalize(county_input: &str, district_input: Option<&str>) -> Canonical {
    let county = lookup_county(county_input);
    // Strip first, then check empty — `,` / `，` / `,  ` all
    // strip to "" and should be treated as "no district given"
    // rather than "empty district passed through". This keeps
    // district_raw honest (always Some(non-empty) or None).
    let stripped = district_input
        .map(strip_whitespace)
        .filter(|s| !s.is_empty());
    let (district_code, district_raw) = match (county, stripped) {
        (Some(c), Some(raw)) => {
            let code = lookup_district(c, &raw);
            (code, Some(raw))
        }
        (_, Some(raw)) => (DistrictCode::Unknown, Some(raw)),
        _ => (DistrictCode::Unknown, None),
    };
    Canonical {
        county_code: county,
        county_name_zh: county.map(CountyCode::name_zh),
        district_code,
        district_raw,
    }
}

/// Drop every whitespace char (ASCII + 全形) and ASCII / 全形
/// commas. Stripping *internal* whitespace too lets us match
/// "信義 區" → "信義區" in the table; it's documented behaviour
/// of this function rather than a trim. `district_raw` echoes
/// the post-strip form so callers see what was matched against.
fn strip_whitespace(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != ',' && *c != '，')
        .collect()
}

fn county_from_canonical_name(name: &str) -> Option<CountyCode> {
    match name {
        "台北市" => Some(CountyCode::Taipei),
        "新北市" => Some(CountyCode::NewTaipei),
        "桃園市" => Some(CountyCode::Taoyuan),
        "台中市" => Some(CountyCode::Taichung),
        "台南市" => Some(CountyCode::Tainan),
        "高雄市" => Some(CountyCode::Kaohsiung),
        "基隆市" => Some(CountyCode::Keelung),
        "新竹市" => Some(CountyCode::Hsinchu),
        "嘉義市" => Some(CountyCode::Chiayi),
        "新竹縣" => Some(CountyCode::HsinchuCo),
        "苗栗縣" => Some(CountyCode::Miaoli),
        "彰化縣" => Some(CountyCode::Changhua),
        "南投縣" => Some(CountyCode::Nantou),
        "雲林縣" => Some(CountyCode::Yunlin),
        "嘉義縣" => Some(CountyCode::ChiayiCo),
        "屏東縣" => Some(CountyCode::Pingtung),
        "宜蘭縣" => Some(CountyCode::Yilan),
        "花蓮縣" => Some(CountyCode::Hualien),
        "台東縣" => Some(CountyCode::Taitung),
        "澎湖縣" => Some(CountyCode::Penghu),
        "金門縣" => Some(CountyCode::Kinmen),
        "連江縣" => Some(CountyCode::Lienchiang),
        _ => None,
    }
}

/// Look up a district name within a specific county. Returns
/// [`DistrictCode::Known`] when matched against the v0.1 tables
/// (currently the 6 直轄市), else [`DistrictCode::Unknown`].
#[must_use]
fn lookup_district(county: CountyCode, district: &str) -> DistrictCode {
    let table: &[(&str, &str)] = match county {
        CountyCode::Taipei => TAIPEI_DISTRICTS,
        CountyCode::NewTaipei => NEW_TAIPEI_DISTRICTS,
        CountyCode::Taoyuan => TAOYUAN_DISTRICTS,
        CountyCode::Taichung => TAICHUNG_DISTRICTS,
        CountyCode::Tainan => TAINAN_DISTRICTS,
        CountyCode::Kaohsiung => KAOHSIUNG_DISTRICTS,
        _ => return DistrictCode::Unknown,
    };
    for (zh, code) in table {
        if *zh == district {
            return DistrictCode::Known(code);
        }
    }
    // Try with the 區 suffix added — some pre-改制 inputs use
    // "市" or omit the suffix entirely (e.g. "鳳山" instead of
    // "鳳山區"). Append and retry.
    let with_district_suffix = format!("{district}區");
    for (zh, code) in table {
        if *zh == with_district_suffix {
            return DistrictCode::Known(code);
        }
    }
    // Also try replacing trailing 市 with 區 (post-改制 reorgs
    // converted 鳳山市 → 鳳山區, etc.).
    if let Some(stem) = district.strip_suffix('市') {
        let reorg = format!("{stem}區");
        for (zh, code) in table {
            if *zh == reorg {
                return DistrictCode::Known(code);
            }
        }
    }
    DistrictCode::Unknown
}

/// Public exposure of the 改制 alias map so MCP-tool callers can
/// see what's covered. Re-export of [`crate::address::COUNTY_ALIASES`]
/// — kept here only so this module is the discoverable surface.
#[must_use]
pub fn county_aliases() -> &'static [(&'static str, &'static str)] {
    COUNTY_ALIASES
}

// District tables. Each tuple is (zh-TW name, stable code). The
// code prefix is `DIST_` followed by a county-specific suffix to
// disambiguate cross-county collisions (e.g. there's a 中山區 in
// 台北 and 基隆 and 高雄 — code includes the county).

const TAIPEI_DISTRICTS: &[(&str, &str)] = &[
    ("中正區", "DIST_TPE_ZHONGZHENG"),
    ("大同區", "DIST_TPE_DATONG"),
    ("中山區", "DIST_TPE_ZHONGSHAN"),
    ("松山區", "DIST_TPE_SONGSHAN"),
    ("大安區", "DIST_TPE_DAAN"),
    ("萬華區", "DIST_TPE_WANHUA"),
    ("信義區", "DIST_TPE_XINYI"),
    ("士林區", "DIST_TPE_SHILIN"),
    ("北投區", "DIST_TPE_BEITOU"),
    ("內湖區", "DIST_TPE_NEIHU"),
    ("南港區", "DIST_TPE_NANGANG"),
    ("文山區", "DIST_TPE_WENSHAN"),
];

const NEW_TAIPEI_DISTRICTS: &[(&str, &str)] = &[
    ("板橋區", "DIST_NTPE_BANQIAO"),
    ("三重區", "DIST_NTPE_SANCHONG"),
    ("中和區", "DIST_NTPE_ZHONGHE"),
    ("永和區", "DIST_NTPE_YONGHE"),
    ("新莊區", "DIST_NTPE_XINZHUANG"),
    ("新店區", "DIST_NTPE_XINDIAN"),
    ("土城區", "DIST_NTPE_TUCHENG"),
    ("蘆洲區", "DIST_NTPE_LUZHOU"),
    ("樹林區", "DIST_NTPE_SHULIN"),
    ("汐止區", "DIST_NTPE_XIZHI"),
    ("鶯歌區", "DIST_NTPE_YINGGE"),
    ("三峽區", "DIST_NTPE_SANXIA"),
    ("淡水區", "DIST_NTPE_TAMSUI"),
    ("八里區", "DIST_NTPE_BALI"),
    ("林口區", "DIST_NTPE_LINKOU"),
    ("泰山區", "DIST_NTPE_TAISHAN"),
    ("五股區", "DIST_NTPE_WUGU"),
    ("瑞芳區", "DIST_NTPE_RUIFANG"),
    ("貢寮區", "DIST_NTPE_GONGLIAO"),
    ("雙溪區", "DIST_NTPE_SHUANGXI"),
    ("平溪區", "DIST_NTPE_PINGXI"),
    ("石碇區", "DIST_NTPE_SHIDING"),
    ("深坑區", "DIST_NTPE_SHENKENG"),
    ("烏來區", "DIST_NTPE_WULAI"),
    ("坪林區", "DIST_NTPE_PINGLIN"),
    ("石門區", "DIST_NTPE_SHIMEN"),
    ("三芝區", "DIST_NTPE_SANZHI"),
    ("金山區", "DIST_NTPE_JINSHAN"),
    ("萬里區", "DIST_NTPE_WANLI"),
];

const TAOYUAN_DISTRICTS: &[(&str, &str)] = &[
    ("桃園區", "DIST_TYN_TAOYUAN"),
    ("中壢區", "DIST_TYN_ZHONGLI"),
    ("平鎮區", "DIST_TYN_PINGZHEN"),
    ("八德區", "DIST_TYN_BADE"),
    ("楊梅區", "DIST_TYN_YANGMEI"),
    ("蘆竹區", "DIST_TYN_LUZHU"),
    ("大溪區", "DIST_TYN_DAXI"),
    ("龜山區", "DIST_TYN_GUISHAN"),
    ("龍潭區", "DIST_TYN_LONGTAN"),
    ("大園區", "DIST_TYN_DAYUAN"),
    ("觀音區", "DIST_TYN_GUANYIN"),
    ("新屋區", "DIST_TYN_XINWU"),
    ("復興區", "DIST_TYN_FUXING"),
];

const TAICHUNG_DISTRICTS: &[(&str, &str)] = &[
    ("中區", "DIST_TXG_CENTRAL"),
    ("東區", "DIST_TXG_EAST"),
    ("南區", "DIST_TXG_SOUTH"),
    ("西區", "DIST_TXG_WEST"),
    ("北區", "DIST_TXG_NORTH"),
    ("北屯區", "DIST_TXG_BEITUN"),
    ("西屯區", "DIST_TXG_XITUN"),
    ("南屯區", "DIST_TXG_NANTUN"),
    ("太平區", "DIST_TXG_TAIPING"),
    ("大里區", "DIST_TXG_DALI"),
    ("霧峰區", "DIST_TXG_WUFENG"),
    ("烏日區", "DIST_TXG_WURI"),
    ("豐原區", "DIST_TXG_FENGYUAN"),
    ("后里區", "DIST_TXG_HOULI"),
    ("石岡區", "DIST_TXG_SHIGANG"),
    ("東勢區", "DIST_TXG_DONGSHI"),
    ("和平區", "DIST_TXG_HEPING"),
    ("新社區", "DIST_TXG_XINSHE"),
    ("潭子區", "DIST_TXG_TANZI"),
    ("大雅區", "DIST_TXG_DAYA"),
    ("神岡區", "DIST_TXG_SHENGANG"),
    ("大肚區", "DIST_TXG_DADU"),
    ("沙鹿區", "DIST_TXG_SHALU"),
    ("龍井區", "DIST_TXG_LONGJING"),
    ("梧棲區", "DIST_TXG_WUQI"),
    ("清水區", "DIST_TXG_QINGSHUI"),
    ("大甲區", "DIST_TXG_DAJIA"),
    ("外埔區", "DIST_TXG_WAIPU"),
    ("大安區", "DIST_TXG_DAAN"),
];

const TAINAN_DISTRICTS: &[(&str, &str)] = &[
    ("中西區", "DIST_TNN_CENTRAL_WEST"),
    ("東區", "DIST_TNN_EAST"),
    ("南區", "DIST_TNN_SOUTH"),
    ("北區", "DIST_TNN_NORTH"),
    ("安平區", "DIST_TNN_ANPING"),
    ("安南區", "DIST_TNN_ANNAN"),
    ("永康區", "DIST_TNN_YONGKANG"),
    ("歸仁區", "DIST_TNN_GUIREN"),
    ("新化區", "DIST_TNN_XINHUA"),
    ("左鎮區", "DIST_TNN_ZUOZHEN"),
    ("玉井區", "DIST_TNN_YUJING"),
    ("楠西區", "DIST_TNN_NANXI"),
    ("南化區", "DIST_TNN_NANHUA"),
    ("仁德區", "DIST_TNN_RENDE"),
    ("關廟區", "DIST_TNN_GUANMIAO"),
    ("龍崎區", "DIST_TNN_LONGQI"),
    ("官田區", "DIST_TNN_GUANTIAN"),
    ("麻豆區", "DIST_TNN_MADOU"),
    ("佳里區", "DIST_TNN_JIALI"),
    ("西港區", "DIST_TNN_XIGANG"),
    ("七股區", "DIST_TNN_QIGU"),
    ("將軍區", "DIST_TNN_JIANGJUN"),
    ("學甲區", "DIST_TNN_XUEJIA"),
    ("北門區", "DIST_TNN_BEIMEN"),
    ("新營區", "DIST_TNN_XINYING"),
    ("後壁區", "DIST_TNN_HOUBI"),
    ("白河區", "DIST_TNN_BAIHE"),
    ("東山區", "DIST_TNN_DONGSHAN"),
    ("六甲區", "DIST_TNN_LIUJIA"),
    ("下營區", "DIST_TNN_XIAYING"),
    ("柳營區", "DIST_TNN_LIUYING"),
    ("鹽水區", "DIST_TNN_YANSHUI"),
    ("善化區", "DIST_TNN_SHANHUA"),
    ("大內區", "DIST_TNN_DANEI"),
    ("山上區", "DIST_TNN_SHANSHANG"),
    ("新市區", "DIST_TNN_XINSHI"),
    ("安定區", "DIST_TNN_ANDING"),
];

const KAOHSIUNG_DISTRICTS: &[(&str, &str)] = &[
    ("新興區", "DIST_KHH_XINXING"),
    ("前金區", "DIST_KHH_QIANJIN"),
    ("苓雅區", "DIST_KHH_LINGYA"),
    ("鹽埕區", "DIST_KHH_YANCHENG"),
    ("鼓山區", "DIST_KHH_GUSHAN"),
    ("旗津區", "DIST_KHH_QIJIN"),
    ("前鎮區", "DIST_KHH_QIANZHEN"),
    ("三民區", "DIST_KHH_SANMIN"),
    ("楠梓區", "DIST_KHH_NANZI"),
    ("小港區", "DIST_KHH_XIAOGANG"),
    ("左營區", "DIST_KHH_ZUOYING"),
    ("仁武區", "DIST_KHH_RENWU"),
    ("大社區", "DIST_KHH_DASHE"),
    ("岡山區", "DIST_KHH_GANGSHAN"),
    ("路竹區", "DIST_KHH_LUZHU"),
    ("阿蓮區", "DIST_KHH_ALIAN"),
    ("田寮區", "DIST_KHH_TIANLIAO"),
    ("燕巢區", "DIST_KHH_YANCHAO"),
    ("橋頭區", "DIST_KHH_QIAOTOU"),
    ("梓官區", "DIST_KHH_ZIGUAN"),
    ("彌陀區", "DIST_KHH_MITUO"),
    ("永安區", "DIST_KHH_YONGAN"),
    ("湖內區", "DIST_KHH_HUNEI"),
    ("鳳山區", "DIST_KHH_FENGSHAN"),
    ("大寮區", "DIST_KHH_DALIAO"),
    ("林園區", "DIST_KHH_LINYUAN"),
    ("鳥松區", "DIST_KHH_NIAOSONG"),
    ("大樹區", "DIST_KHH_DASHU"),
    ("旗山區", "DIST_KHH_QISHAN"),
    ("美濃區", "DIST_KHH_MEINONG"),
    ("六龜區", "DIST_KHH_LIUGUI"),
    ("內門區", "DIST_KHH_NEIMEN"),
    ("杉林區", "DIST_KHH_SHANLIN"),
    ("甲仙區", "DIST_KHH_JIAXIAN"),
    ("桃源區", "DIST_KHH_TAOYUAN"),
    ("那瑪夏區", "DIST_KHH_NAMASIA"),
    ("茂林區", "DIST_KHH_MAOLIN"),
    ("茄萣區", "DIST_KHH_QIEDING"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_county_canonical_forms() {
        for (input, expected) in &[
            ("台北市", CountyCode::Taipei),
            ("新北市", CountyCode::NewTaipei),
            ("桃園市", CountyCode::Taoyuan),
            ("台中市", CountyCode::Taichung),
            ("台南市", CountyCode::Tainan),
            ("高雄市", CountyCode::Kaohsiung),
            ("基隆市", CountyCode::Keelung),
            ("新竹縣", CountyCode::HsinchuCo),
            ("連江縣", CountyCode::Lienchiang),
        ] {
            assert_eq!(lookup_county(input), Some(*expected), "input: {input}");
        }
    }

    #[test]
    fn lookup_county_pre_reorg_aliases() {
        // 改制 aliases all normalise to the post-改制 form.
        assert_eq!(lookup_county("台中縣"), Some(CountyCode::Taichung));
        assert_eq!(lookup_county("台南縣"), Some(CountyCode::Tainan));
        assert_eq!(lookup_county("高雄縣"), Some(CountyCode::Kaohsiung));
        assert_eq!(lookup_county("桃園縣"), Some(CountyCode::Taoyuan));
        assert_eq!(lookup_county("台北縣"), Some(CountyCode::NewTaipei));
        // Traditional 臺 → simplified 台.
        assert_eq!(lookup_county("臺北市"), Some(CountyCode::Taipei));
        assert_eq!(lookup_county("臺中縣"), Some(CountyCode::Taichung));
    }

    #[test]
    fn lookup_county_with_trailing_garbage_rejected() {
        // Inputs that include district suffixes are *not* counties.
        // Callers wanting peel-off behaviour should run the address
        // normalizer first.
        assert_eq!(lookup_county("台北市信義區"), None);
        assert_eq!(lookup_county("台北市junk"), None);
    }

    #[test]
    fn lookup_county_whitespace_tolerant() {
        assert_eq!(lookup_county("  台北市  "), Some(CountyCode::Taipei));
    }

    #[test]
    fn lookup_county_unknown_returns_none() {
        assert_eq!(lookup_county("Atlantis"), None);
        assert_eq!(lookup_county(""), None);
    }

    #[test]
    fn canonicalize_taipei_xinyi_district() {
        let out = canonicalize("台北市", Some("信義區"));
        assert_eq!(out.county_code, Some(CountyCode::Taipei));
        assert_eq!(out.county_name_zh, Some("台北市"));
        assert_eq!(
            out.district_code.as_code(),
            Some("DIST_TPE_XINYI"),
            "got {:?}",
            out.district_code,
        );
        assert_eq!(out.district_raw.as_deref(), Some("信義區"));
    }

    #[test]
    fn canonicalize_new_taipei_banqiao_district() {
        let out = canonicalize("新北市", Some("板橋區"));
        assert_eq!(out.county_code, Some(CountyCode::NewTaipei));
        assert_eq!(out.district_code.as_code(), Some("DIST_NTPE_BANQIAO"));
    }

    #[test]
    fn canonicalize_pre_reorg_input_normalizes() {
        // Pre-改制 county + post-改制 district name. Returned
        // county code is the post-改制 one.
        let out = canonicalize("高雄縣", Some("鳳山區"));
        assert_eq!(out.county_code, Some(CountyCode::Kaohsiung));
        assert_eq!(out.district_code.as_code(), Some("DIST_KHH_FENGSHAN"));
    }

    #[test]
    fn canonicalize_district_without_district_suffix() {
        // Some inputs omit the 區 suffix or use the pre-改制 市
        // suffix. The lookup tries both forms.
        let out_no_suffix = canonicalize("台北市", Some("信義"));
        assert_eq!(
            out_no_suffix.district_code.as_code(),
            Some("DIST_TPE_XINYI")
        );
        // 鳳山市 (pre-改制) → 鳳山區 (post-改制) in Kaohsiung.
        let out_pre_reorg = canonicalize("高雄縣", Some("鳳山市"));
        assert_eq!(
            out_pre_reorg.district_code.as_code(),
            Some("DIST_KHH_FENGSHAN")
        );
    }

    /// R4 fix: comma-only / whitespace-comma district inputs used
    /// to pass the `.trim()`-based guard but strip to `""`
    /// downstream, surfacing `district_raw: Some("")` which was
    /// misleading. Now they collapse to `district_raw: None`.
    #[test]
    fn comma_only_district_collapses_to_none() {
        for junk in [",", "，", " ,  ", "  ，  ，  "] {
            let out = canonicalize("台北市", Some(junk));
            assert_eq!(out.county_code, Some(CountyCode::Taipei));
            assert_eq!(out.district_code, DistrictCode::Unknown);
            assert_eq!(out.district_raw, None, "junk={junk:?}");
        }
    }

    #[test]
    fn canonicalize_county_only_returns_unknown_district() {
        let out = canonicalize("台北市", None);
        assert_eq!(out.county_code, Some(CountyCode::Taipei));
        assert_eq!(out.district_code, DistrictCode::Unknown);
        assert_eq!(out.district_raw, None);
    }

    #[test]
    fn canonicalize_non_municipality_returns_unknown_district() {
        // v0.1 only bakes the 6 直轄市. 新竹縣 + 竹北市 should
        // resolve the county but surface Unknown for the district.
        let out = canonicalize("新竹縣", Some("竹北市"));
        assert_eq!(out.county_code, Some(CountyCode::HsinchuCo));
        assert_eq!(out.district_code, DistrictCode::Unknown);
        // Raw input preserved for caller fallback.
        assert_eq!(out.district_raw.as_deref(), Some("竹北市"));
    }

    #[test]
    fn canonicalize_unknown_county_propagates_none() {
        let out = canonicalize("Atlantis", Some("信義區"));
        assert_eq!(out.county_code, None);
        assert_eq!(out.county_name_zh, None);
        assert_eq!(out.district_code, DistrictCode::Unknown);
        assert_eq!(out.district_raw.as_deref(), Some("信義區"));
    }

    #[test]
    fn county_code_as_code_strings_unique() {
        // Locks against accidental code collisions if someone
        // edits the enum. Lists *every* variant so a new addition
        // can't slip through with a duplicated code.
        let codes = [
            CountyCode::Taipei.as_code(),
            CountyCode::NewTaipei.as_code(),
            CountyCode::Taoyuan.as_code(),
            CountyCode::Taichung.as_code(),
            CountyCode::Tainan.as_code(),
            CountyCode::Kaohsiung.as_code(),
            CountyCode::Keelung.as_code(),
            CountyCode::Hsinchu.as_code(),
            CountyCode::Chiayi.as_code(),
            CountyCode::HsinchuCo.as_code(),
            CountyCode::Miaoli.as_code(),
            CountyCode::Changhua.as_code(),
            CountyCode::Nantou.as_code(),
            CountyCode::Yunlin.as_code(),
            CountyCode::ChiayiCo.as_code(),
            CountyCode::Pingtung.as_code(),
            CountyCode::Yilan.as_code(),
            CountyCode::Hualien.as_code(),
            CountyCode::Taitung.as_code(),
            CountyCode::Penghu.as_code(),
            CountyCode::Kinmen.as_code(),
            CountyCode::Lienchiang.as_code(),
        ];
        assert_eq!(codes.len(), 22, "all 22 counties must be listed");
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(codes.len(), unique.len(), "duplicate codes: {codes:?}");
    }

    #[test]
    fn district_codes_within_county_unique() {
        for (county, table) in [
            (CountyCode::Taipei, TAIPEI_DISTRICTS),
            (CountyCode::NewTaipei, NEW_TAIPEI_DISTRICTS),
            (CountyCode::Taoyuan, TAOYUAN_DISTRICTS),
            (CountyCode::Taichung, TAICHUNG_DISTRICTS),
            (CountyCode::Tainan, TAINAN_DISTRICTS),
            (CountyCode::Kaohsiung, KAOHSIUNG_DISTRICTS),
        ] {
            let codes: std::collections::HashSet<_> = table.iter().map(|(_, c)| *c).collect();
            assert_eq!(codes.len(), table.len(), "dup codes in {county:?}");
        }
    }

    #[test]
    fn county_aliases_re_exposes_address_module_table() {
        let aliases = county_aliases();
        assert!(aliases.iter().any(|(a, _)| *a == "台中縣"));
        assert!(aliases.iter().any(|(a, _)| *a == "臺北市"));
    }
}
