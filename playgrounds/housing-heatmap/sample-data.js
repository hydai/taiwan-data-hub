/* Sample data for the Housing Heatmap playground.
 *
 * Shipped as a .js file (loaded via <script src> from index.html
 * to satisfy CSP `script-src 'self'`; `fetch('./sample-data.json')`
 * would be blocked by `connect-src 'none'`).
 *
 * Layout reuses the same 6×14 tile cartogram as the Taiwan Map
 * playground so users see a familiar geography. Each county
 * carries:
 *   - row/col           — grid placement (same as #6.6)
 *   - price_per_ping    — keyed by year (2012-2024); median sale
 *                          price in NT$ per ping. Numbers are
 *                          SYNTHETIC, calibrated to be in the
 *                          right ballpark per Sinyi / 內政部不動產
 *                          交易實價登錄統計(e.g. 台北高、宜花東低
 *                          的相對排序保留, but absolute values
 *                          should not be used for any decision).
 *   - txns              — three illustrative transaction stubs
 *                          for the drill-down panel (district +
 *                          date + price); date format YYYY-MM.
 *
 * Disclaimer banner on every render makes the synthetic nature
 * explicit.
 */

(function () {
	'use strict';

	var YEARS = [2012, 2013, 2014, 2015, 2016, 2017, 2018, 2019, 2020, 2021, 2022, 2023, 2024];

	// Base price per ping (NT$) in 2024 — synthetic but calibrated
	// to public-source order-of-magnitude per county.
	var BASE_2024 = {
		TPE: 950000, // 台北市
		NWT: 580000,
		KLU: 280000,
		YLN: 240000,
		TYN: 360000,
		HCT: 470000,
		HCQ: 380000,
		HUA: 220000,
		MIA: 170000,
		TXG: 380000,
		NTU: 130000,
		CWH: 180000,
		YUN: 130000,
		CYQ: 130000,
		CYI: 200000,
		TTT: 160000,
		TNN: 240000,
		KHH: 280000,
		PIF: 130000,
		PEH: 180000,
		KMN: 130000,
		LCQ: 110000
	};

	// Deterministic PRNG so the chart is byte-stable across builds.
	var seed = 24681012;
	function rand() {
		seed = (seed * 9301 + 49297) % 233280;
		return seed / 233280;
	}

	function buildSeries(code) {
		var base2024 = BASE_2024[code] || 200000;
		// 2024-back-projection: ~6 % nominal growth/year compounded;
		// 2012 starting around 0.55× of 2024 + small jitter.
		var out = {};
		for (var i = 0; i < YEARS.length; i += 1) {
			var y = YEARS[i];
			var yrsFrom2012 = y - 2012;
			var growth = Math.pow(1.06, yrsFrom2012 - 12); // negative for early years
			var jitter = 0.96 + rand() * 0.08;
			out[y] = Math.round(base2024 * growth * jitter);
		}
		return out;
	}

	var COUNTIES = [
		{ code: 'LCQ', name: '連江縣', row: 1, col: 6 },
		{ code: 'KMN', name: '金門縣', row: 2, col: 6 },
		{ code: 'KLU', name: '基隆市', row: 3, col: 3 },
		{ code: 'YLN', name: '宜蘭縣', row: 3, col: 5 },
		{ code: 'TPE', name: '臺北市', row: 4, col: 3 },
		{ code: 'NWT', name: '新北市', row: 4, col: 2 },
		{ code: 'TYN', name: '桃園市', row: 5, col: 2 },
		{ code: 'HCT', name: '新竹市', row: 6, col: 2 },
		{ code: 'HCQ', name: '新竹縣', row: 6, col: 3 },
		{ code: 'HUA', name: '花蓮縣', row: 6, col: 5 },
		{ code: 'MIA', name: '苗栗縣', row: 7, col: 2 },
		{ code: 'TXG', name: '臺中市', row: 8, col: 2 },
		{ code: 'NTU', name: '南投縣', row: 8, col: 4 },
		{ code: 'CWH', name: '彰化縣', row: 9, col: 2 },
		{ code: 'YUN', name: '雲林縣', row: 10, col: 2 },
		{ code: 'CYQ', name: '嘉義縣', row: 11, col: 2 },
		{ code: 'CYI', name: '嘉義市', row: 11, col: 3 },
		{ code: 'TTT', name: '臺東縣', row: 11, col: 5 },
		{ code: 'TNN', name: '臺南市', row: 12, col: 2 },
		{ code: 'KHH', name: '高雄市', row: 13, col: 2 },
		{ code: 'PIF', name: '屏東縣', row: 14, col: 2 },
		{ code: 'PEH', name: '澎湖縣', row: 14, col: 6 }
	];

	function sampleTxns(code, name) {
		// Three illustrative sample-transaction stubs per county for
		// the drill-down panel. District names are deliberately
		// generic / illustrative (e.g. "示範一段") to make clear
		// these aren't real registry rows.
		var seriesAvg = (BASE_2024[code] || 200000) / 10000; // NT$萬 per ping
		return [
			{ date: '2024-06', district: name.replace(/[市縣]$/, '') + '示範一段', ping: 28.5, total_ntd_wan: Math.round(seriesAvg * 28.5) },
			{ date: '2024-03', district: name.replace(/[市縣]$/, '') + '示範二段', ping: 35.2, total_ntd_wan: Math.round(seriesAvg * 35.2) },
			{ date: '2023-11', district: name.replace(/[市縣]$/, '') + '示範三段', ping: 22.0, total_ntd_wan: Math.round(seriesAvg * 22.0) }
		];
	}

	var byCode = {};
	for (var i = 0; i < COUNTIES.length; i += 1) {
		var c = COUNTIES[i];
		byCode[c.code] = {
			code: c.code,
			name: c.name,
			row: c.row,
			col: c.col,
			price_per_ping: buildSeries(c.code),
			txns: sampleTxns(c.code, c.name)
		};
	}

	window.__HOUSING_HEATMAP_DATA__ = {
		_meta: {
			schema_version: 1,
			years: YEARS,
			unit: 'NT$/ping (median)',
			'real_data_disclaimer_zh-TW':
				'熱圖數值與下鑽交易皆為依公開均值生成的合成樣本,並非真實實價登錄紀錄。正式部署需連線至載入內政部不動產交易實價登錄資料的後端。',
			real_data_disclaimer_en:
				'Heatmap values and drill-down transactions are synthetic samples calibrated to public averages, NOT real registry records. Production deployments must connect to a backend with the MOI real-estate transaction registry loaded.'
		},
		counties: COUNTIES,
		by_code: byCode
	};
})();
