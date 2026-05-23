/**
 * `/account/bookmarks` page server (#5a.4). Lists the
 * caller's bookmarks (optionally filtered by kind) and their
 * private collections.
 */

import { env } from '$env/dynamic/private';
import { fail } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import {
	classifyGatewayStatus,
	normaliseGatewayBase,
	parseGatewayErrorBody,
	withCookieHeader
} from '$lib/account/gateway';
import { bookmarksUrl, collectionByIdUrl, collectionsUrl } from '$lib/bookmarks/gateway';
import type { Bookmark, BookmarkTargetKind, Collection } from '$lib/bookmarks/types';
import {
	BOOKMARK_TARGET_KINDS,
	parseBookmarkArray,
	parseCollectionArray
} from '$lib/bookmarks/types';

const GATEWAY_UNREACHABLE_MESSAGE =
	'Gateway temporarily unreachable. Please try again in a moment.';

type LoadOk = {
	state: 'ok';
	bookmarks: Bookmark[];
	collections: Collection[];
	kindFilter: BookmarkTargetKind | null;
};
type LoadDegraded =
	| { state: 'unauthenticated' }
	| { state: 'unavailable' | 'unexpected'; message: string };

function friendlyLoadErrorMessage(status: number): string {
	if (status === 404) {
		return 'Bookmarks are not enabled on this deployment. Ask your operator to configure the gateway with DATABASE_URL and SESSION_HMAC_KEY.';
	}
	return GATEWAY_UNREACHABLE_MESSAGE;
}

function parseKindParam(raw: string | null): BookmarkTargetKind | null {
	if (raw === null) return null;
	return (BOOKMARK_TARGET_KINDS as readonly string[]).includes(raw)
		? (raw as BookmarkTargetKind)
		: null;
}

export const load: PageServerLoad = async ({
	fetch,
	request,
	url
}): Promise<LoadOk | LoadDegraded> => {
	const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	const kindFilter = parseKindParam(url.searchParams.get('kind'));

	const cookieHeader = request.headers.get('cookie');
	const headers = () => withCookieHeader(new Headers({ accept: 'application/json' }), cookieHeader);

	let bookmarksRes: Response;
	let collectionsRes: Response;
	try {
		[bookmarksRes, collectionsRes] = await Promise.all([
			fetch(`${base}${bookmarksUrl(kindFilter ?? undefined)}`, {
				method: 'GET',
				headers: headers()
			}),
			fetch(`${base}${collectionsUrl()}`, {
				method: 'GET',
				headers: headers()
			})
		]);
	} catch (e) {
		console.error('[/account/bookmarks] gateway unreachable:', e);
		return { state: 'unavailable', message: GATEWAY_UNREACHABLE_MESSAGE };
	}

	if (bookmarksRes.status === 401 || collectionsRes.status === 401) {
		return { state: 'unauthenticated' };
	}
	if (!bookmarksRes.ok || !collectionsRes.ok) {
		// Pick the worse classification across both responses
		// so a (400 on one, 500 on the other) pair surfaces as
		// "unavailable" — the more actionable signal for the
		// operator. Status pairs that are both `unexpected`
		// fall through to `unexpected`. Both statuses are
		// logged so dashboard noise correlates back to the
		// actual responses.
		const bookmarksKind = bookmarksRes.ok ? null : classifyGatewayStatus(bookmarksRes.status);
		const collectionsKind = collectionsRes.ok ? null : classifyGatewayStatus(collectionsRes.status);
		console.error(
			`[/account/bookmarks] non-ok responses: bookmarks=${bookmarksRes.status} (${bookmarksKind ?? 'ok'}) collections=${collectionsRes.status} (${collectionsKind ?? 'ok'})`
		);
		if (bookmarksKind === 'unavailable' || collectionsKind === 'unavailable') {
			// Pick the status from whichever response was
			// `unavailable`; if both are, prefer bookmarks (so
			// the 404-means-feature-disabled message can land
			// when the bookmarks subrouter is missing).
			const status = bookmarksKind === 'unavailable' ? bookmarksRes.status : collectionsRes.status;
			return { state: 'unavailable', message: friendlyLoadErrorMessage(status) };
		}
		return { state: 'unexpected', message: 'Unexpected response from the gateway.' };
	}

	const bookmarks = parseBookmarkArray(await bookmarksRes.json().catch(() => null));
	const collections = parseCollectionArray(await collectionsRes.json().catch(() => null));
	if (bookmarks === null || collections === null) {
		console.error('[/account/bookmarks] gateway response failed parse');
		return { state: 'unexpected', message: 'Unexpected response shape from the gateway.' };
	}
	return { state: 'ok', bookmarks, collections, kindFilter };
};

export const actions: Actions = {
	create_collection: async ({ fetch, request }) => {
		const form = await request.formData();
		const name = (form.get('name') ?? '').toString().trim();
		const description = (form.get('description') ?? '').toString().trim();
		if (name.length === 0) {
			return fail(400, { message: 'Collection name is required.' });
		}
		const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
		let response: Response;
		try {
			response = await fetch(`${base}${collectionsUrl()}`, {
				method: 'POST',
				headers: withCookieHeader(
					new Headers({
						accept: 'application/json',
						'content-type': 'application/json'
					}),
					request.headers.get('cookie')
				),
				body: JSON.stringify({
					name,
					description: description.length === 0 ? null : description
				})
			});
		} catch (e) {
			console.error('[/account/bookmarks] create gateway unreachable:', e);
			return fail(503, { message: GATEWAY_UNREACHABLE_MESSAGE });
		}
		if (response.status === 401) return fail(401, { message: 'Please sign in again.' });
		if (response.status === 409) {
			return fail(409, { message: 'You already have a collection with that name.' });
		}
		if (!response.ok) {
			const body = parseGatewayErrorBody(await response.json().catch(() => null));
			return fail(response.status, {
				message: body?.message ?? 'Could not create collection.'
			});
		}
		return { created: true };
	},

	delete_collection: async ({ fetch, request }) => {
		const form = await request.formData();
		const id = (form.get('id') ?? '').toString();
		if (id.length === 0) return fail(400, { message: 'Missing collection id.' });
		const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
		let response: Response;
		try {
			response = await fetch(`${base}${collectionByIdUrl(id)}`, {
				method: 'DELETE',
				headers: withCookieHeader(
					new Headers({ accept: 'application/json' }),
					request.headers.get('cookie')
				)
			});
		} catch (e) {
			console.error('[/account/bookmarks] delete gateway unreachable:', e);
			return fail(503, { message: GATEWAY_UNREACHABLE_MESSAGE });
		}
		if (response.status === 401) return fail(401, { message: 'Please sign in again.' });
		if (response.status === 404)
			return fail(404, { message: 'Collection not found or already deleted.' });
		if (!response.ok) {
			// Surface the gateway's structured `{error,
			// message}` body when present so actionable
			// failures (e.g. invalid UUID → 400 with a
			// useful hint) reach the user. Matches what
			// `create_collection` above already does.
			const body = parseGatewayErrorBody(await response.json().catch(() => null));
			return fail(response.status, {
				message: body?.message ?? 'Could not delete collection.'
			});
		}
		return { deleted: id };
	}
};
