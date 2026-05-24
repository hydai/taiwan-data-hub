/* Sample data for the Company 360 playground.
 *
 * Shipped as a .js file (not .json) and loaded via <script src> so
 * it lands inside the iframe under CSP `script-src 'self'`. A
 * `fetch('./sample-data.json')` would be blocked by the
 * framework's `connect-src 'none'` — playgrounds may only reach
 * the network through the parent's `tdh.fetch` proxy, and the
 * proxy only forwards `/api/v1/*`. Static co-shipped data takes
 * the script-tag escape hatch.
 *
 * The shape mirrors what a live `join_datasets` MCP call against
 * MOEA registry × Judicial Yuan cases × Public-Construction
 * Commission procurement would return, so swapping mock for real
 * data later is a one-file edit.
 *
 * Registry values (name, address, capital, established date) are
 * drawn from each company's public business-registry record.
 * Judicial cases + procurement awards are CLEARLY-LABELLED demo
 * stubs — the case numbers start with `示範-`, the agencies are
 * prefixed with `(示範)`, and a disclaimer banner is shown on
 * every render. Reviewers should NOT mistake this for real
 * litigation or contract data; production deployments need the
 * Judicial Yuan public-case API and the e-Procurement dataset
 * loaded.
 */

window.__COMPANY_360_DATA__ = {
	_meta: {
		schema_version: 1,
		'real_data_disclaimer_zh-TW':
			'司法案件與政府採購欄位皆為示範用樣本,僅顯示資料結構與面板配置;正式部署應連線至載入司法院案件公開 API 與政府電子採購網資料的後端。',
		real_data_disclaimer_en:
			'Judicial case and government procurement entries are illustrative samples — they show the data shape and panel layout only. Production deployments must connect to a backend with the Judicial Yuan public case API and the Government e-Procurement dataset loaded.'
	},
	companies: {
		'22099131': {
			tax_id: '22099131',
			registry: {
				name: '台灣積體電路製造股份有限公司',
				name_en: 'Taiwan Semiconductor Manufacturing Company Limited',
				registered_address: '新竹市東區力行六路 8 號',
				capital_twd: 259303804580,
				established_date: '1987-02-21',
				representative: '魏哲家',
				business_status: '核准設立'
			},
			judicial_cases: [
				{
					case_no: '示範-民事-2024-001',
					court: '智慧財產及商業法院',
					case_type: '智慧財產民事',
					filed_date: '2024-03-15',
					status: '結案',
					summary: '示範案件 — 用於展示面板配置,非真實案件。'
				},
				{
					case_no: '示範-行政-2024-002',
					court: '最高行政法院',
					case_type: '稅務行政',
					filed_date: '2024-08-02',
					status: '審理中',
					summary: '示範案件 — 用於展示面板配置,非真實案件。'
				}
			],
			procurement_awards: [
				{
					tender_id: '示範-A-2024-100',
					agency: '(示範)經濟部',
					subject: '示範案 — 半導體製程設備採購',
					award_date: '2024-05-10',
					amount_twd: 480000000
				}
			]
		},
		'04541302': {
			tax_id: '04541302',
			registry: {
				name: '鴻海精密工業股份有限公司',
				name_en: 'Hon Hai Precision Industry Co., Ltd.',
				registered_address: '新北市土城區自由街 2 號',
				capital_twd: 138627000000,
				established_date: '1974-02-20',
				representative: '劉揚偉',
				business_status: '核准設立'
			},
			judicial_cases: [
				{
					case_no: '示範-民事-2023-014',
					court: '臺灣高等法院',
					case_type: '勞動民事',
					filed_date: '2023-11-04',
					status: '結案',
					summary: '示範案件 — 用於展示面板配置,非真實案件。'
				}
			],
			procurement_awards: [
				{
					tender_id: '示範-B-2024-205',
					agency: '(示範)交通部',
					subject: '示範案 — 通訊設備統包採購',
					award_date: '2024-09-22',
					amount_twd: 215000000
				},
				{
					tender_id: '示範-B-2023-198',
					agency: '(示範)國防部',
					subject: '示範案 — 伺服器主機採購',
					award_date: '2023-12-08',
					amount_twd: 87500000
				}
			]
		},
		'22555003': {
			tax_id: '22555003',
			registry: {
				name: '統一企業股份有限公司',
				name_en: 'Uni-President Enterprises Corp.',
				registered_address: '臺南市永康區中正路 301 號',
				capital_twd: 56820000000,
				established_date: '1967-07-01',
				representative: '羅智先',
				business_status: '核准設立'
			},
			judicial_cases: [
				{
					case_no: '示範-民事-2024-051',
					court: '臺灣臺南地方法院',
					case_type: '消費者保護',
					filed_date: '2024-02-12',
					status: '結案',
					summary: '示範案件 — 用於展示面板配置,非真實案件。'
				}
			],
			procurement_awards: []
		},
		'96979933': {
			tax_id: '96979933',
			registry: {
				name: '中華電信股份有限公司',
				name_en: 'Chunghwa Telecom Co., Ltd.',
				registered_address: '臺北市中正區信義路一段 21-3 號',
				capital_twd: 77574000000,
				established_date: '1996-07-01',
				representative: '簡志誠',
				business_status: '核准設立'
			},
			judicial_cases: [
				{
					case_no: '示範-行政-2024-077',
					court: '最高行政法院',
					case_type: '電信管理',
					filed_date: '2024-06-18',
					status: '審理中',
					summary: '示範案件 — 用於展示面板配置,非真實案件。'
				}
			],
			procurement_awards: [
				{
					tender_id: '示範-C-2024-310',
					agency: '(示範)財政部',
					subject: '示範案 — 公務雲端服務年度授權',
					award_date: '2024-04-15',
					amount_twd: 56000000
				}
			]
		}
	}
};
