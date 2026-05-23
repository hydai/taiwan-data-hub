import { env } from '$env/dynamic/private';
import { error } from '@sveltejs/kit';
import { datasetSlugToUuid } from '$lib/comments/slug-uuid.server';
import { findDatasetBySlug } from '$lib/datasets/load';
import { normaliseGatewayBase, withCookieHeader } from '$lib/account/gateway';
import { parseMeResponse } from '$lib/gateway/config';
import type { PageServerLoad } from './$types';

/**
 * Resolves the dataset record for /datasets/[id]. 404s cleanly if the
 * id is unknown so the SEO crawl doesn't index empty pages.
 *
 * 5-min stale-while-revalidate matches /domains and /collections so
 * the marketplace surfaces share a single cache rhythm.
 *
 * Also passes the data the comment thread (#5a.3) needs:
 *
 *   * `commentTargetId` — UUIDv5 derived from the slug so the
 *     comments table can key on a stable UUID until the
 *     dataset table itself becomes the gateway DB's source of
 *     truth (the helper drops out at that point).
 *   * `currentUserId` — read from `/api/v1/me`; `null` for
 *     anonymous traffic. The CommentThread component renders
 *     a "sign in to comment" prompt instead of the form.
 *
 * The browser-side `CommentThread` component does its own
 * fetching against same-origin `/api/v1/comments…` paths —
 * the reverse proxy (Caddy in prod, vite proxy in dev) routes
 * those to the gateway. The page does NOT leak the internal
 * `GATEWAY_HTTP_URL` to the client.
 */
/**
 * Strict cookie-presence check that survives values containing
 * the cookie's name as a substring (e.g.
 * `wat_tdh_session=hi`). Splits on `;`, trims each pair, and
 * matches the exact name before `=`. Mirrors the logic in
 * `$lib/account/gateway::extractCookie`.
 */
function cookieHeaderHas(cookieHeader: string | null, name: string): boolean {
	if (cookieHeader === null) return false;
	for (const pair of cookieHeader.split(';')) {
		const trimmed = pair.trim();
		const eq = trimmed.indexOf('=');
		if (eq <= 0) continue;
		if (trimmed.substring(0, eq) === name) return true;
	}
	return false;
}

export const load: PageServerLoad = async ({ fetch, params, request, setHeaders }) => {
	const dataset = findDatasetBySlug(params.id);
	if (!dataset) {
		throw error(404, `Dataset "${params.id}" not found`);
	}
	// Cache policy depends on whether the response carries a
	// per-user payload. A session cookie means the `/me` probe
	// below populates `currentUserId`, which a shared cache
	// MUST NOT serve to other users. Without a cookie, the
	// page is identical for every viewer and is safe to share.
	// `Vary: Cookie` only fires in the per-user branch so the
	// anonymous response keeps a wide hit rate (CDNs that key
	// on every `Vary` header don't shred on unrelated cookies).
	const hasSessionCookie = cookieHeaderHas(request.headers.get('cookie'), 'tdh_session');
	if (hasSessionCookie) {
		// Per-user response: must not be shared across viewers,
		// and the CDN must key on the cookie if any layer
		// ignores `private`.
		setHeaders({
			'cache-control': 'private, no-store',
			vary: 'Cookie'
		});
	} else {
		// Identical for every anonymous viewer — keep the wide
		// public cache and skip `Vary: Cookie` so unrelated
		// cookies (analytics / A/B) don't shred hit rates on
		// CDNs that key by every Vary header.
		setHeaders({
			'cache-control': 'public, max-age=300, stale-while-revalidate=300'
		});
	}

	const gatewayBase = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	const commentTargetId = datasetSlugToUuid(dataset.slug);

	// Skip the `/me` round trip entirely for anonymous traffic
	// — without `tdh_session`, the gateway is guaranteed to
	// answer `{ user: null }` and the response would be
	// thrown away.
	let currentUserId: string | null = null;
	if (hasSessionCookie) {
		try {
			const res = await fetch(`${gatewayBase}/api/v1/me`, {
				method: 'GET',
				headers: withCookieHeader(
					new Headers({ accept: 'application/json' }),
					request.headers.get('cookie')
				)
			});
			if (res.ok) {
				const parsed = parseMeResponse(await res.json().catch(() => null));
				if (parsed !== null && parsed.user !== null) {
					currentUserId = parsed.user.user_id;
				}
			}
		} catch (e) {
			console.error('[/datasets/:id] /me probe failed:', e);
		}
	}

	return {
		dataset,
		commentTargetId,
		currentUserId
	};
};
