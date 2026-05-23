import { env } from '$env/dynamic/private';
import { error } from '@sveltejs/kit';
import { normaliseGatewayBase, withCookieHeader } from '$lib/account/gateway';
import { parseBookmarkArray } from '$lib/bookmarks/types';
import { datasetSlugToUuid } from '$lib/comments/slug-uuid.server';
import { findDatasetBySlug } from '$lib/datasets/load';
import { parseRatingView, type RatingView } from '$lib/ratings/types';
import type { PageServerLoad } from './$types';

/** Default view when the gateway probe fails — degrades to "no ratings yet". */
const EMPTY_RATING_VIEW: RatingView = {
	avg_score: null,
	count: 0,
	viewer_score: null
};

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
				// Run the response through the same runtime
				// narrower that the account page uses so a
				// shape drift here can't throw an unchecked
				// TypeError from `.some(...)` and bubble up
				// to the outer catch as a noisy 500 trace.
				// A `null` parse degrades to "not bookmarked"
				// — same outcome as a network failure, no
				// extra branch needed.
				const rows = parseBookmarkArray(await res.json().catch(() => null));
				if (rows !== null) {
					bookmarked = rows.some(
						(r) => r.target_kind === 'dataset' && r.target_id === commentTargetId
					);
				}
			}
		} catch (e) {
			console.error('[/datasets/:id] bookmark probe failed:', e);
		}
	}

	// Pre-paint the rating view (aggregate + viewer's own
	// score) so the stars render with the correct fill on
	// first byte. Anonymous traffic still sees the aggregate
	// — the gateway endpoint is anonymous-readable. A probe
	// failure degrades to "no ratings yet" rather than
	// 500-ing the page.
	let ratingView: RatingView = EMPTY_RATING_VIEW;
	try {
		const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
		const res = await fetch(`${base}/api/v1/ratings/dataset/${commentTargetId}`, {
			method: 'GET',
			headers: withCookieHeader(
				new Headers({ accept: 'application/json' }),
				request.headers.get('cookie')
			)
		});
		if (res.ok) {
			const parsed = parseRatingView(await res.json().catch(() => null));
			if (parsed !== null) {
				ratingView = parsed;
			}
		}
	} catch (e) {
		console.error('[/datasets/:id] rating probe failed:', e);
	}

	return {
		dataset,
		commentTargetId,
		currentUserId,
		// Mirror the layout's mode so community-facing
		// surfaces (comments thread + HeartButton + stars)
		// are SSR-skipped in personal-mode deploys — the
		// gateway doesn't mount their subrouters there, so
		// a probe would 404 and the components would render
		// as "Loading…" stubs that the client only hides at
		// hydration. One flag covers all because the auth
		// subrouter is the shared gate; if features ship
		// separately in the future, split the flag.
		communityEnabled: parentData.mode === 'multi-user',
		bookmarked,
		ratingView
	};
};
