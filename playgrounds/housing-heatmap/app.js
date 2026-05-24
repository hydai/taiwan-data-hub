/* Housing Heatmap playground.
 *
 * Year-by-year choropleth of median housing price (NT$/ping)
 * across Taiwan's 22 counties, with click-to-drill sample
 * transactions. All styles applied via CSS classes — the
 * framework CSP forbids `element.style.foo = ...` mutations (see
 * the Taiwan Map playground's app.js for the same constraint).
 *
 * Year + selected county encoded in framework share-link state
 * so the URL fully reproduces a view. Time-slider value is
 * validated against the known YEARS list before being applied.
 */

(async function () {
	'use strict';

	if (!window.tdh) {
		document.body.textContent = 'Playground framework shim missing.';
		return;
	}
	var data = window.__HOUSING_HEATMAP_DATA__;
	if (!data || !Array.isArray(data.counties)) {
		document.body.textContent = 'sample-data.js failed to load.';
		return;
	}

	var YEARS = data._meta.years;
	var counties = data.counties;
	var byCode = data.by_code;
	var PALETTE_LENGTH = 7;

	var slider = document.getElementById('year-slider');
	var yearDisplay = document.getElementById('year-display');
	var mapEl = document.getElementById('map');
	var detailEl = document.getElementById('detail');
	var legendEl = document.getElementById('legend');
	var summaryEl = document.getElementById('summary');

	// Year slider goes by index into YEARS so a malformed share-
	// link value can't escape the valid range (max attribute clamps
	// it). Display the actual year value separately.
	slider.min = '0';
	slider.max = String(YEARS.length - 1);
	slider.value = String(YEARS.length - 1); // default to most-recent

	var selectedCounty = null;
	var validYears = new Set(YEARS);

	var initial = await window.tdh.getState();
	if (initial && typeof initial === 'object') {
		if (typeof initial.year === 'number' && validYears.has(initial.year)) {
			slider.value = String(YEARS.indexOf(initial.year));
		}
		if (typeof initial.county === 'string' && byCode[initial.county]) {
			selectedCounty = initial.county;
		}
	}

	buildGrid();
	slider.addEventListener('input', function () {
		render();
		pushState();
	});

	render();

	function currentYear() {
		var idx = Number(slider.value);
		if (!Number.isFinite(idx) || idx < 0 || idx >= YEARS.length) idx = YEARS.length - 1;
		return YEARS[idx];
	}

	function buildGrid() {
		mapEl.textContent = '';
		for (var i = 0; i < counties.length; i += 1) {
			var c = counties[i];
			var btn = document.createElement('button');
			btn.type = 'button';
			btn.className = 'tile';
			btn.dataset.code = c.code;
			btn.dataset.col = String(c.col);
			btn.dataset.row = String(c.row);
			btn.setAttribute('aria-pressed', 'false');
			var label = document.createElement('span');
			label.className = 'tile-label';
			label.textContent = c.name;
			btn.appendChild(label);
			var value = document.createElement('span');
			value.className = 'tile-value';
			btn.appendChild(value);
			btn.addEventListener('click', function (ev) {
				selectedCounty = ev.currentTarget.dataset.code;
				renderDetail();
				updatePressed();
				pushState();
			});
			mapEl.appendChild(btn);
		}
	}

	function render() {
		var year = currentYear();
		yearDisplay.textContent = String(year);

		// Collect this year's values across counties to size the
		// palette equal-width bins.
		var values = [];
		for (var i = 0; i < counties.length; i += 1) {
			var c = counties[i];
			var v = (byCode[c.code].price_per_ping || {})[year];
			if (typeof v === 'number' && Number.isFinite(v)) values.push(v);
		}
		var min = values.length ? Math.min.apply(null, values) : 0;
		var max = values.length ? Math.max.apply(null, values) : 1;
		var ticks = computeTicks(min, max, PALETTE_LENGTH);

		var tiles = mapEl.querySelectorAll('.tile');
		for (var t = 0; t < tiles.length; t += 1) {
			var tile = tiles[t];
			var c2 = byCode[tile.dataset.code];
			if (!c2) continue;
			var v2 = (c2.price_per_ping || {})[year];
			var idx = bucketIndex(typeof v2 === 'number' ? v2 : 0, ticks);
			for (var b = 0; b < PALETTE_LENGTH; b += 1) tile.classList.remove('bin-' + b);
			tile.classList.add('bin-' + idx);
			var valEl = tile.querySelector('.tile-value');
			valEl.textContent = formatPrice(v2);
		}

		updatePressed();
		renderLegend(ticks);
		summaryEl.textContent =
			'年份 ' +
			year +
			';縣市中位數房價(每坪)區間 NT$' +
			formatPrice(min) +
			' – NT$' +
			formatPrice(max) +
			'。';
		renderDetail();
	}

	function updatePressed() {
		var tiles = mapEl.querySelectorAll('.tile');
		for (var i = 0; i < tiles.length; i += 1) {
			var isSel = tiles[i].dataset.code === selectedCounty;
			tiles[i].classList.toggle('selected', isSel);
			tiles[i].setAttribute('aria-pressed', isSel ? 'true' : 'false');
		}
	}

	function renderDetail() {
		detailEl.textContent = '';
		if (!selectedCounty) {
			var p = document.createElement('p');
			p.className = 'muted';
			p.textContent = '點擊任一縣市以檢視該年度樣本交易紀錄。';
			detailEl.appendChild(p);
			return;
		}
		var c = byCode[selectedCounty];
		if (!c) return;
		var year = currentYear();
		var h = document.createElement('h2');
		h.textContent = c.name + ' · ' + year;
		detailEl.appendChild(h);
		var price = (c.price_per_ping || {})[year];
		var sub = document.createElement('p');
		sub.className = 'muted sub';
		sub.textContent = '中位數房價:NT$ ' + formatPrice(price) + ' / 坪';
		detailEl.appendChild(sub);

		var h3 = document.createElement('h3');
		h3.textContent = '樣本交易紀錄';
		detailEl.appendChild(h3);

		var txns = c.txns || [];
		if (txns.length === 0) {
			var none = document.createElement('p');
			none.className = 'muted';
			none.textContent = '無樣本交易紀錄。';
			detailEl.appendChild(none);
			return;
		}
		var ul = document.createElement('ul');
		ul.className = 'txns';
		for (var i = 0; i < txns.length; i += 1) {
			var t = txns[i];
			var li = document.createElement('li');
			var meta = document.createElement('span');
			meta.className = 'meta';
			meta.textContent = t.date + ' · ' + t.district;
			li.appendChild(meta);
			var size = document.createElement('span');
			size.className = 'size';
			size.textContent = t.ping + ' 坪';
			li.appendChild(size);
			var total = document.createElement('span');
			total.className = 'total';
			total.textContent = 'NT$ ' + t.total_ntd_wan.toLocaleString('zh-TW') + ' 萬';
			li.appendChild(total);
			ul.appendChild(li);
		}
		detailEl.appendChild(ul);
		var dis = document.createElement('p');
		dis.className = 'disclaimer';
		dis.textContent = '樣本交易為依公開均值生成的示範資料。';
		detailEl.appendChild(dis);
	}

	function renderLegend(ticks) {
		legendEl.textContent = '';
		var label = document.createElement('span');
		label.className = 'legend-label';
		label.textContent = '每坪價格 (NT$)';
		legendEl.appendChild(label);
		for (var i = 0; i < PALETTE_LENGTH; i += 1) {
			var bin = document.createElement('span');
			bin.className = 'legend-bin';
			var swatch = document.createElement('span');
			swatch.className = 'legend-swatch bin-' + i;
			bin.appendChild(swatch);
			var t = document.createElement('span');
			t.className = 'legend-tick';
			t.textContent =
				formatPrice(ticks[i]) +
				(i < PALETTE_LENGTH - 1 ? '–' + formatPrice(ticks[i + 1]) : '+');
			bin.appendChild(t);
			legendEl.appendChild(bin);
		}
	}

	function formatPrice(v) {
		if (typeof v !== 'number' || !Number.isFinite(v)) return '—';
		if (v >= 1000000) return (v / 10000).toLocaleString('zh-TW', { maximumFractionDigits: 0 }) + '萬';
		if (v >= 10000) return (v / 10000).toFixed(1) + '萬';
		return String(Math.round(v));
	}

	function computeTicks(min, max, n) {
		var ticks = new Array(n + 1);
		var span = max - min;
		if (span <= 0) span = 1;
		for (var i = 0; i <= n; i += 1) ticks[i] = min + (span * i) / n;
		return ticks;
	}
	function bucketIndex(v, ticks) {
		for (var i = 0; i < ticks.length - 1; i += 1) {
			if (v < ticks[i + 1]) return i;
		}
		return ticks.length - 2;
	}

	function pushState() {
		window.tdh.setState({
			year: currentYear(),
			county: selectedCounty
		});
	}
})();
