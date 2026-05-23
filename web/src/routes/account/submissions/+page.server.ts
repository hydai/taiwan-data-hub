/**
 * `/account/submissions` page server (#5a.1). Lists the
 * caller's submissions and supports a single author-side
 * action: `withdraw`, which flips a `pending` row to
 * `withdrawn` so the moderator never has to triage it.
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
import {
	parseSubmissionSummaryArray,
	submissionByIdUrl,
	submissionsUrl
} from '$lib/submissions/gateway';
import type { SubmissionSummary } from '$lib/submissions/types';

const GATEWAY_UNREACHABLE_MESSAGE =
	'Gateway temporarily unreachable. Please try again in a moment.';

/**
 * Map a load-time HTTP failure to user-facing copy. Mirrors
 * the helper in `/account/+page.server.ts`: 404 specifically
 * means the submissions subrouter is not mounted (gateway
 * missing `DATABASE_URL` or `SESSION_HMAC_KEY`), which is a
 * distinct operator-actionable state from "gateway down".
 */
function friendlyLoadErrorMessage(status: number): string {
	if (status === 404) {
		return 'Submissions are not enabled on this deployment. Ask your operator to configure the gateway with DATABASE_URL and SESSION_HMAC_KEY.';
	}
	return GATEWAY_UNREACHABLE_MESSAGE;
}

type LoadOk = {
	state: 'ok';
	submissions: SubmissionSummary[];
};

type LoadUnauthenticated = {
	state: 'unauthenticated';
};

type LoadDegraded = {
	state: 'unavailable' | 'unexpected';
	message: string;
};

export const load: PageServerLoad = async ({
	fetch,
	request
}): Promise<LoadOk | LoadUnauthenticated | LoadDegraded> => {
	const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	let response: Response;
	try {
		response = await fetch(submissionsUrl(base), {
			method: 'GET',
			headers: withCookieHeader(
				new Headers({ accept: 'application/json' }),
				request.headers.get('cookie')
			)
		});
	} catch (e) {
		console.error('[/account/submissions] gateway unreachable:', e);
		return { state: 'unavailable', message: GATEWAY_UNREACHABLE_MESSAGE };
	}
	if (!response.ok) {
		const kind = classifyGatewayStatus(response.status);
		if (kind === 'unauthenticated') return { state: 'unauthenticated' };
		if (kind === 'unavailable') {
			console.error('[/account/submissions] gateway returned status:', response.status);
			return {
				state: 'unavailable',
				message: friendlyLoadErrorMessage(response.status)
			};
		}
		console.error('[/account/submissions] unexpected status:', response.status);
		return { state: 'unexpected', message: 'Unexpected response from the gateway.' };
	}
	const parsed = parseSubmissionSummaryArray(await response.json().catch(() => null));
	if (parsed === null) {
		console.error('[/account/submissions] gateway response failed parse');
		return { state: 'unexpected', message: 'Unexpected response shape from the gateway.' };
	}
	return { state: 'ok', submissions: parsed };
};

export const actions: Actions = {
	withdraw: async ({ fetch, request }) => {
		const form = await request.formData();
		const id = (form.get('id') ?? '').toString();
		if (id.length === 0) {
			return fail(400, { message: 'Missing submission id.' });
		}
		const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
		let response: Response;
		try {
			response = await fetch(submissionByIdUrl(base, id), {
				method: 'DELETE',
				headers: withCookieHeader(
					new Headers({ accept: 'application/json' }),
					request.headers.get('cookie')
				)
			});
		} catch (e) {
			console.error('[/account/submissions] gateway unreachable on withdraw:', e);
			return fail(503, { message: GATEWAY_UNREACHABLE_MESSAGE });
		}
		if (response.status === 401) {
			return fail(401, { message: 'Your session has expired. Please sign in again.' });
		}
		if (response.status === 404) {
			return fail(404, {
				message: 'Submission not found, not yours, or already past the pending stage.'
			});
		}
		if (!response.ok) {
			const errBody = parseGatewayErrorBody(await response.json().catch(() => null));
			return fail(response.status, {
				message: errBody?.message ?? 'Could not withdraw the submission.'
			});
		}
		return { withdrew: id };
	}
};
