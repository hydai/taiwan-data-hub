import { env } from '$env/dynamic/private';
import { error } from '@sveltejs/kit';
import { datasetSlugToUuid } from '$lib/comments/slug-uuid';
import { findDatasetBySlug } from '$lib/datasets/load';
import { normaliseGatewayBase } from '$lib/account/gateway';
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
 *   * `gatewayBase` — same-origin (empty string) by default;
 *     overridable for cross-origin deploys via
 *     `GATEWAY_HTTP_URL`.
 */
export const load: PageServerLoad = async ({ fetch, params, request, setHeaders }) => {
	const dataset = findDatasetBySlug(params.id);
	if (!dataset) {
		throw error(404, `Dataset "${params.id}" not found`);
	}
	setHeaders({
		'cache-control': 'public, max-age=300, stale-while-revalidate=300'
	});

	const gatewayBase = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	const commentTargetId = datasetSlugToUuid(dataset.slug);

	// Soft-fail on the `/me` probe: a dropped gateway shouldn't
	// 500 the dataset page. Anonymous fallback hides the form.
	let currentUserId: string | null = null;
	try {
		const res = await fetch(`${gatewayBase}/api/v1/me`, {
			method: 'GET',
			headers: {
				accept: 'application/json',
				...(request.headers.get('cookie') ? { cookie: request.headers.get('cookie')! } : {})
			}
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

	return {
		dataset,
		commentTargetId,
		currentUserId,
		gatewayBase
	};
};
