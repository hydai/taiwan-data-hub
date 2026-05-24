/* Procurement War Room playground.
 *
 * Three-panel dashboard over a flat contracts array:
 *   - top vendors by current-year total (horizontal bar list)
 *   - anomaly list (vendors with >5× YoY growth)
 *   - per-agency total (horizontal bar list)
 *
 * Filters: year selector + currency unit. All dynamic styles are
 * CSS classes — the framework CSP blocks `element.style.foo = ...`
 * mutations. Bar widths use percentage classes `.w-0` … `.w-100`
 * defined in style.css (rounded to nearest 5 %), applied via
 * `classList.add('w-' + bucket)`. Colour bins, grid placement,
 * etc. follow the same data-attr-or-class idiom established in
 * the earlier playgrounds.
 */

(async function () {
	'use strict';

	if (!window.tdh) {
		document.body.textContent = 'Playground framework shim missing.';
		return;
	}
	var data = window.__PROCUREMENT_DATA__;
	if (!data || !Array.isArray(data.contracts)) {
		document.body.textContent = 'sample-data.js failed to load.';
		return;
	}

	var YEARS = data._meta.years;
	var ANOMALY_THRESHOLD = data._meta.anomaly_threshold_x || 5;
	var contracts = data.contracts;
	var vendorById = mapBy(data.vendors, 'id');
	var agencyById = mapBy(data.agencies, 'id');

	var yearSel = document.getElementById('year');
	var unitRadios = document.querySelectorAll('input[name="unit"]');
	var topList = document.getElementById('top-list');
	var anomalyList = document.getElementById('anomaly-list');
	var agencyList = document.getElementById('agency-list');

	// Populate year select.
	for (var i = 0; i < YEARS.length; i += 1) {
		var opt = document.createElement('option');
		opt.value = String(YEARS[i]);
		opt.textContent = String(YEARS[i]);
		if (YEARS[i] === YEARS[YEARS.length - 1]) opt.selected = true;
		yearSel.appendChild(opt);
	}

	var validYears = new Set(YEARS);
	var validUnits = new Set(['m', 'thousand']);

	var initial = await window.tdh.getState();
	if (initial && typeof initial === 'object') {
		if (typeof initial.year === 'number' && validYears.has(initial.year)) {
			yearSel.value = String(initial.year);
		}
		if (typeof initial.unit === 'string' && validUnits.has(initial.unit)) {
			setRadio('unit', initial.unit);
		}
	}

	yearSel.addEventListener('change', function () {
		render();
		pushState();
	});
	unitRadios.forEach(function (r) {
		r.addEventListener('change', function () {
			render();
			pushState();
		});
	});

	render();

	function render() {
		var year = Number(yearSel.value);
		var unit = getRadio('unit') || 'm';
		var unitDivisor = unit === 'm' ? 1000000 : 1000;
		var unitSuffix = unit === 'm' ? 'M' : 'K';

		renderTopVendors(year, unitDivisor, unitSuffix);
		renderAnomalies(year, unitDivisor, unitSuffix);
		renderAgencies(year, unitDivisor, unitSuffix);
	}

	function renderTopVendors(year, divisor, suffix) {
		var totals = sumBy(
			contracts.filter(function (r) { return r.year === year; }),
			'vendor_id',
			'amount_ntd'
		);
		var sorted = sortDesc(totals).slice(0, 10);
		var max = sorted.length ? sorted[0].total : 1;
		topList.textContent = '';
		for (var i = 0; i < sorted.length; i += 1) {
			var s = sorted[i];
			var v = vendorById[s.id];
			topList.appendChild(barRow(i + 1, v ? v.name : s.id, s.total, max, divisor, suffix));
		}
	}

	function renderAnomalies(year, divisor, suffix) {
		anomalyList.textContent = '';
		var prevYear = year - 1;
		// If `prevYear` is before the dataset's earliest year there
		// is no YoY baseline at all — distinguish that from "we
		// checked and found nothing" so the user knows the panel
		// isn't silently failing.
		if (prevYear < YEARS[0]) {
			var firstYearLi = document.createElement('li');
			firstYearLi.className = 'muted';
			firstYearLi.textContent =
				year + ' 年為資料集最早年份,無前一年基準可比;請選擇 ' + YEARS[1] + ' 年(含)以後';
			anomalyList.appendChild(firstYearLi);
			return;
		}
		// Compute current vs previous vendor totals.
		var curr = sumBy(
			contracts.filter(function (r) { return r.year === year; }),
			'vendor_id',
			'amount_ntd'
		);
		var prev = sumBy(
			contracts.filter(function (r) { return r.year === prevYear; }),
			'vendor_id',
			'amount_ntd'
		);
		var prevByVendor = {};
		for (var i = 0; i < prev.length; i += 1) prevByVendor[prev[i].id] = prev[i].total;
		var anomalies = [];
		for (var j = 0; j < curr.length; j += 1) {
			var c = curr[j];
			var p = prevByVendor[c.id];
			if (typeof p !== 'number' || p <= 0) continue;
			var ratio = c.total / p;
			if (ratio >= ANOMALY_THRESHOLD) {
				anomalies.push({ id: c.id, ratio: ratio, current: c.total, previous: p });
			}
		}
		anomalies.sort(function (a, b) { return b.ratio - a.ratio; });
		if (anomalies.length === 0) {
			var emptyLi = document.createElement('li');
			emptyLi.className = 'muted';
			emptyLi.textContent = year + ' 年無 ≥ ' + ANOMALY_THRESHOLD + '× 異常 (相對前一年)';
			anomalyList.appendChild(emptyLi);
			return;
		}
		for (var k = 0; k < anomalies.length; k += 1) {
			var a = anomalies[k];
			var v = vendorById[a.id];
			var li = document.createElement('li');
			var head = document.createElement('header');
			var name = document.createElement('span');
			name.className = 'vendor';
			name.textContent = v ? v.name : a.id;
			head.appendChild(name);
			var pill = document.createElement('span');
			pill.className = 'pill';
			pill.textContent = a.ratio.toFixed(1) + '×';
			head.appendChild(pill);
			li.appendChild(head);
			var detail = document.createElement('p');
			detail.className = 'detail';
			detail.textContent =
				prevYear +
				': NT$ ' +
				formatUnit(a.previous, divisor) +
				suffix +
				' → ' +
				year +
				': NT$ ' +
				formatUnit(a.current, divisor) +
				suffix;
			li.appendChild(detail);
			anomalyList.appendChild(li);
		}
	}

	function renderAgencies(year, divisor, suffix) {
		var totals = sumBy(
			contracts.filter(function (r) { return r.year === year; }),
			'agency_id',
			'amount_ntd'
		);
		// Backfill agencies that have ZERO contracts for the
		// selected year so the panel always shows the full set of
		// 10 (the DoD asks for per-agency comparison; silently
		// omitting agencies with zero spend would hide the
		// "no awards this year" signal).
		var seen = new Set(
			totals.map(function (t) {
				return t.id;
			})
		);
		for (var ai = 0; ai < data.agencies.length; ai += 1) {
			var ag = data.agencies[ai];
			if (!seen.has(ag.id)) totals.push({ id: ag.id, total: 0 });
		}
		var sorted = sortDesc(totals);
		var max = sorted.length ? sorted[0].total : 1;
		agencyList.textContent = '';
		for (var i = 0; i < sorted.length; i += 1) {
			var s = sorted[i];
			var agency = agencyById[s.id];
			agencyList.appendChild(
				barRow(i + 1, agency ? agency.name : s.id, s.total, max, divisor, suffix)
			);
		}
	}

	function barRow(rank, name, total, max, divisor, suffix) {
		var li = document.createElement('li');
		var rankEl = document.createElement('span');
		rankEl.className = 'rank';
		rankEl.textContent = String(rank);
		li.appendChild(rankEl);
		var nameEl = document.createElement('span');
		nameEl.className = 'name';
		nameEl.textContent = name;
		li.appendChild(nameEl);
		var bar = document.createElement('span');
		// Width via `.w-N` class (multiples of 5 %); CSP rejects
		// `el.style.width = ...`.
		var pct = max > 0 ? Math.round((total / max) * 100) : 0;
		var bucket = Math.max(0, Math.min(100, Math.round(pct / 5) * 5));
		bar.className = 'bar w-' + bucket;
		li.appendChild(bar);
		var value = document.createElement('span');
		value.className = 'value';
		value.textContent = 'NT$ ' + formatUnit(total, divisor) + suffix;
		li.appendChild(value);
		return li;
	}

	function sumBy(rows, key, valueKey) {
		var bucket = {};
		for (var i = 0; i < rows.length; i += 1) {
			var r = rows[i];
			var k = r[key];
			bucket[k] = (bucket[k] || 0) + (r[valueKey] || 0);
		}
		var out = [];
		for (var k in bucket) {
			if (Object.prototype.hasOwnProperty.call(bucket, k)) {
				out.push({ id: k, total: bucket[k] });
			}
		}
		return out;
	}

	function sortDesc(rows) {
		var copy = rows.slice();
		copy.sort(function (a, b) { return b.total - a.total; });
		return copy;
	}

	function formatUnit(total, divisor) {
		return (total / divisor).toLocaleString('zh-TW', { maximumFractionDigits: 1 });
	}

	function mapBy(arr, key) {
		var out = {};
		for (var i = 0; i < arr.length; i += 1) out[arr[i][key]] = arr[i];
		return out;
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
	function pushState() {
		window.tdh.setState({
			year: Number(yearSel.value),
			unit: getRadio('unit') || 'm'
		});
	}
})();
