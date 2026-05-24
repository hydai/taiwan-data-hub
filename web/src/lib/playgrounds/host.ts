/**
 * Parent-side glue for the playground framing page.
 *
 * Exposes `attachPlaygroundHost({ iframe })` which wires:
 *
 *   1. A message listener that accepts ONLY messages whose `source`
 *      is the supplied iframe's `contentWindow`. (`event.origin` is
 *      "null" for sandboxed iframes — relying on it would mean
 *      "trust everyone".)
 *   2. The READY → INIT handshake (parent reads the URL's `?state=`,
 *      decodes it, posts INIT once the child says it's ready).
 *   3. STATE_CHANGED → re-encode + replace the URL's `?state=`.
 *   4. API_CALL → allowlist the path → fetch from the same-origin
 *      gateway → post API_RESPONSE with the result.
 *
 * Returns a `detach()` you can call from Svelte's `onDestroy` so the
 * listener doesn't leak across navigations.
 */

import { encodeState, decodeState, StateTooLargeError } from './state';
import {
	PLAYGROUND_GATEWAY_PREFIX,
	PLAYGROUND_MSG_TYPES,
	type PlaygroundChildToParent
} from './protocol';

export interface PlaygroundHostOptions {
	iframe: HTMLIFrameElement;
	/** Optional state-update debounce (ms). Default 100. */
	urlUpdateDebounceMs?: number;
}

export function attachPlaygroundHost(opts: PlaygroundHostOptions): () => void {
	const { iframe } = opts;
	const debounceMs = opts.urlUpdateDebounceMs ?? 100;

	let pendingUrlUpdate: ReturnType<typeof setTimeout> | null = null;
	let latestState: unknown = null;

	function postUrlSoon(state: unknown): void {
		latestState = state;
		if (pendingUrlUpdate !== null) return;
		pendingUrlUpdate = setTimeout(() => {
			pendingUrlUpdate = null;
			try {
				const encoded = encodeState(latestState);
				const url = new URL(window.location.href);
				url.searchParams.set('state', encoded);
				window.history.replaceState(window.history.state, '', url);
			} catch (e) {
				if (e instanceof StateTooLargeError) {
					console.warn(e.message);
					return;
				}
				throw e;
			}
		}, debounceMs);
	}

	function sendInit(): void {
		const state = decodeState(new URL(window.location.href).searchParams.get('state'));
		iframe.contentWindow?.postMessage(
			{ type: PLAYGROUND_MSG_TYPES.INIT, state },
			'*' // sandboxed iframe → opaque origin → '*' is the only correct target
		);
	}

	async function proxyApiCall(
		id: string,
		path: string,
		init: { method?: string; headers?: Record<string, string>; body?: string } | undefined
	): Promise<void> {
		if (!path.startsWith(PLAYGROUND_GATEWAY_PREFIX)) {
			iframe.contentWindow?.postMessage(
				{
					type: PLAYGROUND_MSG_TYPES.API_RESPONSE,
					id,
					ok: false,
					status: 0,
					contentType: null,
					body: '',
					error: `Path "${path}" rejected: must start with ${PLAYGROUND_GATEWAY_PREFIX}`
				},
				'*'
			);
			return;
		}
		try {
			const res = await fetch(path, {
				method: init?.method ?? 'GET',
				headers: init?.headers,
				body: init?.body
			});
			const body = await res.text();
			iframe.contentWindow?.postMessage(
				{
					type: PLAYGROUND_MSG_TYPES.API_RESPONSE,
					id,
					ok: res.ok,
					status: res.status,
					contentType: res.headers.get('content-type'),
					body
				},
				'*'
			);
		} catch (e) {
			iframe.contentWindow?.postMessage(
				{
					type: PLAYGROUND_MSG_TYPES.API_RESPONSE,
					id,
					ok: false,
					status: 0,
					contentType: null,
					body: '',
					error: e instanceof Error ? e.message : 'fetch failed'
				},
				'*'
			);
		}
	}

	function handleMessage(event: MessageEvent): void {
		if (event.source !== iframe.contentWindow) return;
		const data = event.data as PlaygroundChildToParent | { type: string } | null;
		if (!data || typeof data !== 'object' || typeof data.type !== 'string') return;
		switch (data.type) {
			case PLAYGROUND_MSG_TYPES.READY:
				sendInit();
				return;
			case PLAYGROUND_MSG_TYPES.STATE_CHANGED:
				postUrlSoon((data as { state: unknown }).state);
				return;
			case PLAYGROUND_MSG_TYPES.API_CALL: {
				const m = data as {
					id: string;
					path: string;
					init?: { method?: string; headers?: Record<string, string>; body?: string };
				};
				if (typeof m.id !== 'string' || typeof m.path !== 'string') return;
				void proxyApiCall(m.id, m.path, m.init);
				return;
			}
			default:
				// Unknown messages are silently ignored — the iframe is
				// the only sender we trust, but a future framework
				// version might add message types this build doesn't
				// know yet, and we don't want to break forward-compat.
				return;
		}
	}

	window.addEventListener('message', handleMessage);

	return function detach(): void {
		window.removeEventListener('message', handleMessage);
		if (pendingUrlUpdate !== null) {
			clearTimeout(pendingUrlUpdate);
			pendingUrlUpdate = null;
		}
	};
}
