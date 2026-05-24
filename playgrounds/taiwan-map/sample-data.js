/* Sample data for the Taiwan Map playground.
 *
 * Shipped as a .js file (loaded via <script src> from index.html
 * to satisfy CSP `script-src 'self'`; `fetch('./sample-data.json')`
 * would be blocked by `connect-src 'none'`).
 *
 * Population figures are 2024 figures from the Ministry of the
 * Interior monthly statistics report (rounded to the nearest
 * person). Area figures are from each county's official profile.
 * Density = population / area_km2 (computed at render time).
 *
 * `tile_layout` is a 6-column × 14-row cartogram — NOT a real
 * geographic projection. Each county sits in a row roughly
 * matching its north-to-south position on the main island
 * (rows 1-14); outer islands cluster in column 6. The trade-off:
 * we lose cartographic accuracy but the bundle stays under 5 KB,
 * which keeps the playground snappy under the framework's
 * Lighthouse budget. A real geo_basemap render would use
 * MapLibre with the full township-level boundary vectors loaded
 * from the gateway — out of scope for this self-contained demo.
 */

window.__TAIWAN_MAP_DATA__ = {
	_meta: {
		schema_version: 1,
		population_source: '內政部戶政司 2024 年底人口統計',
		area_source: '各縣市政府公告土地面積',
		layout_kind: 'tile_cartogram',
		'real_data_disclaimer_zh-TW':
			'地圖以「方塊型示意圖 (tile cartogram)」呈現,並非真實地理投影;人口與面積數字為公開官方資料。正式版以 368 個鄉鎮區為單位需連線至載入完整邊界向量的後端 (MapLibre GL + geo_basemap MCP 工具)。',
		real_data_disclaimer_en:
			'The map uses a tile cartogram — NOT a real geographic projection. Population and area figures are official public data. A true 368-township map needs a backend with full boundary vectors loaded (MapLibre GL + geo_basemap MCP tool).'
	},
	counties: [
		// row 1: 外島 + 北端
		{ code: 'LCQ', name: '連江縣', name_en: 'Lienchiang', pop: 14068, area_km2: 28.8, row: 1, col: 6 },
		{ code: 'KMN', name: '金門縣', name_en: 'Kinmen', pop: 144283, area_km2: 151.7, row: 2, col: 6 },
		// row 3-4: 大臺北
		{ code: 'KLU', name: '基隆市', name_en: 'Keelung', pop: 359234, area_km2: 132.8, row: 3, col: 3 },
		{ code: 'YLN', name: '宜蘭縣', name_en: 'Yilan', pop: 449007, area_km2: 2143.6, row: 3, col: 5 },
		{ code: 'TPE', name: '臺北市', name_en: 'Taipei', pop: 2500164, area_km2: 271.8, row: 4, col: 3 },
		{ code: 'NWT', name: '新北市', name_en: 'New Taipei', pop: 3996829, area_km2: 2052.6, row: 4, col: 2 },
		// row 5-6
		{ code: 'TYN', name: '桃園市', name_en: 'Taoyuan', pop: 2323066, area_km2: 1221.0, row: 5, col: 2 },
		{ code: 'HCT', name: '新竹市', name_en: 'Hsinchu City', pop: 458019, area_km2: 104.2, row: 6, col: 2 },
		{ code: 'HCQ', name: '新竹縣', name_en: 'Hsinchu', pop: 591041, area_km2: 1427.6, row: 6, col: 3 },
		{ code: 'HUA', name: '花蓮縣', name_en: 'Hualien', pop: 315113, area_km2: 4628.6, row: 6, col: 5 },
		// row 7-8: 中部
		{ code: 'MIA', name: '苗栗縣', name_en: 'Miaoli', pop: 535193, area_km2: 1820.3, row: 7, col: 2 },
		{ code: 'TXG', name: '臺中市', name_en: 'Taichung', pop: 2851747, area_km2: 2214.9, row: 8, col: 2 },
		{ code: 'NTU', name: '南投縣', name_en: 'Nantou', pop: 480082, area_km2: 4106.4, row: 8, col: 4 },
		// row 9-11: 南部+東部
		{ code: 'CWH', name: '彰化縣', name_en: 'Changhua', pop: 1232089, area_km2: 1074.4, row: 9, col: 2 },
		{ code: 'YUN', name: '雲林縣', name_en: 'Yunlin', pop: 660055, area_km2: 1290.8, row: 10, col: 2 },
		{ code: 'CYQ', name: '嘉義縣', name_en: 'Chiayi', pop: 488895, area_km2: 1903.6, row: 11, col: 2 },
		{ code: 'CYI', name: '嘉義市', name_en: 'Chiayi City', pop: 263335, area_km2: 60.0, row: 11, col: 3 },
		{ code: 'TTT', name: '臺東縣', name_en: 'Taitung', pop: 207478, area_km2: 3515.3, row: 11, col: 5 },
		// row 12-14: 南端
		{ code: 'TNN', name: '臺南市', name_en: 'Tainan', pop: 1857562, area_km2: 2191.7, row: 12, col: 2 },
		{ code: 'KHH', name: '高雄市', name_en: 'Kaohsiung', pop: 2729652, area_km2: 2952.0, row: 13, col: 2 },
		{ code: 'PIF', name: '屏東縣', name_en: 'Pingtung', pop: 781945, area_km2: 2775.6, row: 14, col: 2 },
		{ code: 'PEH', name: '澎湖縣', name_en: 'Penghu', pop: 109236, area_km2: 126.9, row: 14, col: 6 }
	]
};
