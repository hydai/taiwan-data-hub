/* Taiwan Map playground — tile-cartogram + population choropleth.
 *
 * Builds a 6-col × 14-row CSS grid of <button> tiles (one per
 * county), colors each by the selected metric (density or
 * population) using a 7-step viridis-ish palette, and shows a
 * detail panel on click. Selected county + metric are pushed to
 * the framework share-link state.
 *
 * Tile-cartogram chosen over a real geographic projection so the
 * playground stays self-contained: a township-level real map
 * needs MapLibre GL + a multi-MB boundary geojson loaded from
 * the gateway. Honest trade-off documented in the page banner.
 */

(async function () {
	'use strict';

	if (!window.tdh) {
		document.body.textContent = 'Playground framework shim missing.';
		return;
	}
	var data = window.__TAIWAN_MAP_DATA__;
	if (!data || !Array.isArray(data.counties)) {
		document.body.textContent = 'sample-data.js failed to load.';
		return;
	}

	var counties = data.counties;
	var mapEl = document.getElementById('map');
	var detailEl = document.getElementById('detail');
	var legendEl = document.getElementById('legend');
	var summaryEl = document.getElementById('summary');
	var metricRadios = document.querySelectorAll('input[name="metric"]');

	// 7-step palette — light to dark; viridis-inspired but native
	// CSS colors only (no external palette dependency).
	var PALETTE = ['#f1f5f9', '#cbd5e1', '#94a3b8', '#64748b', '#475569', '#334155', '#1e293b'];

	var validMetrics = new Set(['density', 'population']);
	var selectedCounty = null;

	var initial = await window.tdh.getState();
	if (initial && typeof initial === 'object') {
		if (typeof initial.metric === 'string' && validMetrics.has(initial.metric)) {
			setRadio('metric', initial.metric);
		}
		if (typeof initial.county === 'string') {
			var match = counties.find(function (c) { return c.code === initial.county; });
			if (match) selectedCounty = match.code;
		}
	}

	buildGrid();
	metricRadios.forEach(function (r) {
		r.addEventListener('change', function () {
			recolor();
			pushState();
		});
	});

	recolor();

	function buildGrid() {
		// Clear once; tiles never need to be recreated (only re-colored).
		mapEl.textContent = '';
		for (var i = 0; i < counties.length; i += 1) {
			var c = counties[i];
			var btn = document.createElement('button');
			btn.type = 'button';
			btn.className = 'tile';
			btn.dataset.code = c.code;
			btn.style.gridColumn = String(c.col);
			btn.style.gridRow = String(c.row);
			btn.setAttribute('aria-label', c.name + ' — 人口 ' + c.pop.toLocaleString('zh-TW'));
			var label = document.createElement('span');
			label.className = 'tile-label';
			label.textContent = c.name;
			btn.appendChild(label);
			var value = document.createElement('span');
			value.className = 'tile-value';
			btn.appendChild(value);
			btn.addEventListener('click', function (ev) {
				var code = ev.currentTarget.dataset.code;
				select(code);
				pushState();
			});
			mapEl.appendChild(btn);
		}
	}

	function currentMetric() {
		return getRadio('metric') || 'density';
	}

	function metricValue(c, metric) {
		if (metric === 'population') return c.pop;
		// density
		return c.area_km2 > 0 ? c.pop / c.area_km2 : 0;
	}

	function recolor() {
		var metric = currentMetric();
		var values = counties.map(function (c) { return metricValue(c, metric); });
		var min = Math.min.apply(null, values);
		var max = Math.max.apply(null, values);
		var ticks = computeTicks(min, max, PALETTE.length);
		var tiles = mapEl.querySelectorAll('.tile');
		var minIdx = PALETTE.length - 1;
		var maxIdx = 0;
		var totalPop = 0;
		for (var i = 0; i < tiles.length; i += 1) {
			var tile = tiles[i];
			var c = counties.find(function (cc) { return cc.code === tile.dataset.code; });
			if (!c) continue;
			var v = metricValue(c, metric);
			var idx = bucketIndex(v, ticks);
			tile.style.background = PALETTE[idx];
			tile.style.color = idx >= 4 ? '#fafafa' : '#0f172a';
			var valEl = tile.querySelector('.tile-value');
			valEl.textContent = formatMetric(v, metric);
			tile.classList.toggle('selected', c.code === selectedCounty);
			totalPop += c.pop;
			if (idx < minIdx) minIdx = idx;
			if (idx > maxIdx) maxIdx = idx;
		}
		renderLegend(ticks, metric);
		summaryEl.textContent =
			'總人口 ' +
			totalPop.toLocaleString('zh-TW') +
			' 人;' +
			(metric === 'population' ? '人口數' : '人口密度') +
			'分為 ' +
			PALETTE.length +
			' 個分桶。';
		if (selectedCounty) {
			var sel = counties.find(function (cc) { return cc.code === selectedCounty; });
			if (sel) renderDetail(sel);
		}
	}

	function select(code) {
		selectedCounty = code;
		var sel = counties.find(function (cc) { return cc.code === code; });
		if (sel) renderDetail(sel);
		var tiles = mapEl.querySelectorAll('.tile');
		for (var i = 0; i < tiles.length; i += 1) {
			tiles[i].classList.toggle('selected', tiles[i].dataset.code === code);
		}
	}

	function renderDetail(c) {
		detailEl.textContent = '';
		var h = document.createElement('h2');
		h.textContent = c.name;
		detailEl.appendChild(h);
		var sub = document.createElement('p');
		sub.className = 'muted sub';
		sub.textContent = c.name_en + ' · code ' + c.code;
		detailEl.appendChild(sub);
		var dl = document.createElement('dl');
		dl.appendChild(dt('人口'));
		dl.appendChild(dd(c.pop.toLocaleString('zh-TW') + ' 人'));
		dl.appendChild(dt('面積'));
		dl.appendChild(dd(c.area_km2.toLocaleString('zh-TW') + ' km²'));
		dl.appendChild(dt('密度'));
		dl.appendChild(dd((c.pop / c.area_km2).toLocaleString('zh-TW', { maximumFractionDigits: 1 }) + ' 人/km²'));
		detailEl.appendChild(dl);
	}

	function renderLegend(ticks, metric) {
		legendEl.textContent = '';
		var label = document.createElement('span');
		label.className = 'legend-label';
		label.textContent = metric === 'population' ? '人口數' : '人口密度';
		legendEl.appendChild(label);
		for (var i = 0; i < PALETTE.length; i += 1) {
			var bin = document.createElement('span');
			bin.className = 'legend-bin';
			var swatch = document.createElement('span');
			swatch.className = 'legend-swatch';
			swatch.style.background = PALETTE[i];
			bin.appendChild(swatch);
			var t = document.createElement('span');
			t.className = 'legend-tick';
			t.textContent =
				formatMetric(ticks[i], metric) + (i < PALETTE.length - 1 ? '–' + formatMetric(ticks[i + 1], metric) : '+');
			bin.appendChild(t);
			legendEl.appendChild(bin);
		}
	}

	function formatMetric(v, metric) {
		if (metric === 'population') {
			if (v >= 1000000) return (v / 1000000).toFixed(1) + 'M';
			if (v >= 1000) return Math.round(v / 1000) + 'k';
			return String(Math.round(v));
		}
		return Math.round(v).toLocaleString('zh-TW');
	}

	function computeTicks(min, max, n) {
		// Equal-width bins between min and max. n+1 tick boundaries
		// for n buckets so the legend reads `[t0, t1) [t1, t2) …`.
		var ticks = new Array(n + 1);
		var span = max - min;
		if (span <= 0) span = 1;
		for (var i = 0; i <= n; i += 1) {
			ticks[i] = min + (span * i) / n;
		}
		return ticks;
	}

	function bucketIndex(v, ticks) {
		for (var i = 0; i < ticks.length - 1; i += 1) {
			if (v < ticks[i + 1]) return i;
		}
		return ticks.length - 2;
	}

	function getRadio(name) {
		var nodes = document.querySelectorAll('input[name="' + name + '"]');
		for (var i = 0; i < nodes.length; i += 1) if (nodes[i].checked) return nodes[i].value;
		return '';
	}
	function setRadio(name, value) {
		var nodes = document.querySelectorAll('input[name="' + name + '"]');
		for (var i = 0; i < nodes.length; i += 1) if (nodes[i].value === value) nodes[i].checked = true;
	}
	function dt(text) {
		var e = document.createElement('dt');
		e.textContent = text;
		return e;
	}
	function dd(text) {
		var e = document.createElement('dd');
		e.textContent = text;
		return e;
	}

	function pushState() {
		window.tdh.setState({
			metric: currentMetric(),
			county: selectedCounty
		});
	}
})();
