/**
 * `/admin/reports` page server (#5a.6). Moderator-only
 * queue for open content reports. Server-side gating
 * happens at the gateway via `users.role`; this loader
 * just maps the gateway's status codes onto the
 * page's `LoadOk | LoadUnauthenticated | LoadForbidden |
 * LoadDegraded` discriminated union, mirroring the
 * existing `/admin/moderation` page.
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
	parseReportArray,
	REPORT_ACTIONS,
	type Report,
	type ReportAction
} from '$lib/reports/types';

const GATEWAY_UNREACHABLE_MESSAGE =
	'Gateway temporarily unreachable. Please try again in a moment.';

type LoadOk = {
	state: 'ok';
	reports: Report[];
};
type LoadUnauthenticated = { state: 'unauthenticated' };
type LoadForbidden = { state: 'forbidden' };
type LoadDegraded = { state: 'unavailable' | 'unexpected'; message: string };

function friendlyLoadErrorMessage(status: number): string {
	if (status === 404) {
		return 'Reports are not enabled on this deployment. Ask your operator to configure the gateway with DATABASE_URL and SESSION_HMAC_KEY.';
	}
	return GATEWAY_UNREACHABLE_MESSAGE;
}

export const load: PageServerLoad = async ({
	fetch,
	request
}): Promise<LoadOk | LoadUnauthenticated | LoadForbidden | LoadDegraded> => {
	const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	let response: Response;
	try {
		response = await fetch(`${base}/api/v1/admin/reports`, {
			method: 'GET',
			headers: withCookieHeader(
				new Headers({ accept: 'application/json' }),
				request.headers.get('cookie')
			)
		});
	} catch (e) {
		console.error('[/admin/reports] gateway unreachable:', e);
		return { state: 'unavailable', message: GATEWAY_UNREACHABLE_MESSAGE };
	}
	if (response.status === 401) return { state: 'unauthenticated' };
	if (response.status === 403) return { state: 'forbidden' };
	if (!response.ok) {
		const kind = classifyGatewayStatus(response.status);
		if (kind === 'unavailable') {
			return { state: 'unavailable', message: friendlyLoadErrorMessage(response.status) };
		}
		console.error('[/admin/reports] unexpected status:', response.status);
		return { state: 'unexpected', message: 'Unexpected response from the gateway.' };
	}
	const reports = parseReportArray(await response.json().catch(() => null));
	if (reports === null) {
		console.error('[/admin/reports] gateway response failed parse');
		return { state: 'unexpected', message: 'Unexpected response shape from the gateway.' };
	}
	return { state: 'ok', reports };
};

export const actions: Actions = {
	resolve: async ({ fetch, request }) => {
		const form = await request.formData();
		const id = (form.get('id') ?? '').toString();
		const actionRaw = (form.get('action') ?? '').toString();
		const note = (form.get('resolution_note') ?? '').toString().trim();
		if (id.length === 0) {
			return fail(400, { message: 'Missing report id.' });
		}
		if (!(REPORT_ACTIONS as readonly string[]).includes(actionRaw)) {
			return fail(400, { message: 'Invalid action.' });
		}
		const action = actionRaw as ReportAction;
		const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
		let response: Response;
		try {
			response = await fetch(`${base}/api/v1/admin/reports/${encodeURIComponent(id)}/resolve`, {
				method: 'POST',
				headers: withCookieHeader(
					new Headers({
						accept: 'application/json',
						'content-type': 'application/json'
					}),
					request.headers.get('cookie')
				),
				body: JSON.stringify({
					action,
					resolution_note: note.length === 0 ? null : note
				})
			});
		} catch (e) {
			console.error('[/admin/reports] gateway unreachable on resolve:', e);
			return fail(503, { message: GATEWAY_UNREACHABLE_MESSAGE });
		}
		if (response.status === 401) return fail(401, { message: 'Please sign in again.' });
		if (response.status === 403) return fail(403, { message: 'Moderator role required.' });
		if (response.status === 404)
			return fail(404, { message: 'Report not found or already resolved.' });
		if (!response.ok) {
			const errBody = parseGatewayErrorBody(await response.json().catch(() => null));
			return fail(response.status, {
				message: errBody?.message ?? 'Could not resolve the report.'
			});
		}
		return { resolved: { id, action } };
	}
};
