import { error } from '@sveltejs/kit';
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
export const load: PageServerLoad = async ({ params, parent, setHeaders }) => {
	const dataset = findDatasetBySlug(params.id);
	if (!dataset) {
		throw error(404, `Dataset "${params.id}" not found`);
	}
	const parentData = await parent();
	const currentUserId = parentData.user?.user_id ?? null;
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
	return {
		dataset,
		commentTargetId: datasetSlugToUuid(dataset.slug),
		currentUserId
	};
};
