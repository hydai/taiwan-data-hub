/**
 * `/submit` page server (#5a.1). Renders the multi-step form
 * for authenticated users in multi-user mode; the SvelteKit
 * `create` action POSTs the validated payload to the gateway.
 *
 * The load function maps the gateway state to one of three
 * branches so the form is never rendered in a context it
 * can't submit from:
 *
 *   * **personal mode** → `state: 'unavailable'` with the
 *     "Submissions are disabled on this deployment" copy.
 *     Detected via a 404 on `/api/v1/me` because that route
 *     is only mounted when DATABASE_URL + SESSION_HMAC_KEY
 *     are configured.
 *   * **anonymous multi-user** → `state: 'unauthenticated'`
 *     with a "please sign in" prompt. Detected via a 401 or
 *     a `{ user: null }` body.
 *   * **authenticated multi-user** → `state: 'ok'` and the
 *     form renders.
 *
 * The layout already drives auth-aware navigation; this
 * server just hardens the access gate at the route level so
 * a stale layout cache cannot let an anonymous request reach
 * the form.
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
import { parseMeResponse } from '$lib/gateway/config';
import { parseCreateSubmissionResponse, submissionsUrl } from '$lib/submissions/gateway';
import {
	SUBMISSION_FIELD_LIMITS,
	SUBMISSION_KINDS,
	type SubmissionKind,
	type SubmissionPayload
} from '$lib/submissions/types';

type LoadOk = {
	state: 'ok';
};

type LoadUnauthenticated = {
	state: 'unauthenticated';
};

type LoadDegraded = {
	state: 'unavailable' | 'unexpected';
	message: string;
};

const GATEWAY_UNREACHABLE_MESSAGE =
	'Gateway temporarily unreachable. Please try again in a moment.';

/**
 * Page load — verifies the caller has a live session by
 * hitting `/api/v1/me`. The dedicated check (vs. relying on
 * the global layout) keeps the page server-side-authoritative:
 * a stale layout cache cannot let an anonymous request render
 * the form.
 */
export const load: PageServerLoad = async ({
	fetch,
	request
}): Promise<LoadOk | LoadUnauthenticated | LoadDegraded> => {
	const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
	let response: Response;
	try {
		response = await fetch(`${base}/api/v1/me`, {
			method: 'GET',
			headers: withCookieHeader(
				new Headers({ accept: 'application/json' }),
				request.headers.get('cookie')
			)
		});
	} catch (e) {
		console.error('[/submit] gateway unreachable on /me:', e);
		return { state: 'unavailable', message: GATEWAY_UNREACHABLE_MESSAGE };
	}
	if (response.status === 404) {
		// `/api/v1/me` is only mounted when DATABASE_URL +
		// SESSION_HMAC_KEY are configured. A 404 here means the
		// gateway is in personal mode (or misconfigured); the
		// form has no use either way.
		return { state: 'unavailable', message: 'Submissions are disabled on this deployment.' };
	}
	if (!response.ok) {
		const kind = classifyGatewayStatus(response.status);
		if (kind === 'unauthenticated') {
			return { state: 'unauthenticated' };
		}
		if (kind === 'unavailable') {
			console.error('[/submit] gateway returned status:', response.status);
			return { state: 'unavailable', message: GATEWAY_UNREACHABLE_MESSAGE };
		}
		console.error('[/submit] unexpected /me status:', response.status);
		return { state: 'unexpected', message: 'Unexpected response from the gateway.' };
	}
	// Reuse `$lib/gateway/config::parseMeResponse` so the loader
	// shares its shape contract with the layout's loader. The
	// helper returns `null` on shape drift (logged + mapped to
	// `unexpected`), `{ user: null }` for anonymous traffic, or
	// `{ user: <object> }` for an authenticated session.
	const me = parseMeResponse(await response.json().catch(() => null));
	if (me === null) {
		console.error('[/submit] /me returned unexpected shape');
		return { state: 'unexpected', message: 'Unexpected response from the gateway.' };
	}
	if (me.user === null) {
		return { state: 'unauthenticated' };
	}
	return { state: 'ok' };
};

/**
 * Build the typed payload from the form values, applying the
 * same trims + URL-scheme checks the Rust validator runs. The
 * server-side validation in the gateway is authoritative; this
 * is a cheap pre-flight to short-circuit the network call when
 * the data is obviously wrong.
 */
function buildPayload(kind: SubmissionKind, form: FormData): SubmissionPayload | { error: string } {
	const get = (name: string): string => (form.get(name) ?? '').toString().trim();
	// Count Unicode scalar values, matching Rust's
	// `.chars().count()` on the gateway side. JS `string.length`
	// counts UTF-16 code units, so non-BMP characters (emoji,
	// older CJK plane B/C, etc.) would otherwise count double on
	// the client and cause the preflight to reject inputs the
	// server would accept (and vice versa).
	const scalarLength = (s: string): number => [...s].length;
	const required = (field: string, value: string, limit: number): string | { error: string } => {
		if (value.length === 0) return { error: `${field} is required` };
		if (scalarLength(value) > limit)
			return { error: `${field} must be ${limit} characters or fewer` };
		return value;
	};
	const url = (field: string, value: string): string | { error: string } => {
		const trimmed = required(field, value, SUBMISSION_FIELD_LIMITS.url);
		if (typeof trimmed !== 'string') return trimmed;
		if (!(trimmed.startsWith('http://') || trimmed.startsWith('https://'))) {
			return { error: `${field} must start with http:// or https://` };
		}
		return trimmed;
	};

	switch (kind) {
		case 'dataset': {
			const title = required('Title', get('title'), SUBMISSION_FIELD_LIMITS.name);
			if (typeof title !== 'string') return title;
			const description = required(
				'Description',
				get('description'),
				SUBMISSION_FIELD_LIMITS.description
			);
			if (typeof description !== 'string') return description;
			const source_url = url('Source URL', get('source_url'));
			if (typeof source_url !== 'string') return source_url;
			const license = required('License', get('license'), SUBMISSION_FIELD_LIMITS.name);
			if (typeof license !== 'string') return license;
			const domain_slug = required('Domain slug', get('domain_slug'), SUBMISSION_FIELD_LIMITS.name);
			if (typeof domain_slug !== 'string') return domain_slug;
			if (!/^[A-Za-z0-9_-]+$/.test(domain_slug)) {
				return { error: 'Domain slug must contain only letters, digits, - and _' };
			}
			return { kind, title, description, source_url, license, domain_slug };
		}
		case 'tool': {
			const name = required('Name', get('name'), SUBMISSION_FIELD_LIMITS.name);
			if (typeof name !== 'string') return name;
			const description = required(
				'Description',
				get('description'),
				SUBMISSION_FIELD_LIMITS.description
			);
			if (typeof description !== 'string') return description;
			const repo_url = url('Repository URL', get('repo_url'));
			if (typeof repo_url !== 'string') return repo_url;
			const language = required('Language', get('language'), SUBMISSION_FIELD_LIMITS.name);
			if (typeof language !== 'string') return language;
			return { kind, name, description, repo_url, language };
		}
		case 'connector': {
			const name = required('Name', get('name'), SUBMISSION_FIELD_LIMITS.name);
			if (typeof name !== 'string') return name;
			const description = required(
				'Description',
				get('description'),
				SUBMISSION_FIELD_LIMITS.description
			);
			if (typeof description !== 'string') return description;
			const repo_url = url('Repository URL', get('repo_url'));
			if (typeof repo_url !== 'string') return repo_url;
			const license = required('License', get('license'), SUBMISSION_FIELD_LIMITS.name);
			if (typeof license !== 'string') return license;
			return { kind, name, description, repo_url, license };
		}
		case 'playground': {
			const name = required('Name', get('name'), SUBMISSION_FIELD_LIMITS.name);
			if (typeof name !== 'string') return name;
			const description = required(
				'Description',
				get('description'),
				SUBMISSION_FIELD_LIMITS.description
			);
			if (typeof description !== 'string') return description;
			const demo_url = url('Demo URL', get('demo_url'));
			if (typeof demo_url !== 'string') return demo_url;
			const repoRaw = get('repo_url');
			let repo_url: string | null = null;
			if (repoRaw.length > 0) {
				const checked = url('Repository URL', repoRaw);
				if (typeof checked !== 'string') return checked;
				repo_url = checked;
			}
			return { kind, name, description, demo_url, repo_url };
		}
	}
}

export const actions: Actions = {
	create: async ({ fetch, request }) => {
		const form = await request.formData();
		const rawKind = (form.get('kind') ?? '').toString();
		if (!(SUBMISSION_KINDS as readonly string[]).includes(rawKind)) {
			return fail(400, { message: 'Please pick a submission type.' });
		}
		const kind = rawKind as SubmissionKind;
		const built = buildPayload(kind, form);
		if ('error' in built) {
			return fail(400, { message: built.error, kind, values: snapshot(form) });
		}
		const base = normaliseGatewayBase(env.GATEWAY_HTTP_URL);
		let response: Response;
		try {
			response = await fetch(submissionsUrl(base), {
				method: 'POST',
				headers: withCookieHeader(
					new Headers({
						accept: 'application/json',
						'content-type': 'application/json'
					}),
					request.headers.get('cookie')
				),
				body: JSON.stringify(built)
			});
		} catch (e) {
			console.error('[/submit] gateway unreachable on create:', e);
			return fail(503, { message: GATEWAY_UNREACHABLE_MESSAGE, kind, values: snapshot(form) });
		}
		if (response.status === 401) {
			// Echo `kind` + `values` so a no-JS submit still
			// re-renders the in-progress step 2 form with the
			// user's input intact. Without these, the page
			// resets to step 1 and the error message displays
			// detached from the form fields the user was just
			// looking at.
			return fail(401, {
				message: 'Your session has expired. Please sign in again.',
				kind,
				values: snapshot(form)
			});
		}
		if (!response.ok) {
			const errBody = parseGatewayErrorBody(await response.json().catch(() => null));
			const message = errBody?.message ?? 'Submission failed. Please review the form.';
			return fail(response.status, { message, kind, values: snapshot(form) });
		}
		const created = parseCreateSubmissionResponse(await response.json().catch(() => null));
		if (created === null) {
			return fail(502, {
				message: 'The gateway returned an unexpected response shape.',
				kind,
				values: snapshot(form)
			});
		}
		return { created };
	}
};

/**
 * Allowlist of field names the four submission kinds use.
 * `snapshot` only echoes these — extra fields a malicious
 * client could attach to inflate the response are dropped.
 */
const SNAPSHOT_ALLOWED_FIELDS = [
	'kind',
	'title',
	'name',
	'description',
	'source_url',
	'demo_url',
	'repo_url',
	'license',
	'language',
	'domain_slug'
] as const;

/**
 * Capture the form values so we can re-render the in-progress
 * form on a validation failure without forcing the user to
 * retype everything.
 *
 * Two guards live here as defence against a malicious client:
 *
 *   1. **Allowlist of field names** — only keys the form
 *      template actually renders survive. A POST that sneaks
 *      in arbitrary extra fields cannot bounce them back via
 *      the action response.
 *   2. **Per-field length clamp** — each value is truncated to
 *      slightly above the gateway's authoritative cap. The
 *      preflight rejects over-cap values BEFORE this function
 *      is called, but the 401/5xx branches snapshot a form
 *      that hasn't gone through the preflight; without the
 *      clamp an attacker could POST megabytes per field and
 *      have them reflected.
 */
function snapshot(form: FormData): Record<string, string> {
	const out: Record<string, string> = {};
	for (const name of SNAPSHOT_ALLOWED_FIELDS) {
		const raw = form.get(name);
		if (typeof raw !== 'string') continue;
		// `description` is the only field with a 2 KiB cap;
		// everything else is bounded by `SUBMISSION_FIELD_LIMITS
		// .name` (120 chars) or `.url` (2048). Give a small
		// headroom (×1.1) so a borderline-over value is still
		// echoed for the user to fix rather than truncated to
		// confusion.
		const limit = Math.ceil(
			(name === 'description'
				? SUBMISSION_FIELD_LIMITS.description
				: name === 'source_url' || name === 'demo_url' || name === 'repo_url'
					? SUBMISSION_FIELD_LIMITS.url
					: SUBMISSION_FIELD_LIMITS.name) * 1.1
		);
		// Clamp by Unicode scalar values to match the
		// preflight + Rust validator. A `raw.slice(0, limit)`
		// would (a) count UTF-16 code units (so emoji etc. get
		// limited to half the intended cap) and (b) risk
		// splitting a surrogate pair, producing a malformed
		// string in the echo. The `[...raw]` iterator yields
		// scalar values and `join('')` reassembles them safely.
		const scalars = [...raw];
		out[name] = scalars.length > limit ? scalars.slice(0, limit).join('') : raw;
	}
	return out;
}
