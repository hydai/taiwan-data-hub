/* Company 360 playground.
 *
 * Loads `sample-data.js` (shipped alongside this script under the
 * playground's served prefix and pulled in via `<script src>` from
 * index.html — see the CSP note below for why JS-tag-loaded data
 * is the right path here), reads the initial 統編 from the
 * framework's share-link state, and renders three panels:
 * business registry, judicial cases, procurement awards. Pushes
 * every search back into the share-link state so a URL pasted
 * into chat reproduces the exact view.
 *
 * The framework's `tdh.fetch` could in principle hit
 * `/api/v1/...` endpoints, but this playground keeps every data
 * dependency local: no network calls, deterministic output, works
 * offline. Real live-data playgrounds will arrive once the
 * gateway exposes dataset query endpoints (M7 territory).
 */

(async function () {
	'use strict';

	if (!window.tdh) {
		document.body.textContent = 'Playground framework shim missing.';
		return;
	}

	var form = document.getElementById('search-form');
	var input = document.getElementById('tax-id');
	var sampleList = document.getElementById('sample-list');
	var result = document.getElementById('result');
	var empty = document.getElementById('empty');
	var emptyTaxId = document.getElementById('empty-tax-id');

	// Sample data is loaded via `<script src="./sample-data.js">` in
	// index.html — it assigns to `window.__COMPANY_360_DATA__`. The
	// script-tag path is the only escape hatch under the framework
	// CSP (`script-src 'self'` allows same-origin script loads;
	// `connect-src 'none'` blocks fetch / XHR / etc.).
	var data = window.__COMPANY_360_DATA__;
	if (!data) {
		result.hidden = false;
		result.textContent = 'sample-data 未載入。請確認 sample-data.js 已包含於 index.html。';
		return;
	}

	var companies = data.companies || {};
	var disclaimerZh = (data._meta && data._meta['real_data_disclaimer_zh-TW']) || '';

	renderSampleList();
	wireForm();

	var initialState = await window.tdh.getState();
	if (initialState && typeof initialState.taxId === 'string') {
		input.value = initialState.taxId;
		runQuery(initialState.taxId, false);
	}

	function wireForm() {
		form.addEventListener('submit', function (event) {
			event.preventDefault();
			var value = (input.value || '').trim();
			if (!/^\d{8}$/.test(value)) {
				input.setCustomValidity('統一編號需為 8 碼數字');
				input.reportValidity();
				return;
			}
			input.setCustomValidity('');
			runQuery(value, true);
		});
		input.addEventListener('input', function () {
			input.setCustomValidity('');
		});
	}

	function renderSampleList() {
		var keys = Object.keys(companies).sort();
		sampleList.textContent = '';
		for (var i = 0; i < keys.length; i += 1) {
			var key = keys[i];
			var company = companies[key];
			var li = document.createElement('li');
			var btn = document.createElement('button');
			btn.type = 'button';
			btn.className = 'sample';
			// Build the label as an explicit conditional. The earlier
			// inline form `key + ' — ' + (registry && name) || key`
			// concatenates before applying `||`, so the LHS is always
			// truthy and the `|| key` fallback never fired — when a
			// registry was missing a name, the button rendered as
			// `<key> — undefined`.
			var name = company.registry && company.registry.name;
			btn.textContent = name ? key + ' — ' + name : key;
			btn.dataset.taxId = key;
			btn.addEventListener('click', function (ev) {
				var taxId = ev.currentTarget.dataset.taxId;
				input.value = taxId;
				runQuery(taxId, true);
			});
			li.appendChild(btn);
			sampleList.appendChild(li);
		}
	}

	function runQuery(taxId, push) {
		result.hidden = true;
		result.textContent = '';
		empty.hidden = true;
		if (push) {
			window.tdh.setState({ taxId: taxId });
		}
		var record = companies[taxId];
		if (!record) {
			emptyTaxId.textContent = taxId;
			empty.hidden = false;
			return;
		}
		result.hidden = false;
		result.appendChild(renderRegistryPanel(record));
		result.appendChild(renderJudicialPanel(record));
		result.appendChild(renderProcurementPanel(record));
		var note = document.createElement('p');
		note.className = 'muted disclaimer';
		note.textContent = disclaimerZh;
		result.appendChild(note);
	}

	function renderRegistryPanel(record) {
		var r = record.registry || {};
		var section = document.createElement('section');
		section.className = 'panel';
		section.appendChild(panelHeader('商業登記', '經濟部商業司 (示範值為公開資料)'));
		var dl = document.createElement('dl');
		dl.appendChild(dlRow('公司名稱', r.name || ''));
		if (r.name_en) dl.appendChild(dlRow('英文名稱', r.name_en));
		dl.appendChild(dlRow('地址', r.registered_address || ''));
		dl.appendChild(
			dlRow('資本額 (TWD)', r.capital_twd ? r.capital_twd.toLocaleString('zh-TW') : '')
		);
		dl.appendChild(dlRow('設立日期', r.established_date || ''));
		dl.appendChild(dlRow('代表人', r.representative || ''));
		dl.appendChild(dlRow('登記狀態', r.business_status || ''));
		section.appendChild(dl);
		return section;
	}

	function renderJudicialPanel(record) {
		var cases = record.judicial_cases || [];
		var section = document.createElement('section');
		section.className = 'panel';
		section.appendChild(panelHeader('司法案件', '示範樣本 — 非真實案件'));
		if (cases.length === 0) {
			var empty = document.createElement('p');
			empty.className = 'muted';
			empty.textContent = '此公司目前無已收錄案件 (示範資料範圍內)。';
			section.appendChild(empty);
			return section;
		}
		var ul = document.createElement('ul');
		ul.className = 'records';
		for (var i = 0; i < cases.length; i += 1) {
			var c = cases[i];
			var li = document.createElement('li');
			var head = document.createElement('header');
			var caseNo = document.createElement('span');
			caseNo.className = 'mono';
			caseNo.textContent = c.case_no;
			head.appendChild(caseNo);
			var status = document.createElement('span');
			status.className = 'status status-' + (c.status === '結案' ? 'closed' : 'open');
			status.textContent = c.status;
			head.appendChild(status);
			li.appendChild(head);
			var meta = document.createElement('p');
			meta.className = 'meta';
			meta.textContent = [c.court, c.case_type, c.filed_date].filter(Boolean).join(' · ');
			li.appendChild(meta);
			var summary = document.createElement('p');
			summary.className = 'summary';
			summary.textContent = c.summary;
			li.appendChild(summary);
			ul.appendChild(li);
		}
		section.appendChild(ul);
		return section;
	}

	function renderProcurementPanel(record) {
		var awards = record.procurement_awards || [];
		var section = document.createElement('section');
		section.className = 'panel';
		section.appendChild(panelHeader('政府採購得標', '示範樣本 — 非真實得標'));
		if (awards.length === 0) {
			var empty = document.createElement('p');
			empty.className = 'muted';
			empty.textContent = '此公司目前無得標紀錄 (示範資料範圍內)。';
			section.appendChild(empty);
			return section;
		}
		var ul = document.createElement('ul');
		ul.className = 'records';
		for (var i = 0; i < awards.length; i += 1) {
			var a = awards[i];
			var li = document.createElement('li');
			var head = document.createElement('header');
			var tid = document.createElement('span');
			tid.className = 'mono';
			tid.textContent = a.tender_id;
			head.appendChild(tid);
			var amt = document.createElement('span');
			amt.className = 'amount';
			amt.textContent = 'NT$ ' + (a.amount_twd || 0).toLocaleString('zh-TW');
			head.appendChild(amt);
			li.appendChild(head);
			var meta = document.createElement('p');
			meta.className = 'meta';
			meta.textContent = [a.agency, a.award_date].filter(Boolean).join(' · ');
			li.appendChild(meta);
			var subject = document.createElement('p');
			subject.className = 'summary';
			subject.textContent = a.subject;
			li.appendChild(subject);
			ul.appendChild(li);
		}
		section.appendChild(ul);
		return section;
	}

	function panelHeader(title, sub) {
		var h = document.createElement('header');
		var h2 = document.createElement('h2');
		h2.textContent = title;
		h.appendChild(h2);
		if (sub) {
			var p = document.createElement('p');
			p.className = 'muted';
			p.textContent = sub;
			h.appendChild(p);
		}
		return h;
	}

	function dlRow(label, value) {
		var dt = document.createElement('dt');
		dt.textContent = label;
		var dd = document.createElement('dd');
		dd.textContent = value === null || value === undefined ? '' : String(value);
		var wrap = document.createDocumentFragment();
		wrap.appendChild(dt);
		wrap.appendChild(dd);
		return wrap;
	}
})();
