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

	/**
	 * Recovery for the "shim already posted READY before this host
	 * was attached" race (cached iframe loads). The previous attempt
	 * here gated on `iframe.contentDocument.readyState`, but a
	 * sandboxed iframe has an opaque cross-origin document so
	 * `contentDocument` is always `null` — the check could never
	 * recover the case it was meant to fix.
	 *
	 * Simpler: fire INIT unconditionally at attach time, and also on
	 * the next `load` event. The shim resolves its init promise
	 * exactly once, so any of these firing is harmless; what we MUST
	 * avoid is zero INITs reaching a loaded shim.
	 *
	 *   - INIT at attach: handles the cached-load race. If the iframe
	 *     hasn't loaded yet, the postMessage hits the about:blank
	 *     contentWindow and is silently discarded — no error, no
	 *     side effect, and the `load` path below picks it back up.
	 *   - INIT on `load`: handles the fresh-load case.
	 *   - INIT on READY (in `handleMessage`): handles the steady-
	 *     state contract.
	 */
	function handleIframeLoad(): void {
		sendInit();
	}

	function postApiError(id: string, error: string): void {
		iframe.contentWindow?.postMessage(
			{
				type: PLAYGROUND_MSG_TYPES.API_RESPONSE,
				id,
				ok: false,
				status: 0,
				contentType: null,
				body: '',
				error
			},
			'*'
		);
	}

	/**
	 * Normalise the iframe-supplied `init` into a typed record after
	 * checking every field — `event.data` originates from author-
	 * controlled code, so we can't trust the shape declared in
	 * `protocol.ts`. Returns `null` to signal the parent should
	 * reject the call without proxying.
	 */
	function coerceInit(
		raw: unknown
	): { method?: string; headers?: Record<string, string>; body?: string } | null {
		if (raw === undefined) return {};
		if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) return null;
		const src = raw as Record<string, unknown>;
		const out: { method?: string; headers?: Record<string, string>; body?: string } = {};
		if (src.method !== undefined) {
			if (typeof src.method !== 'string') return null;
			out.method = src.method;
		}
		if (src.headers !== undefined) {
			if (src.headers === null || typeof src.headers !== 'object' || Array.isArray(src.headers)) {
				return null;
			}
			const headersOut: Record<string, string> = {};
			for (const [k, v] of Object.entries(src.headers as Record<string, unknown>)) {
				if (typeof k !== 'string' || typeof v !== 'string') return null;
				headersOut[k] = v;
			}
			out.headers = headersOut;
		}
		if (src.body !== undefined) {
			if (typeof src.body !== 'string') return null;
			out.body = src.body;
		}
		return out;
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
			postApiError(
				id,
				`Path "${path}" rejected: must resolve under ${PLAYGROUND_GATEWAY_PREFIX} (same-origin, no dot-segment escape)`
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
			postApiError(
				id,
				`Method "${requestedMethod}" rejected: playground proxy only forwards ${[
					...PLAYGROUND_ALLOWED_METHODS
				].join(' / ')}`
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
			postApiError(id, e instanceof Error ? e.message : 'fetch failed');
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
					id?: unknown;
					path?: unknown;
					init?: unknown;
				};
				if (typeof m.id !== 'string' || typeof m.path !== 'string') return;
				// Validate every field of `init` before it reaches
				// `toUpperCase()` etc. — a hostile playground that
				// bypasses the shim could otherwise post
				// `{method: 42}` and crash this handler with an
				// unhandled `TypeError`, leaving the call's promise
				// unresolved.
				const coerced = coerceInit(m.init);
				if (coerced === null) {
					postApiError(m.id, 'Malformed api-call init: every field must be string-typed.');
					return;
				}
				void proxyApiCall(m.id, m.path, coerced);
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
	iframe.addEventListener('load', handleIframeLoad);
	sendInit();

	return function detach(): void {
		window.removeEventListener('message', handleMessage);
		iframe.removeEventListener('load', handleIframeLoad);
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
