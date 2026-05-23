/**
 * `/admin/moderation` page server (#5a.2). Moderator queue
 * for pending submissions. Server-side authorization happens
 * at the gateway via `users.role`; this loader only renders
 * a 403 surface when the gateway returns one. Personal-mode
 * and anonymous traffic hit the same paths the /submit
 * loader handles.
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
	moderationDecisionUrl,
	moderationListUrl,
	parseDecisionResponse,
	parseModerationSubmissionArray
} from '$lib/moderation/gateway';
import type { ModerationSubmission } from '$lib/moderation/types';
import { SUBMISSION_KINDS, type SubmissionKind } from '$lib/submissions/types';

const GATEWAY_UNREACHABLE_MESSAGE =
	'Gateway temporarily unreachable. Please try again in a moment.';

type LoadOk = {
	state: 'ok';
	submissions: ModerationSubmission[];
	kindFilter: SubmissionKind | null;
};

type LoadUnauthenticated = { state: 'unauthenticated' };
type LoadForbidden = { state: 'forbidden' };
type LoadDegraded = { state: 'unavailable' | 'unexpected'; message: string };

function friendlyLoadErrorMessage(status: number): string {
	if (status === 404) {
		return 'Moderation is not enabled on this deployment. Ask your operator to configure the gateway with DATABASE_URL and SESSION_HMAC_KEY.';
	}
	return GATEWAY_UNREACHABLE_MESSAGE;
}

function parseKindParam(raw: string | null): SubmissionKind | null {
	if (raw === null) return null;
	return (SUBMISSION_KINDS as readonly string[]).includes(raw) ? (raw as SubmissionKind) : null;
}

export const load: PageServerLoad = async ({
	fetch,
	request,
	url
}): Promise<LoadOk | LoadUnauthenticated | LoadForbidden | LoadDegraded> => {
	const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	const kindFilter = parseKindParam(url.searchParams.get('kind'));
	let response: Response;
	try {
		response = await fetch(moderationListUrl(base, kindFilter ?? undefined), {
			method: 'GET',
			headers: withCookieHeader(
				new Headers({ accept: 'application/json' }),
				request.headers.get('cookie')
			)
		});
	} catch (e) {
		console.error('[/admin/moderation] gateway unreachable:', e);
		return { state: 'unavailable', message: GATEWAY_UNREACHABLE_MESSAGE };
	}
	if (response.status === 401) return { state: 'unauthenticated' };
	if (response.status === 403) return { state: 'forbidden' };
	if (!response.ok) {
		const kind = classifyGatewayStatus(response.status);
		if (kind === 'unavailable') {
			console.error('[/admin/moderation] gateway returned status:', response.status);
			return { state: 'unavailable', message: friendlyLoadErrorMessage(response.status) };
		}
		console.error('[/admin/moderation] unexpected status:', response.status);
		return { state: 'unexpected', message: 'Unexpected response from the gateway.' };
	}
	const submissions = parseModerationSubmissionArray(await response.json().catch(() => null));
	if (submissions === null) {
		console.error('[/admin/moderation] gateway response failed parse');
		return {
			state: 'unexpected',
			message: 'Unexpected response shape from the gateway.'
		};
	}
	return { state: 'ok', submissions, kindFilter };
};

async function decide(
	event: Parameters<NonNullable<Actions['approve']>>[0],
	action: 'approve' | 'reject'
) {
	const { fetch, request } = event;
	const form = await request.formData();
	const id = (form.get('id') ?? '').toString();
	const reason = (form.get('reason') ?? '').toString();
	if (id.length === 0) {
		return fail(400, { message: 'Missing submission id.' });
	}
	if (action === 'reject' && reason.trim().length === 0) {
		return fail(400, { message: 'A reason is required when rejecting a submission.' });
	}
	const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	let response: Response;
	try {
		response = await fetch(moderationDecisionUrl(base, id, action), {
			method: 'POST',
			headers: withCookieHeader(
				new Headers({
					accept: 'application/json',
					'content-type': 'application/json'
				}),
				request.headers.get('cookie')
			),
			body: JSON.stringify({ reason: reason.length === 0 ? null : reason })
		});
	} catch (e) {
		console.error('[/admin/moderation] gateway unreachable on decide:', e);
		return fail(503, { message: GATEWAY_UNREACHABLE_MESSAGE });
	}
	if (response.status === 401) return fail(401, { message: 'Please sign in again.' });
	if (response.status === 403) return fail(403, { message: 'Moderator role required.' });
	if (response.status === 409)
		return fail(409, {
			message: 'This submission was already decided by another moderator. Refresh the page.'
		});
	if (!response.ok) {
		const errBody = parseGatewayErrorBody(await response.json().catch(() => null));
		return fail(response.status, {
			message: errBody?.message ?? 'Could not record the decision.'
		});
	}
	const decision = parseDecisionResponse(await response.json().catch(() => null));
	if (decision === null) {
		return fail(502, { message: 'The gateway returned an unexpected response shape.' });
	}
	return { decided: { id: decision.submission.id, status: decision.submission.status } };
}

export const actions: Actions = {
	approve: async (event) => decide(event, 'approve'),
	reject: async (event) => decide(event, 'reject')
};
