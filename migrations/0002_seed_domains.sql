-- M0 #0.8 — seed the 20 marketplace domains.
--
-- GENERATED from config/domains.yaml — do not edit by hand.
-- Re-run scripts/regen-domain-seed.py after editing the YAML.
--
-- Inserts are idempotent (ON CONFLICT … DO UPDATE) so re-running
-- this migration won't duplicate rows. If you actually need to
-- delete a domain, write a follow-up migration that does so explicitly.

INSERT INTO domains (slug, kind, sort_order, name_i18n, description_i18n) VALUES
    ('realestate-land', 'topical', 10, $json${"zh-TW": "不動產與土地", "en": "Real estate & land"}$json$::jsonb, $json${"zh-TW": "實價登錄、地價、土地公告現值", "en": "Transaction prices, land valuations, public-notice values"}$json$::jsonb),
    ('economy-business', 'topical', 20, $json${"zh-TW": "經濟與產業", "en": "Economy & industry"}$json$::jsonb, $json${"zh-TW": "公司登記、產業統計、進出口", "en": "Company registry, industry stats, import/export"}$json$::jsonb),
    ('procurement-subsidy', 'topical', 30, $json${"zh-TW": "政府採購與補助", "en": "Procurement & subsidies"}$json$::jsonb, $json${"zh-TW": "政府採購得標、補助核定", "en": "Awarded contracts, granted subsidies"}$json$::jsonb),
    ('public-finance', 'topical', 40, $json${"zh-TW": "公共財政", "en": "Public finance"}$json$::jsonb, $json${"zh-TW": "預算、決算、債務", "en": "Budgets, accounts, public debt"}$json$::jsonb),
    ('tax-revenue', 'topical', 50, $json${"zh-TW": "稅收", "en": "Tax revenue"}$json$::jsonb, $json${"zh-TW": "各稅目徵起、退稅、欠稅", "en": "Tax collections, refunds, arrears"}$json$::jsonb),
    ('transport', 'topical', 60, $json${"zh-TW": "交通", "en": "Transport"}$json$::jsonb, $json${"zh-TW": "公路、鐵路、空運、海運", "en": "Roads, rail, aviation, maritime"}$json$::jsonb),
    ('public-safety', 'topical', 70, $json${"zh-TW": "公共安全", "en": "Public safety"}$json$::jsonb, $json${"zh-TW": "警政、消防、災害統計", "en": "Police, fire, disaster statistics"}$json$::jsonb),
    ('judicial-legal', 'topical', 80, $json${"zh-TW": "司法與法務", "en": "Judicial & legal"}$json$::jsonb, $json${"zh-TW": "法院裁判、檢察統計", "en": "Court rulings, prosecution stats"}$json$::jsonb),
    ('legislature', 'topical', 90, $json${"zh-TW": "立法", "en": "Legislature"}$json$::jsonb, $json${"zh-TW": "議案、發言、表決", "en": "Bills, speeches, votes"}$json$::jsonb),
    ('health-food', 'topical', 100, $json${"zh-TW": "衛福與食安", "en": "Health & food safety"}$json$::jsonb, $json${"zh-TW": "醫療、健保、食品稽查", "en": "Medical care, NHI, food inspection"}$json$::jsonb),
    ('environment', 'topical', 110, $json${"zh-TW": "環境", "en": "Environment"}$json$::jsonb, $json${"zh-TW": "空品、水質、廢棄物", "en": "Air quality, water, waste"}$json$::jsonb),
    ('education-research', 'topical', 120, $json${"zh-TW": "教育與研究", "en": "Education & research"}$json$::jsonb, $json${"zh-TW": "各級學校、學位、研究經費", "en": "Schools, degrees, R&D funding"}$json$::jsonb),
    ('agriculture-fisheries', 'topical', 130, $json${"zh-TW": "農林漁牧", "en": "Agriculture & fisheries"}$json$::jsonb, $json${"zh-TW": "農產、漁業、林業", "en": "Farm products, fisheries, forestry"}$json$::jsonb),
    ('labor-employment', 'topical', 140, $json${"zh-TW": "勞動與就業", "en": "Labor & employment"}$json$::jsonb, $json${"zh-TW": "薪資、失業、勞動統計", "en": "Wages, unemployment, labor stats"}$json$::jsonb),
    ('social-population', 'topical', 150, $json${"zh-TW": "社會與人口", "en": "Social & population"}$json$::jsonb, $json${"zh-TW": "戶籍、出生、婚姻", "en": "Household registry, births, marriages"}$json$::jsonb),
    ('culture-tourism-sport', 'topical', 160, $json${"zh-TW": "文化、觀光、體育", "en": "Culture, tourism, sport"}$json$::jsonb, $json${"zh-TW": "文化資產、遊客、賽事", "en": "Heritage, tourism, sporting events"}$json$::jsonb),
    ('foreign-affairs', 'topical', 170, $json${"zh-TW": "外交與國際", "en": "Foreign affairs"}$json$::jsonb, $json${"zh-TW": "邦交、援外、簽證", "en": "Diplomatic ties, aid, visas"}$json$::jsonb),
    ('gov-publication', 'meta', 200, $json${"zh-TW": "政府刊物", "en": "Government publications"}$json$::jsonb, $json${"zh-TW": "公報、年鑑、白皮書", "en": "Gazettes, yearbooks, white papers"}$json$::jsonb),
    ('geo-basemap', 'horizontal', 300, $json${"zh-TW": "地理底圖", "en": "Geographic basemaps"}$json$::jsonb, $json${"zh-TW": "行政區界、地形、地名", "en": "Admin boundaries, terrain, gazetteers"}$json$::jsonb),
    ('utilities-telecom', 'horizontal', 310, $json${"zh-TW": "公用事業與電信", "en": "Utilities & telecom"}$json$::jsonb, $json${"zh-TW": "電力、自來水、通訊", "en": "Electricity, water, telecom"}$json$::jsonb)
ON CONFLICT (slug) DO UPDATE SET
    kind             = EXCLUDED.kind,
    sort_order       = EXCLUDED.sort_order,
    name_i18n        = EXCLUDED.name_i18n,
    description_i18n = EXCLUDED.description_i18n;
