/* Reference playground app.
 *
 * Exercises every framework API exposed by `window.tdh`:
 *   - `getState`  — read the initial state from the share-link URL
 *   - `setState`  — update the share-link URL on every counter change
 *   - `fetch`     — proxy a GET /api/v1/healthz call through the parent
 *
 * Authored as plain ES2018 — no bundler runs on this file, the
 * browser executes it verbatim under the iframe sandbox.
 */

(async function () {
	'use strict';

	if (!window.tdh) {
		document.body.textContent = 'Playground framework shim missing.';
		return;
	}

	var valueEl = document.getElementById('value');
	var apiOut = document.getElementById('api-out');

	var state = await window.tdh.getState();
	var count = state && typeof state.count === 'number' ? state.count : 0;
	render();

	document.getElementById('inc').addEventListener('click', function () {
		count += 1;
		render();
		window.tdh.setState({ count: count });
	});
	document.getElementById('dec').addEventListener('click', function () {
		count -= 1;
		render();
		window.tdh.setState({ count: count });
	});
	document.getElementById('ping').addEventListener('click', async function () {
		apiOut.textContent = 'Calling…';
		try {
			var res = await window.tdh.fetch('/api/v1/healthz');
			var body = await res.text();
			apiOut.textContent = 'HTTP ' + res.status + '\n' + body;
		} catch (e) {
			apiOut.textContent = 'Error: ' + (e && e.message ? e.message : String(e));
		}
	});

	function render() {
		valueEl.textContent = String(count);
	}
})();
