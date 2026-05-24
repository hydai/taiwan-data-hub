/* eslint-disable @typescript-eslint/ban-ts-comment */
// @ts-nocheck
/* Framework shim served at /playgrounds/<slug>/app/__framework.js.
 *
 * Loaded as a regular <script> in the playground's index.html
 * BEFORE the playground's own app.js. Sets up `window.tdh` and
 * performs the postMessage handshake with the parent frame.
 *
 * Why plain JS instead of a TypeScript file? This file ships
 * verbatim to the iframe — no bundling, no transpile. Keeping it
 * untouched JS means what we read here matches what the browser
 * runs, including the comment-level documentation. svelte-check
 * picks up the .js by default for the rest of the codebase, but
 * this file's surface is the in-iframe global `window.tdh` which
 * TypeScript can't know about; @ts-nocheck silences the false
 * positives without disabling checks repo-wide, and the
 * eslint-disable above lets us keep ban-ts-comment everywhere
 * else.
 */

(function () {
	'use strict';

	if (window.tdh) return; // idempotent — double-load is harmless

	var TYPES = {
		INIT: 'tdh:init',
		STATE_CHANGED: 'tdh:state-changed',
		API_CALL: 'tdh:api-call',
		API_RESPONSE: 'tdh:api-response'
	};

	var pendingInit = null;
	var initResolve = null;
	var initialState = null;
	var initialised = false;

	var pendingCalls = new Map();
	var callIdCounter = 0;

	pendingInit = new Promise(function (resolve) {
		initResolve = resolve;
	});

	function handleMessage(event) {
		// Sandboxed iframes have a unique opaque origin and receive
		// "null" as `event.origin` for parent messages. The reliable
		// identity check is on `event.source` (must be the parent
		// window). Reject anything else so a deep-frame attacker
		// can't inject API responses.
		if (event.source !== window.parent) return;
		var msg = event.data;
		if (!msg || typeof msg !== 'object') return;
		if (msg.type === TYPES.INIT) {
			initialState = msg.state == null ? null : msg.state;
			if (!initialised && initResolve) {
				initialised = true;
				initResolve(initialState);
			}
		} else if (msg.type === TYPES.API_RESPONSE) {
			var cb = pendingCalls.get(msg.id);
			if (!cb) return;
			pendingCalls.delete(msg.id);
			cb(msg);
		}
	}

	window.addEventListener('message', handleMessage);

	function getState() {
		return pendingInit;
	}

	function setState(value) {
		window.parent.postMessage({ type: TYPES.STATE_CHANGED, state: value }, '*');
	}

	function fetchProxy(path, init) {
		if (typeof path !== 'string' || path.indexOf('/api/v1/') !== 0) {
			return Promise.reject(
				new Error('tdh.fetch: path must start with /api/v1/ (parent allowlist enforced)')
			);
		}
		callIdCounter += 1;
		var id = 'p' + Date.now() + '-' + callIdCounter;
		var safeInit = null;
		if (init) {
			safeInit = {};
			if (typeof init.method === 'string') safeInit.method = init.method;
			if (init.headers && typeof init.headers === 'object') {
				// Filter headers to a flat string map — Headers /
				// arrays don't structured-clone cleanly across all
				// browsers.
				var flatHeaders = {};
				if (init.headers instanceof Headers) {
					init.headers.forEach(function (v, k) {
						flatHeaders[k] = v;
					});
				} else {
					var keys = Object.keys(init.headers);
					for (var i = 0; i < keys.length; i += 1) {
						flatHeaders[keys[i]] = String(init.headers[keys[i]]);
					}
				}
				safeInit.headers = flatHeaders;
			}
			if (typeof init.body === 'string') safeInit.body = init.body;
		}
		return new Promise(function (resolve, reject) {
			pendingCalls.set(id, function (msg) {
				if (msg.error) {
					reject(new Error(msg.error));
					return;
				}
				var headers = msg.contentType ? { 'content-type': msg.contentType } : {};
				resolve(
					new Response(msg.body, {
						status: msg.status,
						headers: headers
					})
				);
			});
			window.parent.postMessage(
				{ type: TYPES.API_CALL, id: id, path: path, init: safeInit || undefined },
				'*'
			);
		});
	}

	window.tdh = {
		getState: getState,
		setState: setState,
		fetch: fetchProxy
	};

	// Tell the parent the iframe is ready to receive init. The parent
	// can't reliably listen for `load` (it fires before our scripts
	// run in some browsers), so the child kicks the handshake.
	window.parent.postMessage({ type: 'tdh:ready' }, '*');
})();
