-- M0 #0.8 — seed the 20 marketplace domains.
--
-- GENERATED from config/domains.yaml — do not edit by hand.
-- Re-run scripts/regen-domain-seed.py after editing the YAML.
--
-- Inserts are idempotent (ON CONFLICT … DO UPDATE) so re-running
-- this migration won't duplicate rows. If you actually need to
-- delete a domain, write a follow-up migration that does so explicitly.

INSERT INTO domains (slug, kind, sort_order, name_i18n, description_i18n) VALUES
    ('realestate-land', 'topical', 10, '{"zh-TW": "不動產與土地", "en": "Real estate & land"}'::jsonb, '{"zh-TW": "實價登錄、地價、土地公告現值", "en": "Transaction prices, land valuations, public-notice values"}'::jsonb),
    ('economy-business', 'topical', 20, '{"zh-TW": "經濟與產業", "en": "Economy & industry"}'::jsonb, '{"zh-TW": "公司登記、產業統計、進出口", "en": "Company registry, industry stats, import/export"}'::jsonb),
    ('procurement-subsidy', 'topical', 30, '{"zh-TW": "政府採購與補助", "en": "Procurement & subsidies"}'::jsonb, '{"zh-TW": "政府採購得標、補助核定", "en": "Awarded contracts, granted subsidies"}'::jsonb),
    ('public-finance', 'topical', 40, '{"zh-TW": "公共財政", "en": "Public finance"}'::jsonb, '{"zh-TW": "預算、決算、債務", "en": "Budgets, accounts, public debt"}'::jsonb),
    ('tax-revenue', 'topical', 50, '{"zh-TW": "稅收", "en": "Tax revenue"}'::jsonb, '{"zh-TW": "各稅目徵起、退稅、欠稅", "en": "Tax collections, refunds, arrears"}'::jsonb),
    ('transport', 'topical', 60, '{"zh-TW": "交通", "en": "Transport"}'::jsonb, '{"zh-TW": "公路、鐵路、空運、海運", "en": "Roads, rail, aviation, maritime"}'::jsonb),
    ('public-safety', 'topical', 70, '{"zh-TW": "公共安全", "en": "Public safety"}'::jsonb, '{"zh-TW": "警政、消防、災害統計", "en": "Police, fire, disaster statistics"}'::jsonb),
    ('judicial-legal', 'topical', 80, '{"zh-TW": "司法與法務", "en": "Judicial & legal"}'::jsonb, '{"zh-TW": "法院裁判、檢察統計", "en": "Court rulings, prosecution stats"}'::jsonb),
    ('legislature', 'topical', 90, '{"zh-TW": "立法", "en": "Legislature"}'::jsonb, '{"zh-TW": "議案、發言、表決", "en": "Bills, speeches, votes"}'::jsonb),
    ('health-food', 'topical', 100, '{"zh-TW": "衛福與食安", "en": "Health & food safety"}'::jsonb, '{"zh-TW": "醫療、健保、食品稽查", "en": "Medical care, NHI, food inspection"}'::jsonb),
    ('environment', 'topical', 110, '{"zh-TW": "環境", "en": "Environment"}'::jsonb, '{"zh-TW": "空品、水質、廢棄物", "en": "Air quality, water, waste"}'::jsonb),
    ('education-research', 'topical', 120, '{"zh-TW": "教育與研究", "en": "Education & research"}'::jsonb, '{"zh-TW": "各級學校、學位、研究經費", "en": "Schools, degrees, R&D funding"}'::jsonb),
    ('agriculture-fisheries', 'topical', 130, '{"zh-TW": "農林漁牧", "en": "Agriculture & fisheries"}'::jsonb, '{"zh-TW": "農產、漁業、林業", "en": "Farm products, fisheries, forestry"}'::jsonb),
    ('labor-employment', 'topical', 140, '{"zh-TW": "勞動與就業", "en": "Labor & employment"}'::jsonb, '{"zh-TW": "薪資、失業、勞動統計", "en": "Wages, unemployment, labor stats"}'::jsonb),
    ('social-population', 'topical', 150, '{"zh-TW": "社會與人口", "en": "Social & population"}'::jsonb, '{"zh-TW": "戶籍、出生、婚姻", "en": "Household registry, births, marriages"}'::jsonb),
    ('culture-tourism-sport', 'topical', 160, '{"zh-TW": "文化、觀光、體育", "en": "Culture, tourism, sport"}'::jsonb, '{"zh-TW": "文化資產、遊客、賽事", "en": "Heritage, tourism, sporting events"}'::jsonb),
    ('foreign-affairs', 'topical', 170, '{"zh-TW": "外交與國際", "en": "Foreign affairs"}'::jsonb, '{"zh-TW": "邦交、援外、簽證", "en": "Diplomatic ties, aid, visas"}'::jsonb),
    ('gov-publication', 'meta', 200, '{"zh-TW": "政府刊物", "en": "Government publications"}'::jsonb, '{"zh-TW": "公報、年鑑、白皮書", "en": "Gazettes, yearbooks, white papers"}'::jsonb),
    ('geo-basemap', 'horizontal', 300, '{"zh-TW": "地理底圖", "en": "Geographic basemaps"}'::jsonb, '{"zh-TW": "行政區界、地形、地名", "en": "Admin boundaries, terrain, gazetteers"}'::jsonb),
    ('utilities-telecom', 'horizontal', 310, '{"zh-TW": "公用事業與電信", "en": "Utilities & telecom"}'::jsonb, '{"zh-TW": "電力、自來水、通訊", "en": "Electricity, water, telecom"}'::jsonb)
ON CONFLICT (slug) DO UPDATE SET
    kind             = EXCLUDED.kind,
    sort_order       = EXCLUDED.sort_order,
    name_i18n        = EXCLUDED.name_i18n,
    description_i18n = EXCLUDED.description_i18n;
