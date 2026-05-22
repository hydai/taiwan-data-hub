/**
 * Layout-level server load (#4.8). Fetches the gateway's
 * `/api/v1/config` (always) and `/api/v1/me` (multi-user
 * mode only) so the layout shell can drive auth-conditional
 * rendering without reading env vars on the web side — the
 * single source of truth for the operating mode is the
 * gateway's `MODE` env var, exposed via `/api/v1/config`.
 *
 * SSR caching: SvelteKit memoises load function results for
 * the lifetime of one server render, so the layout's children
 * see the same `mode` / `user` snapshot the layout did
 * without re-fetching.
 *
 * Network-failure posture: if either fetch fails, the layout
 * falls back to `mode: 'personal'` + `user: null`. That's
 * the safe-by-default choice — in the worst case the auth UI
 * is briefly hidden (operators see a "personal mode" badge
 * even when MODE=multi-user) but no broken auth surface ever
 * renders. A `console.error` ships the underlying cause to
 * the server log so ops can diagnose.
 */

import { env } from '$env/dynamic/private';
import type { LayoutServerLoad } from './$types';
import { normaliseGatewayBase, withCookieHeader } from '$lib/account/gateway';
import { parseConfigResponse, parseMeResponse, type MeUser } from '$lib/gateway/config';
import type { GatewayMode } from '$lib/gateway/types';

type LayoutData = {
	mode: GatewayMode;
	user: MeUser | null;
};

export const load: LayoutServerLoad = async ({ fetch, request }): Promise<LayoutData> => {
	const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	const cookie = request.headers.get('cookie');

	const mode = await fetchMode(fetch, base);
	// /api/v1/me is only mounted by the gateway when auth is
	// wired (DATABASE_URL + SESSION_HMAC_KEY); in personal
	// mode we'd just get a 404 here. Skip the round trip
	// entirely instead of relying on the gateway's 404 to
	// fall through.
	const user = mode === 'multi-user' ? await fetchMe(fetch, base, cookie) : null;
	return { mode, user };
};

async function fetchMode(fetch: typeof globalThis.fetch, base: string): Promise<GatewayMode> {
	try {
		const response = await fetch(`${base}/api/v1/config`, {
			method: 'GET',
			headers: { accept: 'application/json' }
		});
		if (!response.ok) {
			console.error('layout config: gateway returned', response.status);
			return 'personal';
		}
		const parsed = parseConfigResponse(await response.json().catch(() => null));
		if (parsed === null) {
			console.error('layout config: gateway returned an unexpected shape');
			return 'personal';
		}
		return parsed.mode;
	} catch (e) {
		console.error('layout config: gateway fetch failed', e);
		return 'personal';
	}
}

async function fetchMe(
	fetch: typeof globalThis.fetch,
	base: string,
	cookie: string | null
): Promise<MeUser | null> {
	try {
		const response = await fetch(`${base}/api/v1/me`, {
			method: 'GET',
			headers: withCookieHeader(new Headers({ accept: 'application/json' }), cookie)
		});
		if (!response.ok) {
			// 404 is expected when the gateway has auth disabled
			// (no SESSION_HMAC_KEY); other statuses warrant a
			// server-side log so ops can see the cause.
			if (response.status !== 404) {
				console.error('layout me: gateway returned', response.status);
			}
			return null;
		}
		const parsed = parseMeResponse(await response.json().catch(() => null));
		if (parsed === null) {
			console.error('layout me: gateway returned an unexpected shape');
			return null;
		}
		return parsed.user;
	} catch (e) {
		console.error('layout me: gateway fetch failed', e);
		return null;
	}
}
