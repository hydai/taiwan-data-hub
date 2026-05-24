/* Judicial Stats playground.
 *
 * Filter controls drive a hand-rolled SVG line chart that
 * aggregates per-row case counts on the fly. Filter + group-by
 * choices are encoded into the framework share-link state, so the
 * URL fully reproduces the view.
 *
 * Why hand-rolled SVG: the framework CSP forbids loading external
 * scripts (`script-src 'self'` plus `connect-src 'none'`), so no
 * d3 / chart.js / echarts. ~80 lines of SVG generation is enough
 * for a multi-series line chart that meets the DoD without bloating
 * the playground bundle.
 */

(async function () {
	'use strict';

	if (!window.tdh) {
		document.body.textContent = 'Playground framework shim missing.';
		return;
	}
	var data = window.__JUDICIAL_STATS_DATA__;
	if (!data) {
		document.body.textContent = 'sample-data.js failed to load.';
		return;
	}

	var meta = data._meta;
	var rows = data.rows;
	var COURTS = meta.courts;
	var CASE_TYPES = meta.case_types;
	var YEARS = meta.years;

	var courtSel = document.getElementById('court');
	var typeSel = document.getElementById('case-type');
	var yearFromSel = document.getElementById('year-from');
	var yearToSel = document.getElementById('year-to');
	var groupRadios = document.querySelectorAll('input[name="group-by"]');
	var summaryEl = document.getElementById('summary');
	var chartEl = document.getElementById('chart');
	var captionEl = document.getElementById('chart-caption');
	var tableHost = document.getElementById('table-host');

	populateSelect(courtSel, COURTS);
	populateSelect(typeSel, CASE_TYPES);
	populateYearSelect(yearFromSel, YEARS, YEARS[0]);
	populateYearSelect(yearToSel, YEARS, YEARS[YEARS.length - 1]);

	// State comes from the share-link URL, which is user-controlled.
	// Validate every field against the known-good lists before
	// applying — otherwise a malformed link (e.g. year_from=0) can
	// expand the chart's year loop to thousands of buckets and
	// freeze the playground.
	var validCourts = new Set(COURTS);
	var validTypes = new Set(CASE_TYPES);
	var validYears = new Set(YEARS);
	var validGroupBy = new Set(['total', 'case_type', 'court']);

	var initial = await window.tdh.getState();
	if (initial && typeof initial === 'object') {
		if (typeof initial.court === 'string' && (initial.court === '' || validCourts.has(initial.court))) {
			courtSel.value = initial.court;
		}
		if (
			typeof initial.case_type === 'string' &&
			(initial.case_type === '' || validTypes.has(initial.case_type))
		) {
			typeSel.value = initial.case_type;
		}
		if (typeof initial.year_from === 'number' && validYears.has(initial.year_from)) {
			yearFromSel.value = String(initial.year_from);
		}
		if (typeof initial.year_to === 'number' && validYears.has(initial.year_to)) {
			yearToSel.value = String(initial.year_to);
		}
		if (typeof initial.group_by === 'string' && validGroupBy.has(initial.group_by)) {
			setRadio('group-by', initial.group_by);
		}
	}

	[courtSel, typeSel, yearFromSel, yearToSel].forEach(function (el) {
		el.addEventListener('change', onFiltersChanged);
	});
	groupRadios.forEach(function (r) {
		r.addEventListener('change', onFiltersChanged);
	});

	render();

	function onFiltersChanged() {
		render();
		window.tdh.setState(currentFilters());
	}

	function currentFilters() {
		return {
			court: courtSel.value,
			case_type: typeSel.value,
			year_from: Number(yearFromSel.value),
			year_to: Number(yearToSel.value),
			group_by: getRadio('group-by')
		};
	}

	function render() {
		var f = currentFilters();
		var yFrom = Math.min(f.year_from, f.year_to);
		var yTo = Math.max(f.year_from, f.year_to);
		var filtered = rows.filter(function (r) {
			if (f.court && r.court !== f.court) return false;
			if (f.case_type && r.case_type !== f.case_type) return false;
			if (r.year < yFrom || r.year > yTo) return false;
			return true;
		});

		var series = aggregate(filtered, f.group_by, yFrom, yTo);
		renderSummary(filtered, series, f);
		renderChart(series, yFrom, yTo);
		renderTable(series, yFrom, yTo);
	}

	/**
	 * Bucket `rows` into per-series-per-year totals.
	 *   group=total      → 1 series ("合計")
	 *   group=case_type  → one series per CASE_TYPES
	 *   group=court      → one series per COURTS
	 * Returns Map<seriesLabel, Map<year, count>>.
	 */
	function aggregate(filtered, groupBy, yFrom, yTo) {
		var series = new Map();
		function bucket(label) {
			if (!series.has(label)) {
				var perYear = new Map();
				for (var y = yFrom; y <= yTo; y += 1) perYear.set(y, 0);
				series.set(label, perYear);
			}
			return series.get(label);
		}
		for (var i = 0; i < filtered.length; i += 1) {
			var r = filtered[i];
			var label =
				groupBy === 'case_type' ? r.case_type : groupBy === 'court' ? r.court : '合計';
			var perYear = bucket(label);
			perYear.set(r.year, (perYear.get(r.year) || 0) + r.count);
		}
		// Sort labels by total descending for legend readability.
		var entries = [...series.entries()];
		entries.sort(function (a, b) {
			return seriesTotal(b[1]) - seriesTotal(a[1]);
		});
		return new Map(entries);
	}

	function seriesTotal(perYear) {
		var sum = 0;
		perYear.forEach(function (v) {
			sum += v;
		});
		return sum;
	}

	function renderSummary(filtered, series, f) {
		var total = filtered.reduce(function (a, r) {
			return a + r.count;
		}, 0);
		var seriesCount = series.size;
		summaryEl.textContent =
			'共 ' +
			filtered.length.toLocaleString('zh-TW') +
			' 列原始紀錄,聚合為 ' +
			seriesCount +
			' 條序列,累計案件量 ' +
			total.toLocaleString('zh-TW') +
			' 件 (' +
			Math.min(f.year_from, f.year_to) +
			' – ' +
			Math.max(f.year_from, f.year_to) +
			')。';
	}

	var PALETTE = ['#2563eb', '#dc2626', '#16a34a', '#f59e0b', '#7c3aed', '#0891b2'];

	function renderChart(series, yFrom, yTo) {
		var width = 720;
		var height = 320;
		var padding = { top: 16, right: 16, bottom: 32, left: 56 };
		var plotW = width - padding.left - padding.right;
		var plotH = height - padding.top - padding.bottom;

		chartEl.setAttribute('viewBox', '0 0 ' + width + ' ' + height);
		chartEl.setAttribute('preserveAspectRatio', 'xMidYMid meet');
		// Clear previous render.
		while (chartEl.firstChild) chartEl.removeChild(chartEl.firstChild);

		var years = [];
		for (var y = yFrom; y <= yTo; y += 1) years.push(y);
		if (years.length === 0) return;

		// Find max for y scale across all visible series.
		var max = 1;
		series.forEach(function (perYear) {
			perYear.forEach(function (v) {
				if (v > max) max = v;
			});
		});
		max = niceMax(max);

		// Axes.
		var axisColor = '#a1a1aa';
		appendLine(chartEl, padding.left, padding.top, padding.left, padding.top + plotH, axisColor);
		appendLine(
			chartEl,
			padding.left,
			padding.top + plotH,
			padding.left + plotW,
			padding.top + plotH,
			axisColor
		);

		// Y gridlines + labels.
		var yTicks = 4;
		for (var i = 0; i <= yTicks; i += 1) {
			var ty = padding.top + plotH - (i / yTicks) * plotH;
			var tv = Math.round((i / yTicks) * max);
			appendLine(chartEl, padding.left, ty, padding.left + plotW, ty, '#e4e4e7');
			appendText(
				chartEl,
				padding.left - 6,
				ty + 4,
				tv.toLocaleString('zh-TW'),
				'#52525b',
				'end',
				10
			);
		}

		// X labels (years).
		var step = years.length === 1 ? 0 : plotW / (years.length - 1);
		for (var xi = 0; xi < years.length; xi += 1) {
			var x = padding.left + step * xi;
			appendText(chartEl, x, padding.top + plotH + 18, String(years[xi]), '#52525b', 'middle', 11);
		}

		// One polyline + circles per series. Append order matters in
		// SVG: later siblings paint on top of earlier ones. So:
		//   gridlines + axis labels (already appended above)
		//   → polyline (paints over the grid so the trend is legible)
		//   → circles (paint over the polyline so points stay
		//     visible at line crossings)
		var seriesIdx = 0;
		series.forEach(function (perYear, label) {
			var color = PALETTE[seriesIdx % PALETTE.length];
			var pts = [];
			var circlePositions = [];
			for (var xj = 0; xj < years.length; xj += 1) {
				var year = years[xj];
				var val = perYear.get(year) || 0;
				var x = padding.left + step * xj;
				var y2 = padding.top + plotH - (val / max) * plotH;
				pts.push(x + ',' + y2);
				circlePositions.push({ x: x, y: y2 });
			}
			chartEl.appendChild(
				createSvg('polyline', {
					points: pts.join(' '),
					fill: 'none',
					stroke: color,
					'stroke-width': '2'
				})
			);
			for (var ci = 0; ci < circlePositions.length; ci += 1) {
				appendCircle(chartEl, circlePositions[ci].x, circlePositions[ci].y, 3, color);
			}
			seriesIdx += 1;
		});

		// Legend.
		captionEl.textContent = '';
		var legend = document.createElement('ul');
		legend.className = 'legend';
		var ix = 0;
		series.forEach(function (_perYear, label) {
			var li = document.createElement('li');
			var swatch = document.createElement('span');
			swatch.className = 'swatch';
			swatch.style.background = PALETTE[ix % PALETTE.length];
			li.appendChild(swatch);
			li.appendChild(document.createTextNode(label));
			legend.appendChild(li);
			ix += 1;
		});
		captionEl.appendChild(legend);
	}

	function renderTable(series, yFrom, yTo) {
		while (tableHost.firstChild) tableHost.removeChild(tableHost.firstChild);
		if (series.size === 0) {
			var p = document.createElement('p');
			p.className = 'muted';
			p.textContent = '無資料。';
			tableHost.appendChild(p);
			return;
		}
		var table = document.createElement('table');
		var thead = document.createElement('thead');
		var trh = document.createElement('tr');
		trh.appendChild(th('系列'));
		for (var y = yFrom; y <= yTo; y += 1) trh.appendChild(th(String(y)));
		trh.appendChild(th('合計'));
		thead.appendChild(trh);
		table.appendChild(thead);
		var tbody = document.createElement('tbody');
		series.forEach(function (perYear, label) {
			var tr = document.createElement('tr');
			tr.appendChild(td(label));
			var total = 0;
			for (var y2 = yFrom; y2 <= yTo; y2 += 1) {
				var v = perYear.get(y2) || 0;
				tr.appendChild(td(v.toLocaleString('zh-TW')));
				total += v;
			}
			var totalTd = td(total.toLocaleString('zh-TW'));
			totalTd.className = 'total';
			tr.appendChild(totalTd);
			tbody.appendChild(tr);
		});
		table.appendChild(tbody);
		tableHost.appendChild(table);
	}

	function th(text) {
		var e = document.createElement('th');
		e.textContent = text;
		return e;
	}
	function td(text) {
		var e = document.createElement('td');
		e.textContent = text;
		return e;
	}

	function populateSelect(sel, items) {
		for (var i = 0; i < items.length; i += 1) {
			var opt = document.createElement('option');
			opt.value = items[i];
			opt.textContent = items[i];
			sel.appendChild(opt);
		}
	}
	function populateYearSelect(sel, years, defaultValue) {
		for (var i = 0; i < years.length; i += 1) {
			var opt = document.createElement('option');
			opt.value = String(years[i]);
			opt.textContent = String(years[i]);
			if (years[i] === defaultValue) opt.selected = true;
			sel.appendChild(opt);
		}
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

	function createSvg(tag, attrs) {
		var el = document.createElementNS('http://www.w3.org/2000/svg', tag);
		if (attrs) Object.keys(attrs).forEach(function (k) { el.setAttribute(k, attrs[k]); });
		return el;
	}
	function appendLine(parent, x1, y1, x2, y2, color) {
		parent.appendChild(createSvg('line', {
			x1: x1, y1: y1, x2: x2, y2: y2, stroke: color, 'stroke-width': '1'
		}));
	}
	function appendText(parent, x, y, text, color, anchor, size) {
		var t = createSvg('text', {
			x: x, y: y, fill: color, 'text-anchor': anchor || 'start',
			'font-size': String(size || 11),
			'font-family': 'ui-sans-serif, system-ui, sans-serif'
		});
		t.textContent = text;
		parent.appendChild(t);
	}
	function appendCircle(parent, cx, cy, r, fill) {
		parent.appendChild(createSvg('circle', { cx: cx, cy: cy, r: r, fill: fill }));
	}

	function niceMax(v) {
		if (v <= 0) return 1;
		var pow = Math.pow(10, Math.floor(Math.log10(v)));
		var n = v / pow;
		var nice = n <= 1 ? 1 : n <= 2 ? 2 : n <= 5 ? 5 : 10;
		return nice * pow;
	}
})();
