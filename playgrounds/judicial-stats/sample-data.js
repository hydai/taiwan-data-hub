/* Sample dataset for the Judicial Stats playground.
 *
 * Shipped as a .js file (loaded via <script src> from index.html
 * to satisfy CSP `script-src 'self'`; a `fetch()` of a sibling
 * .json would hit `connect-src 'none'`).
 *
 * Shape mirrors what an `aggregate_dataset(group_by=[court,
 * case_type, year])` MCP call against the Judicial Yuan
 * statistics dataset would return — a flat array of
 * `{court, case_type, year, count}` rows, ready to slice on the
 * client. Swapping mock for live data is a one-file edit.
 *
 * IMPORTANT: the count values are SYNTHETIC. They follow a
 * realistic-looking pattern (民事 dominant, 刑事 second, 行政
 * smaller; some year-over-year growth in 智財 reflecting the
 * sector trend) but are NOT real Judicial Yuan figures. A
 * banner on every render makes this explicit.
 */

(function () {
	'use strict';

	var COURTS = [
		'臺灣臺北地方法院',
		'臺灣士林地方法院',
		'臺灣高等法院',
		'臺灣高雄地方法院',
		'智慧財產及商業法院',
		'最高法院'
	];
	var CASE_TYPES = ['民事', '刑事', '行政', '智財', '家事'];
	var YEARS = [2020, 2021, 2022, 2023, 2024];

	// Per-court / per-type base counts (synthetic, illustrative).
	// Use a deterministic PRNG seed so the same checkout produces
	// the same chart — reproducible review.
	var seed = 1234567;
	function rand() {
		seed = (seed * 9301 + 49297) % 233280;
		return seed / 233280;
	}

	var baseCountByCourtType = {};
	for (var i = 0; i < COURTS.length; i += 1) {
		baseCountByCourtType[COURTS[i]] = {};
		for (var j = 0; j < CASE_TYPES.length; j += 1) {
			var court = COURTS[i];
			var caseType = CASE_TYPES[j];
			// Scale: district courts handle high volumes; high court /
			// supreme court handle review-only and are smaller;
			// specialised courts are smaller still. 民事 / 刑事
			// dominate at district level; 行政 / 智財 / 家事 are
			// smaller and more specialised.
			var courtScale =
				court.indexOf('地方') >= 0
					? 1.0
					: court.indexOf('高等') >= 0
						? 0.4
						: court.indexOf('最高') >= 0
							? 0.15
							: 0.25; // 智財 / 家事 specialised
			var typeScale = caseType === '民事' ? 1.0
				: caseType === '刑事' ? 0.7
				: caseType === '家事' ? 0.3
				: caseType === '行政' ? 0.2
				: 0.15;
			var base = Math.round(8000 * courtScale * typeScale + rand() * 800);
			baseCountByCourtType[court][caseType] = base;
		}
	}

	var rows = [];
	for (var ci = 0; ci < COURTS.length; ci += 1) {
		for (var ti = 0; ti < CASE_TYPES.length; ti += 1) {
			for (var yi = 0; yi < YEARS.length; yi += 1) {
				var c = COURTS[ci];
				var t = CASE_TYPES[ti];
				var y = YEARS[yi];
				var base = baseCountByCourtType[c][t];
				// Trend: 智財 + 5% YoY (sector growth), 刑事 mostly
				// flat, others -1%..+2% year-on-year jitter.
				var yearsFromBase = y - YEARS[0];
				var growthRate = t === '智財' ? 0.05 : t === '刑事' ? 0 : 0.01;
				var trend = Math.pow(1 + growthRate, yearsFromBase);
				var jitter = 0.95 + rand() * 0.1;
				var count = Math.max(0, Math.round(base * trend * jitter));
				rows.push({ court: c, case_type: t, year: y, count: count });
			}
		}
	}

	window.__JUDICIAL_STATS_DATA__ = {
		_meta: {
			schema_version: 1,
			courts: COURTS,
			case_types: CASE_TYPES,
			years: YEARS,
			'real_data_disclaimer_zh-TW':
				'本面板的所有數字皆為遵循合理量級的合成樣本,並非真實司法院統計。正式部署需連線至載入司法院統計室開放資料的後端。',
			real_data_disclaimer_en:
				'Every number on this panel is a synthetic sample within realistic magnitudes — NOT real Judicial Yuan statistics. Production deployments must connect to a backend with the Judicial Yuan open-statistics dataset loaded.'
		},
		rows: rows
	};
})();
