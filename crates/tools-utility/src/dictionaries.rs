//! Static lookup tables for Taiwan administrative / transport /
//! financial codes — and a generic [`Dictionary`] abstraction so
//! each category can ship two MCP tools (`get_by_id` and
//! `search`) without per-category plumbing.
//!
//! Per the #3.11 Definition of Done, v0.1 ships five categories:
//!  1. 行政區代碼 (administrative district codes from TGOS)
//!  2. MRT 站點 (台北 / 桃園 / 台中 / 高雄 systems)
//!  3. 銀行代碼 (中央銀行 published list)
//!  4. 郵遞區號 (3-digit, district granularity)
//!  5. 縣市代碼 (mirrors the 22-code list from
//!     [`crate::canonical::CountyCode`], with the same stable
//!     identifier strings — kept as a Dictionary so it ships
//!     the same `get_by_id` + `search` MCP surface as the other
//!     four categories)
//!
//! Each category bakes a representative subset rather than the
//! full government registry — the tables are pure-Rust statics
//! so any expansion is a code change. v0.2 will pull from the
//! ETL pipeline once #3.6 lands.

use serde::Serialize;

/// One row of a dictionary. The `code` is the stable identifier
/// (e.g. `"004"` for 臺灣銀行) and `name` is the zh-TW canonical
/// label. `aliases` lets a search match e.g. `"BoT"` against a
/// Chinese-named entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DictEntry {
    pub code: &'static str,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
}

/// A category of codes. `name` is the human label used in error
/// messages; `entries` is the baked table.
#[derive(Debug)]
pub struct Dictionary {
    pub name: &'static str,
    pub entries: &'static [DictEntry],
}

impl Dictionary {
    /// Exact-match lookup by code. Returns `None` if no entry
    /// matches; case-sensitive by design (codes are themselves
    /// case-sensitive — `A01` ≠ `a01`).
    #[must_use]
    pub fn get_by_id(&self, code: &str) -> Option<DictEntry> {
        self.entries.iter().copied().find(|e| e.code == code)
    }

    /// Substring search across the name + alias fields, plus
    /// exact-prefix match on the code. Returns up to `limit`
    /// entries in the order they appear in the table. The query
    /// is case-insensitive for ASCII (via `to_ascii_lowercase`
    /// applied uniformly to every field — CJK codepoints pass
    /// through unchanged since they have no case distinction).
    ///
    /// **Perf note**: allocates a lowercased `String` per
    /// entry-field on every call. Acceptable on v0.1's ≤31-entry
    /// tables (nanoseconds per search) but a hot-path concern
    /// once #3.6 ETL replaces the baked subsets with full
    /// registries. v0.2 should precompute the lowercased forms
    /// (or use a manual ASCII-case-insensitive find loop) so
    /// search stays cheap as the tables grow.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<DictEntry> {
        let query_trimmed = query.trim();
        if query_trimmed.is_empty() {
            return Vec::new();
        }
        let query_lower = query_trimmed.to_ascii_lowercase();
        let mut out = Vec::with_capacity(limit.min(self.entries.len()));
        for entry in self.entries {
            if out.len() >= limit {
                break;
            }
            // Apply the same case-fold to every field so name
            // matches behave the same way as code / alias matches.
            // `to_ascii_lowercase` is allocation-cheap for short
            // strings and doesn't touch CJK ranges.
            if entry.code.to_ascii_lowercase().starts_with(&query_lower)
                || entry.name.to_ascii_lowercase().contains(&query_lower)
                || entry
                    .aliases
                    .iter()
                    .any(|a| a.to_ascii_lowercase().contains(&query_lower))
            {
                out.push(*entry);
            }
        }
        out
    }
}

// ============================================================
//  1. 行政區代碼 — TGOS-published district codes (representative
//                  v0.1 subset; 31 entries covering the 6 直轄市:
//                  12 台北 + 7 新北 + 3 桃園 + 3 台中 + 2 台南 + 4 高雄).
// ============================================================

pub const ADMIN_DIVISIONS: Dictionary = Dictionary {
    name: "行政區代碼",
    entries: &[
        // 台北市 (code prefix 63)
        DictEntry {
            code: "63000010",
            name: "台北市中正區",
            aliases: &["Zhongzheng", "中正"],
        },
        DictEntry {
            code: "63000020",
            name: "台北市大同區",
            aliases: &["Datong", "大同"],
        },
        DictEntry {
            code: "63000030",
            name: "台北市中山區",
            aliases: &["Zhongshan", "中山"],
        },
        DictEntry {
            code: "63000040",
            name: "台北市松山區",
            aliases: &["Songshan", "松山"],
        },
        DictEntry {
            code: "63000050",
            name: "台北市大安區",
            aliases: &["Daan", "大安"],
        },
        DictEntry {
            code: "63000060",
            name: "台北市萬華區",
            aliases: &["Wanhua", "萬華"],
        },
        DictEntry {
            code: "63000070",
            name: "台北市信義區",
            aliases: &["Xinyi", "信義"],
        },
        DictEntry {
            code: "63000080",
            name: "台北市士林區",
            aliases: &["Shilin", "士林"],
        },
        DictEntry {
            code: "63000090",
            name: "台北市北投區",
            aliases: &["Beitou", "北投"],
        },
        DictEntry {
            code: "63000100",
            name: "台北市內湖區",
            aliases: &["Neihu", "內湖"],
        },
        DictEntry {
            code: "63000110",
            name: "台北市南港區",
            aliases: &["Nangang", "南港"],
        },
        DictEntry {
            code: "63000120",
            name: "台北市文山區",
            aliases: &["Wenshan", "文山"],
        },
        // 新北市 (code prefix 65) — major districts only
        DictEntry {
            code: "65000010",
            name: "新北市板橋區",
            aliases: &["Banqiao", "板橋"],
        },
        DictEntry {
            code: "65000020",
            name: "新北市三重區",
            aliases: &["Sanchong", "三重"],
        },
        DictEntry {
            code: "65000030",
            name: "新北市中和區",
            aliases: &["Zhonghe", "中和"],
        },
        DictEntry {
            code: "65000040",
            name: "新北市永和區",
            aliases: &["Yonghe", "永和"],
        },
        DictEntry {
            code: "65000050",
            name: "新北市新莊區",
            aliases: &["Xinzhuang", "新莊"],
        },
        DictEntry {
            code: "65000060",
            name: "新北市新店區",
            aliases: &["Xindian", "新店"],
        },
        DictEntry {
            code: "65000080",
            name: "新北市淡水區",
            aliases: &["Tamsui", "淡水"],
        },
        // 桃園市 (code prefix 68)
        DictEntry {
            code: "68000010",
            name: "桃園市桃園區",
            aliases: &["Taoyuan-D", "桃園"],
        },
        DictEntry {
            code: "68000020",
            name: "桃園市中壢區",
            aliases: &["Zhongli", "中壢"],
        },
        DictEntry {
            code: "68000050",
            name: "桃園市八德區",
            aliases: &["Bade", "八德"],
        },
        // 台中市 (code prefix 66)
        DictEntry {
            code: "66000020",
            name: "台中市東區",
            aliases: &["Taichung-E"],
        },
        DictEntry {
            code: "66000040",
            name: "台中市西區",
            aliases: &["Taichung-W"],
        },
        DictEntry {
            code: "66000070",
            name: "台中市西屯區",
            aliases: &["Xitun", "西屯"],
        },
        // 台南市 (code prefix 67)
        DictEntry {
            code: "67000010",
            name: "台南市中西區",
            aliases: &["Central-West", "中西"],
        },
        DictEntry {
            code: "67000050",
            name: "台南市安平區",
            aliases: &["Anping", "安平"],
        },
        // 高雄市 (code prefix 64)
        DictEntry {
            code: "64000010",
            name: "高雄市新興區",
            aliases: &["Xinxing", "新興"],
        },
        DictEntry {
            code: "64000050",
            name: "高雄市鼓山區",
            aliases: &["Gushan", "鼓山"],
        },
        DictEntry {
            code: "64000080",
            name: "高雄市三民區",
            aliases: &["Sanmin", "三民"],
        },
        DictEntry {
            code: "64000110",
            name: "高雄市左營區",
            aliases: &["Zuoying", "左營"],
        },
    ],
};

// ============================================================
//  2. MRT 站點 — Taipei / Taoyuan / Taichung / Kaohsiung systems
//                (representative v0.1 subset).
// ============================================================

pub const MRT_STATIONS: Dictionary = Dictionary {
    name: "MRT 站點",
    entries: &[
        // 台北捷運 TRTC — major interchange + downtown stations
        DictEntry {
            code: "TRTC-BL12",
            name: "台北車站",
            aliases: &["Taipei Main", "TMS"],
        },
        DictEntry {
            code: "TRTC-R10",
            name: "台北車站(紅線)",
            aliases: &["Taipei Main Red"],
        },
        DictEntry {
            code: "TRTC-BL15",
            name: "西門",
            aliases: &["Ximen", "Ximending"],
        },
        DictEntry {
            code: "TRTC-R08",
            name: "中山",
            aliases: &["Zhongshan"],
        },
        DictEntry {
            code: "TRTC-BL17",
            name: "市政府",
            aliases: &["City Hall"],
        },
        DictEntry {
            code: "TRTC-BL18",
            name: "台北101/世貿",
            aliases: &["Taipei 101", "WTC"],
        },
        DictEntry {
            code: "TRTC-R02",
            name: "象山",
            aliases: &["Xiangshan"],
        },
        DictEntry {
            code: "TRTC-R28",
            name: "淡水",
            aliases: &["Tamsui"],
        },
        DictEntry {
            code: "TRTC-G14",
            name: "松山",
            aliases: &["Songshan"],
        },
        // 桃園捷運 TYM — airport line
        DictEntry {
            code: "TYM-A1",
            name: "台北車站(機場)",
            aliases: &["Taipei Main Airport"],
        },
        DictEntry {
            code: "TYM-A12",
            name: "機場第一航廈",
            aliases: &["Airport T1"],
        },
        DictEntry {
            code: "TYM-A13",
            name: "機場第二航廈",
            aliases: &["Airport T2"],
        },
        DictEntry {
            code: "TYM-A21",
            name: "中壢",
            aliases: &["Zhongli"],
        },
        // 台中捷運 TMRT — green line
        DictEntry {
            code: "TMRT-G10",
            name: "台中高鐵",
            aliases: &["Taichung HSR"],
        },
        DictEntry {
            code: "TMRT-G14",
            name: "市政府(台中)",
            aliases: &["Taichung City Hall"],
        },
        // 高雄捷運 KRTC
        DictEntry {
            code: "KRTC-R11",
            name: "高雄車站",
            aliases: &["Kaohsiung Main"],
        },
        DictEntry {
            code: "KRTC-R09",
            name: "中央公園",
            aliases: &["Central Park"],
        },
        DictEntry {
            code: "KRTC-O5",
            name: "美麗島",
            aliases: &["Formosa Boulevard"],
        },
        DictEntry {
            code: "KRTC-O7",
            name: "文化中心",
            aliases: &["Cultural Center"],
        },
    ],
};

// ============================================================
//  3. 銀行代碼 — CBC-published 3-digit codes (representative
//                 v0.1 subset of the largest banks).
// ============================================================

pub const BANK_CODES: Dictionary = Dictionary {
    name: "銀行代碼",
    entries: &[
        DictEntry {
            code: "004",
            name: "臺灣銀行",
            aliases: &["BoT", "Bank of Taiwan"],
        },
        DictEntry {
            code: "005",
            name: "土地銀行",
            aliases: &["LBT", "Land Bank of Taiwan"],
        },
        DictEntry {
            code: "006",
            name: "合作金庫銀行",
            aliases: &["TCB", "Taiwan Cooperative Bank"],
        },
        DictEntry {
            code: "007",
            name: "第一商業銀行",
            aliases: &["FCB", "First Bank"],
        },
        DictEntry {
            code: "008",
            name: "華南商業銀行",
            aliases: &["HNCB", "Hua Nan Bank"],
        },
        DictEntry {
            code: "009",
            name: "彰化商業銀行",
            aliases: &["CCB", "Chang Hwa Bank"],
        },
        DictEntry {
            code: "011",
            name: "上海商業儲蓄銀行",
            aliases: &["SCSB"],
        },
        DictEntry {
            code: "012",
            name: "台北富邦銀行",
            aliases: &["TPBank", "Fubon"],
        },
        DictEntry {
            code: "013",
            name: "國泰世華銀行",
            aliases: &["Cathay United"],
        },
        DictEntry {
            code: "017",
            name: "兆豐國際商業銀行",
            aliases: &["Mega Bank"],
        },
        DictEntry {
            code: "021",
            name: "花旗(台灣)銀行",
            aliases: &["Citi Taiwan"],
        },
        DictEntry {
            code: "048",
            name: "王道商業銀行",
            aliases: &["O-Bank"],
        },
        DictEntry {
            code: "050",
            name: "臺灣中小企業銀行",
            aliases: &["TBB", "TaiwanSME"],
        },
        DictEntry {
            code: "052",
            name: "渣打國際商業銀行",
            aliases: &["Standard Chartered TW"],
        },
        DictEntry {
            code: "081",
            name: "匯豐(台灣)商業銀行",
            aliases: &["HSBC TW"],
        },
        DictEntry {
            code: "103",
            name: "新光商業銀行",
            aliases: &["Shin Kong"],
        },
        DictEntry {
            code: "108",
            name: "陽信商業銀行",
            aliases: &["Sunny Bank"],
        },
        DictEntry {
            code: "700",
            name: "中華郵政",
            aliases: &["Chunghwa Post", "郵局"],
        },
        DictEntry {
            code: "803",
            name: "聯邦商業銀行",
            aliases: &["Union Bank of Taiwan"],
        },
        DictEntry {
            code: "805",
            name: "遠東國際商業銀行",
            aliases: &["Far Eastern"],
        },
        DictEntry {
            code: "806",
            name: "元大商業銀行",
            aliases: &["Yuanta Bank"],
        },
        DictEntry {
            code: "807",
            name: "永豐商業銀行",
            aliases: &["Bank SinoPac"],
        },
        DictEntry {
            code: "808",
            name: "玉山商業銀行",
            // "ESUN" without the period covers the common
            // typo / branding casual form.
            aliases: &["E.SUN Bank", "ESUN"],
        },
        DictEntry {
            code: "812",
            name: "台新國際商業銀行",
            aliases: &["Taishin Bank"],
        },
        DictEntry {
            code: "822",
            name: "中國信託商業銀行",
            aliases: &["CTBC Bank"],
        },
    ],
};

// ============================================================
//  4. 郵遞區號 — 3-digit, one entry per major district. Same
//                 source as TGOS (中華郵政 published).
// ============================================================

pub const POSTAL_CODES: Dictionary = Dictionary {
    name: "郵遞區號",
    entries: &[
        DictEntry {
            code: "100",
            name: "台北市中正區",
            aliases: &["Taipei-Zhongzheng"],
        },
        DictEntry {
            code: "103",
            name: "台北市大同區",
            aliases: &["Taipei-Datong"],
        },
        DictEntry {
            code: "104",
            name: "台北市中山區",
            aliases: &["Taipei-Zhongshan"],
        },
        DictEntry {
            code: "105",
            name: "台北市松山區",
            aliases: &["Taipei-Songshan"],
        },
        DictEntry {
            code: "106",
            name: "台北市大安區",
            aliases: &["Taipei-Daan"],
        },
        DictEntry {
            code: "108",
            name: "台北市萬華區",
            aliases: &["Taipei-Wanhua"],
        },
        DictEntry {
            code: "110",
            name: "台北市信義區",
            aliases: &["Taipei-Xinyi"],
        },
        DictEntry {
            code: "111",
            name: "台北市士林區",
            aliases: &["Taipei-Shilin"],
        },
        DictEntry {
            code: "112",
            name: "台北市北投區",
            aliases: &["Taipei-Beitou"],
        },
        DictEntry {
            code: "114",
            name: "台北市內湖區",
            aliases: &["Taipei-Neihu"],
        },
        DictEntry {
            code: "115",
            name: "台北市南港區",
            aliases: &["Taipei-Nangang"],
        },
        DictEntry {
            code: "116",
            name: "台北市文山區",
            aliases: &["Taipei-Wenshan"],
        },
        DictEntry {
            code: "200",
            name: "基隆市仁愛區",
            aliases: &["Keelung-Renai"],
        },
        DictEntry {
            code: "220",
            name: "新北市板橋區",
            aliases: &["NTPE-Banqiao"],
        },
        DictEntry {
            code: "241",
            name: "新北市三重區",
            aliases: &["NTPE-Sanchong"],
        },
        DictEntry {
            code: "242",
            name: "新北市新莊區",
            aliases: &["NTPE-Xinzhuang"],
        },
        DictEntry {
            code: "300",
            name: "新竹市東區",
            aliases: &["Hsinchu-East"],
        },
        DictEntry {
            code: "320",
            name: "桃園市中壢區",
            aliases: &["Taoyuan-Zhongli"],
        },
        DictEntry {
            code: "330",
            name: "桃園市桃園區",
            aliases: &["Taoyuan-D"],
        },
        DictEntry {
            code: "400",
            name: "台中市中區",
            aliases: &["Taichung-Central"],
        },
        DictEntry {
            code: "404",
            name: "台中市北區",
            aliases: &["Taichung-North"],
        },
        DictEntry {
            code: "407",
            name: "台中市西屯區",
            aliases: &["Taichung-Xitun"],
        },
        DictEntry {
            code: "500",
            name: "彰化縣彰化市",
            aliases: &["Changhua-City"],
        },
        DictEntry {
            code: "600",
            name: "嘉義市東區",
            aliases: &["Chiayi-East"],
        },
        DictEntry {
            code: "700",
            name: "台南市中西區",
            aliases: &["Tainan-Central-West"],
        },
        DictEntry {
            code: "800",
            name: "高雄市新興區",
            aliases: &["KHH-Xinxing"],
        },
        DictEntry {
            code: "807",
            name: "高雄市三民區",
            aliases: &["KHH-Sanmin"],
        },
        DictEntry {
            code: "813",
            name: "高雄市左營區",
            aliases: &["KHH-Zuoying"],
        },
        DictEntry {
            code: "900",
            name: "屏東縣屏東市",
            aliases: &["Pingtung-City"],
        },
    ],
};

// ============================================================
//  5. 縣市代碼 — mirrors the 22 codes from
//                `canonical::CountyCode::as_code()`. Kept as a
//                separate constant (not built from the enum)
//                because Dictionary expects a static slice; the
//                two lists are kept in sync by convention. A
//                proc-macro to derive this from the enum is a
//                v0.2 refactor.
// ============================================================

pub const COUNTY_CODES: Dictionary = Dictionary {
    name: "縣市代碼",
    entries: &[
        DictEntry {
            code: "ROC_CITY_TAIPEI",
            name: "台北市",
            aliases: &["Taipei", "TPE"],
        },
        DictEntry {
            code: "ROC_CITY_NEW_TAIPEI",
            name: "新北市",
            aliases: &["New Taipei", "NTPE", "台北縣"],
        },
        DictEntry {
            code: "ROC_CITY_TAOYUAN",
            name: "桃園市",
            aliases: &["Taoyuan", "TYN", "桃園縣"],
        },
        DictEntry {
            code: "ROC_CITY_TAICHUNG",
            name: "台中市",
            aliases: &["Taichung", "TXG", "台中縣"],
        },
        DictEntry {
            code: "ROC_CITY_TAINAN",
            name: "台南市",
            aliases: &["Tainan", "TNN", "台南縣"],
        },
        DictEntry {
            code: "ROC_CITY_KAOHSIUNG",
            name: "高雄市",
            aliases: &["Kaohsiung", "KHH", "高雄縣"],
        },
        DictEntry {
            code: "ROC_CITY_KEELUNG",
            name: "基隆市",
            aliases: &["Keelung", "KEE"],
        },
        DictEntry {
            code: "ROC_CITY_HSINCHU",
            name: "新竹市",
            aliases: &["Hsinchu City", "HSZ"],
        },
        DictEntry {
            code: "ROC_CITY_CHIAYI",
            name: "嘉義市",
            aliases: &["Chiayi City"],
        },
        DictEntry {
            code: "ROC_COUNTY_HSINCHU",
            name: "新竹縣",
            aliases: &["Hsinchu County"],
        },
        DictEntry {
            code: "ROC_COUNTY_MIAOLI",
            name: "苗栗縣",
            aliases: &["Miaoli"],
        },
        DictEntry {
            code: "ROC_COUNTY_CHANGHUA",
            name: "彰化縣",
            aliases: &["Changhua"],
        },
        DictEntry {
            code: "ROC_COUNTY_NANTOU",
            name: "南投縣",
            aliases: &["Nantou"],
        },
        DictEntry {
            code: "ROC_COUNTY_YUNLIN",
            name: "雲林縣",
            aliases: &["Yunlin"],
        },
        DictEntry {
            code: "ROC_COUNTY_CHIAYI",
            name: "嘉義縣",
            aliases: &["Chiayi County"],
        },
        DictEntry {
            code: "ROC_COUNTY_PINGTUNG",
            name: "屏東縣",
            aliases: &["Pingtung"],
        },
        DictEntry {
            code: "ROC_COUNTY_YILAN",
            name: "宜蘭縣",
            aliases: &["Yilan"],
        },
        DictEntry {
            code: "ROC_COUNTY_HUALIEN",
            name: "花蓮縣",
            aliases: &["Hualien"],
        },
        DictEntry {
            code: "ROC_COUNTY_TAITUNG",
            name: "台東縣",
            aliases: &["Taitung", "臺東縣"],
        },
        DictEntry {
            code: "ROC_COUNTY_PENGHU",
            name: "澎湖縣",
            aliases: &["Penghu"],
        },
        DictEntry {
            code: "ROC_COUNTY_KINMEN",
            name: "金門縣",
            aliases: &["Kinmen"],
        },
        DictEntry {
            code: "ROC_COUNTY_LIENCHIANG",
            name: "連江縣",
            aliases: &["Lienchiang", "Matsu"],
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_get_by_id_known() {
        let e = ADMIN_DIVISIONS.get_by_id("63000070").unwrap();
        assert_eq!(e.name, "台北市信義區");
    }

    #[test]
    fn admin_get_by_id_unknown_returns_none() {
        assert!(ADMIN_DIVISIONS.get_by_id("00000000").is_none());
    }

    #[test]
    fn admin_search_matches_chinese_substring() {
        let hits = ADMIN_DIVISIONS.search("信義", 10);
        assert!(hits.iter().any(|e| e.code == "63000070"));
    }

    #[test]
    fn admin_search_matches_alias() {
        let hits = ADMIN_DIVISIONS.search("Banqiao", 10);
        assert!(hits.iter().any(|e| e.code == "65000010"));
    }

    #[test]
    fn admin_search_respects_limit() {
        let hits = ADMIN_DIVISIONS.search("台北市", 3);
        assert!(hits.len() <= 3);
    }

    #[test]
    fn admin_search_empty_query_returns_empty() {
        assert!(ADMIN_DIVISIONS.search("", 10).is_empty());
        assert!(ADMIN_DIVISIONS.search("   ", 10).is_empty());
    }

    #[test]
    fn mrt_search_finds_taipei_main() {
        let hits = MRT_STATIONS.search("Taipei Main", 5);
        assert!(!hits.is_empty(), "should find Taipei Main");
        assert!(hits.iter().any(|e| e.code.starts_with("TRTC")));
    }

    #[test]
    fn mrt_get_by_id_known() {
        let e = MRT_STATIONS.get_by_id("TRTC-BL15").unwrap();
        assert_eq!(e.name, "西門");
    }

    #[test]
    fn bank_get_by_id_known() {
        let e = BANK_CODES.get_by_id("822").unwrap();
        assert_eq!(e.name, "中國信託商業銀行");
    }

    #[test]
    fn bank_search_alias_case_insensitive() {
        let hits = BANK_CODES.search("ctbc", 5);
        assert!(hits.iter().any(|e| e.code == "822"));
    }

    #[test]
    fn bank_search_chinese_name() {
        let hits = BANK_CODES.search("玉山", 5);
        assert!(hits.iter().any(|e| e.code == "808"));
    }

    #[test]
    fn postal_get_by_id_three_digit() {
        let e = POSTAL_CODES.get_by_id("110").unwrap();
        assert_eq!(e.name, "台北市信義區");
    }

    #[test]
    fn postal_search_district_name() {
        let hits = POSTAL_CODES.search("信義", 5);
        assert!(hits.iter().any(|e| e.code == "110"));
    }

    #[test]
    fn county_get_by_id_known() {
        let e = COUNTY_CODES.get_by_id("ROC_CITY_NEW_TAIPEI").unwrap();
        assert_eq!(e.name, "新北市");
    }

    #[test]
    fn county_search_pre_reorg_alias() {
        // 台北縣 is an alias on the NEW_TAIPEI entry.
        let hits = COUNTY_CODES.search("台北縣", 5);
        assert!(hits.iter().any(|e| e.code == "ROC_CITY_NEW_TAIPEI"));
    }

    #[test]
    fn search_by_code_prefix() {
        // 客戶 wants to search by "8" should find banks starting
        // with "8" via the code-prefix branch.
        let hits = BANK_CODES.search("80", 50);
        assert!(hits.iter().all(|e| e.code.starts_with("80")));
        assert!(!hits.is_empty());
    }

    /// R3 fix: `COUNTY_CODES` is hand-mirrored from
    /// `canonical::CountyCode::as_code()`. This test enumerates
    /// every `CountyCode` variant and asserts each appears exactly
    /// once in the dictionary so a future edit to either side
    /// can't silently drift.
    #[test]
    fn county_codes_dictionary_matches_canonical_county_code_enum() {
        use crate::canonical::CountyCode;
        let enum_codes: std::collections::HashSet<&'static str> = [
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
        ]
        .into_iter()
        .collect();
        let dict_codes: std::collections::HashSet<&'static str> =
            COUNTY_CODES.entries.iter().map(|e| e.code).collect();
        assert_eq!(
            enum_codes.len(),
            22,
            "expected 22 enum variants, this test must enumerate them all",
        );
        assert_eq!(
            dict_codes, enum_codes,
            "COUNTY_CODES dictionary drifted from CountyCode enum — sync them",
        );
        assert_eq!(
            COUNTY_CODES.entries.len(),
            22,
            "COUNTY_CODES has a duplicate or missing entry",
        );
    }

    #[test]
    fn entries_within_category_have_unique_codes() {
        for dict in [
            &ADMIN_DIVISIONS,
            &MRT_STATIONS,
            &BANK_CODES,
            &POSTAL_CODES,
            &COUNTY_CODES,
        ] {
            let codes: std::collections::HashSet<_> = dict.entries.iter().map(|e| e.code).collect();
            assert_eq!(
                codes.len(),
                dict.entries.len(),
                "duplicate codes in {}",
                dict.name,
            );
        }
    }
}
