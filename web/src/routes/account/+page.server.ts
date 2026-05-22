/**
 * Account page (#4.6). Lists the user's API keys + lets them
 * create, revoke, and rotate.
 *
 * The page IS the SSR view; the SvelteKit `<form>` element posts
 * to the actions below for every state-changing operation
 * (create / revoke / rotate). That way browsers without JS can
 * still manage their keys, and the JS-enhanced path uses
 * `enhance` to avoid the full-page reload.
 */

import { env } from '$env/dynamic/private';
import { error, fail, redirect } from '@sveltejs/kit';
import type { Actions, PageServerLoad } from './$types';
import type { ApiKeySummary, IssuedApiKey } from '$lib/account/types';
import { RATE_LIMIT_TIERS } from '$lib/account/types';
import {
	apiKeyByIdUrl,
	apiKeysUrl,
	classifyGatewayStatus,
	normaliseGatewayBase,
	parseApiKeySummaryArray,
	parseGatewayErrorBody,
	parseIssuedApiKey,
	rotateApiKeyUrl,
	withCookieHeader
} from '$lib/account/gateway';

/**
 * Single source of truth for the "gateway is down" message
 * shown to end users. Used by both the load function and every
 * action handler so a future copy edit hits one place. Low-level
 * fetch errors (DNS / TLS / hostnames) are logged to the server
 * console instead of being echoed into the UI.
 */
const GATEWAY_UNREACHABLE_MESSAGE =
	'Gateway temporarily unreachable. Please try again in a moment.';

type LoadOk = {
	state: 'ok';
	keys: ApiKeySummary[];
};

type LoadUnauthenticated = {
	state: 'unauthenticated';
};

type LoadDegraded = {
	state: 'unavailable' | 'unexpected';
	message: string;
};

/**
 * Server-side load — fetches the keys list via the gateway,
 * forwarding the request's `Cookie:` header so the user's
 * session cookie reaches the gateway even on a cross-origin
 * deployment.
 *
 * Returns a discriminated union so the page template can
 * branch cleanly on the three failure shapes ("please log in",
 * "gateway is down", "something is wrong") without losing the
 * keys-list happy path.
 */
export const load: PageServerLoad = async ({
	fetch,
	request
}): Promise<LoadOk | LoadUnauthenticated | LoadDegraded> => {
	const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	let response: Response;
	try {
		response = await fetch(apiKeysUrl(base), {
			method: 'GET',
			headers: withCookieHeader(
				new Headers({ accept: 'application/json' }),
				request.headers.get('cookie')
			)
		});
	} catch (e) {
		// Connection refused / DNS failure / TLS error — the
		// gateway is unreachable. Log the low-level detail
		// server-side (hostnames / ports / TLS chain text live
		// here, not in the UI) and return a generic message so
		// the page shell renders without echoing operational
		// noise to end users.
		console.error('api-keys load: gateway fetch failed', e);
		return {
			state: 'unavailable',
			message: GATEWAY_UNREACHABLE_MESSAGE
		};
	}

	if (!response.ok) {
		const kind = classifyGatewayStatus(response.status);
		if (kind === 'unauthenticated') {
			return { state: 'unauthenticated' };
		}
		const body = parseGatewayErrorBody(await safeJson(response));
		return {
			state: kind,
			message: body?.message ?? `gateway returned ${response.status}`
		};
	}

	const parsed = parseApiKeySummaryArray(await safeJson(response));
	if (parsed === null) {
		return {
			state: 'unexpected',
			message: 'gateway returned an unexpected response shape'
		};
	}
	return { state: 'ok', keys: parsed };
};

export const actions: Actions = {
	/**
	 * Create a new key. POSTs to `/v1/api-keys` and returns the
	 * one-time cleartext (server-side success response). The page
	 * component pops a modal that displays the cleartext exactly
	 * once and then drops it from state.
	 */
	create: async ({ fetch, request }) => {
		const form = await request.formData();
		const name = (form.get('name')?.toString() ?? '').trim();
		const tier = (form.get('rate_limit_tier')?.toString() ?? 'free').trim();
		if (name.length === 0) {
			return fail(400, { create: { error: 'name is required' } });
		}
		if (!(RATE_LIMIT_TIERS as readonly string[]).includes(tier)) {
			return fail(400, { create: { error: `tier "${tier}" is not allowed` } });
		}
		// The scopes field on the form is a comma-separated list;
		// trim + drop empties so a single trailing comma doesn't
		// land as a "" scope on the row.
		const scopes = (form.get('scopes')?.toString() ?? '')
			.split(',')
			.map((s) => s.trim())
			.filter((s) => s.length > 0);

		const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
		let response: Response;
		try {
			response = await fetch(apiKeysUrl(base), {
				method: 'POST',
				headers: withCookieHeader(
					new Headers({ accept: 'application/json', 'content-type': 'application/json' }),
					request.headers.get('cookie')
				),
				body: JSON.stringify({ name, rate_limit_tier: tier, scopes })
			});
		} catch (e) {
			// Network-layer failure (connection refused / DNS /
			// TLS). Log the low-level detail server-side and
			// surface a controlled `fail` so the page can render
			// a friendly message instead of a generic 500.
			console.error('api-keys create: gateway fetch failed', e);
			return fail(503, { create: { error: GATEWAY_UNREACHABLE_MESSAGE } });
		}

		if (response.status === 401) {
			throw redirect(303, '/account');
		}
		if (!response.ok) {
			const body = parseGatewayErrorBody(await safeJson(response));
			return fail(response.status, {
				create: { error: body?.message ?? `gateway returned ${response.status}` }
			});
		}
		const issued = parseIssuedApiKey(await safeJson(response));
		if (issued === null) {
			throw error(502, 'gateway returned an unexpected response shape on create');
		}
		// Pass the one-time cleartext back to the page via the
		// action return so the page template can render the
		// "copy me, you will not see it again" modal.
		return { created: issued satisfies IssuedApiKey };
	},

	/**
	 * Revoke a key. Returns nothing on success; the page reloads
	 * keys via SvelteKit's `invalidate` (built into `use:enhance`).
	 */
	revoke: async ({ fetch, request }) => {
		const form = await request.formData();
		const id = form.get('id')?.toString() ?? '';
		if (id.length === 0) {
			return fail(400, { revoke: { error: 'id is required' } });
		}
		const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
		let response: Response;
		try {
			response = await fetch(apiKeyByIdUrl(base, id), {
				method: 'DELETE',
				headers: withCookieHeader(
					new Headers({ accept: 'application/json' }),
					request.headers.get('cookie')
				)
			});
		} catch (e) {
			console.error('api-keys revoke: gateway fetch failed', e);
			return fail(503, { revoke: { id, error: GATEWAY_UNREACHABLE_MESSAGE } });
		}
		if (response.status === 401) {
			throw redirect(303, '/account');
		}
		if (response.status === 404) {
			return fail(404, { revoke: { id, error: 'key not found or already revoked' } });
		}
		if (!response.ok) {
			// Parse the gateway's structured `{error, message}`
			// body when present so a 400 from validation or a
			// 500 with a useful detail surfaces in the UI
			// instead of the opaque "gateway returned <status>"
			// fallback. Matches the `create` action's pattern.
			const body = parseGatewayErrorBody(await safeJson(response));
			return fail(response.status, {
				revoke: { id, error: body?.message ?? `gateway returned ${response.status}` }
			});
		}
		return { revoked: { id } };
	},

	/**
	 * Rotate a key: revoke the old + create a new one with the
	 * same name/scopes/tier. Returns the new key so the page can
	 * show the cleartext in the same modal as create.
	 */
	rotate: async ({ fetch, request }) => {
		const form = await request.formData();
		const id = form.get('id')?.toString() ?? '';
		if (id.length === 0) {
			return fail(400, { rotate: { error: 'id is required' } });
		}
		const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
		let response: Response;
		try {
			response = await fetch(rotateApiKeyUrl(base, id), {
				method: 'POST',
				headers: withCookieHeader(
					new Headers({ accept: 'application/json' }),
					request.headers.get('cookie')
				)
			});
		} catch (e) {
			console.error('api-keys rotate: gateway fetch failed', e);
			return fail(503, { rotate: { id, error: GATEWAY_UNREACHABLE_MESSAGE } });
		}
		if (response.status === 401) {
			throw redirect(303, '/account');
		}
		if (response.status === 404) {
			return fail(404, { rotate: { id, error: 'key not found or already revoked' } });
		}
		if (!response.ok) {
			// Surface the gateway's structured error body when
			// present, matching the `create` action's pattern —
			// otherwise a validation 400 collapses to the
			// opaque "gateway returned <status>" fallback that
			// hides the actual problem from the user.
			const body = parseGatewayErrorBody(await safeJson(response));
			return fail(response.status, {
				rotate: { id, error: body?.message ?? `gateway returned ${response.status}` }
			});
		}
		const issued = parseIssuedApiKey(await safeJson(response));
		if (issued === null) {
			throw error(502, 'gateway returned an unexpected response shape on rotate');
		}
		// Mark the surface area with an explicit `is_rotated` flag
		// so the page template can adjust copy (`rotated from
		// tdh_abcd…` vs `created`); the cleartext display is the
		// same as create otherwise.
		return { created: issued satisfies IssuedApiKey, was_rotation: true };
	}
};

/**
 * `response.json()` throws on a non-JSON body. We don't want
 * that to bubble up because the gateway might (briefly) return
 * HTML during a startup failure, and we'd rather show a clean
 * "unexpected response" than a stack trace.
 */
async function safeJson(response: Response): Promise<unknown> {
	try {
		return await response.json();
	} catch {
		return null;
	}
}
