import { env } from '$env/dynamic/private';
import { error } from '@sveltejs/kit';
import { normaliseGatewayBase, withCookieHeader } from '$lib/account/gateway';
import { datasetSlugToUuid } from '$lib/comments/slug-uuid.server';
import { findDatasetBySlug } from '$lib/datasets/load';
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
 *   * `currentUserId` — reused from the layout's `/api/v1/me`
 *     fetch (via `await parent()`) so the layout + this page
 *     share one round trip per render. `null` for anonymous
 *     traffic.
 *
 * The browser-side `CommentThread` component does its own
 * fetching against same-origin `/api/v1/comments…` paths —
 * the reverse proxy (Caddy in prod, vite proxy in dev) routes
 * those to the gateway. The page does NOT leak the internal
 * `GATEWAY_HTTP_URL` to the client.
 *
 * Cache-Control varies per session: when the layout populated
 * a `user`, the response is per-viewer and must NOT enter a
 * shared cache. Anonymous renders keep the wide 5-min public
 * cache; `Vary: Cookie` is set only in the per-user branch so
 * unrelated cookies don't shred CDN hit rates.
 */
export const load: PageServerLoad = async ({ fetch, params, parent, request, setHeaders }) => {
	const dataset = findDatasetBySlug(params.id);
	if (!dataset) {
		throw error(404, `Dataset "${params.id}" not found`);
	}
	const parentData = await parent();
	const currentUserId = parentData.user?.user_id ?? null;
	const commentTargetId = datasetSlugToUuid(dataset.slug);
	if (currentUserId !== null) {
		setHeaders({
			'cache-control': 'private, no-store',
			vary: 'Cookie'
		});
	} else {
		setHeaders({
			'cache-control': 'public, max-age=300, stale-while-revalidate=300'
		});
	}

	// Probe the bookmark state for the currently-signed-in
	// user so the heart renders pre-filled on first paint.
	// Anonymous traffic skips the round trip; a failing probe
	// degrades to "not bookmarked" without 500-ing the page.
	let bookmarked = false;
	if (currentUserId !== null) {
		try {
			const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
			// Filter to `kind=dataset` so the probe stays cheap
			// for users with many bookmarks across kinds.
			const res = await fetch(`${base}/api/v1/bookmarks?kind=dataset`, {
				method: 'GET',
				headers: withCookieHeader(
					new Headers({ accept: 'application/json' }),
					request.headers.get('cookie')
				)
			});
			if (res.ok) {
				const rows = (await res.json().catch(() => null)) as Array<{
					target_kind: string;
					target_id: string;
				}> | null;
				if (Array.isArray(rows)) {
					bookmarked = rows.some(
						(r) => r.target_kind === 'dataset' && r.target_id === commentTargetId
					);
				}
			}
		} catch (e) {
			console.error('[/datasets/:id] bookmark probe failed:', e);
		}
	}

	return {
		dataset,
		commentTargetId,
		currentUserId,
		// Mirror the layout's mode so community-facing
		// surfaces (comments thread + HeartButton) are
		// SSR-skipped in personal-mode deploys — the gateway
		// doesn't mount their subrouters there, so a probe
		// would 404 and the components would render as
		// "Loading…" stubs that the client only hides at
		// hydration. One flag covers both because the auth
		// subrouter is the shared gate; if either feature
		// ships separately in the future, split the flag.
		communityEnabled: parentData.mode === 'multi-user',
		bookmarked
	};
};
