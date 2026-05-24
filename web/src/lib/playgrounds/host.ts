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

/**
 * HTTP methods the playground proxy will forward. Read-only by
 * design — see the comment in `proxyApiCall` for the threat model.
 */
const PLAYGROUND_ALLOWED_METHODS: ReadonlySet<string> = new Set(['GET', 'HEAD']);

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
		// Allowlist by NORMALISED same-origin pathname, not raw
		// `startsWith`. A naive prefix check is bypassable with dot-
		// segments like `/api/v1/../../admin` — the literal string
		// matches `/api/v1/` but `fetch()` then resolves the URL
		// against the page origin and normalises it to `/admin`.
		// `URL`'s constructor does that normalisation for us; we
		// reject anything that doesn't land back inside the allowed
		// prefix.
		const normalised = safeGatewayPath(path);
		if (normalised === null) {
			iframe.contentWindow?.postMessage(
				{
					type: PLAYGROUND_MSG_TYPES.API_RESPONSE,
					id,
					ok: false,
					status: 0,
					contentType: null,
					body: '',
					error: `Path "${path}" rejected: must resolve under ${PLAYGROUND_GATEWAY_PREFIX} (same-origin, no dot-segment escape)`
				},
				'*'
			);
			return;
		}
		// Method-allowlist defence. Playgrounds are user-untrusted
		// code: even though the iframe is sandboxed, this parent-
		// side proxy runs in the user's session. If we forwarded
		// arbitrary methods + body the iframe could POST as the
		// logged-in user — spamming ratings/comments/submissions,
		// or hitting admin endpoints if the user is privileged.
		// Pin to read-only methods so the gateway can only ever be
		// queried, never mutated, via the playground proxy.
		const requestedMethod = (init?.method ?? 'GET').toUpperCase();
		if (!PLAYGROUND_ALLOWED_METHODS.has(requestedMethod)) {
			iframe.contentWindow?.postMessage(
				{
					type: PLAYGROUND_MSG_TYPES.API_RESPONSE,
					id,
					ok: false,
					status: 0,
					contentType: null,
					body: '',
					error: `Method "${requestedMethod}" rejected: playground proxy only forwards ${[
						...PLAYGROUND_ALLOWED_METHODS
					].join(' / ')}`
				},
				'*'
			);
			return;
		}
		try {
			// `credentials: 'omit'` strips the user's session cookies
			// from the proxied fetch. Combined with the GET-only
			// method allowlist this means: the playground sees the
			// gateway as an anonymous, read-only client. Defence
			// against a hostile playground using the proxy to act as
			// the logged-in user.
			const res = await fetch(normalised, {
				method: requestedMethod,
				headers: init?.headers,
				credentials: 'omit'
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

/**
 * Normalise an iframe-supplied `path` against the page origin and
 * return the same-origin pathname iff it sits under
 * `PLAYGROUND_GATEWAY_PREFIX`. Returns `null` on any escape (cross
 * -origin, dot-segments that leave the prefix, malformed URL).
 *
 * Exported for tests; not part of the runtime API surface.
 */
export function safeGatewayPath(rawPath: string): string | null {
	let parsed: URL;
	try {
		parsed = new URL(rawPath, window.location.origin);
	} catch {
		return null;
	}
	if (parsed.origin !== window.location.origin) return null;
	if (!parsed.pathname.startsWith(PLAYGROUND_GATEWAY_PREFIX)) return null;
	// Return the normalised pathname (+ search + hash) so the proxied
	// fetch hits the canonical URL the allowlist actually approved.
	return parsed.pathname + parsed.search + parsed.hash;
}
