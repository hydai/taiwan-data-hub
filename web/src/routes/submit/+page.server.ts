/**
 * `/submit` page server (#5a.1). Renders the multi-step form
 * for authenticated users in multi-user mode; the SvelteKit
 * `create` action POSTs the validated payload to the gateway.
 *
 * Personal-mode + anonymous traffic are routed to the
 * "please sign in" branch by the load function so the form
 * is never rendered in a context it can't submit from. The
 * layout already drives auth-aware navigation; this server
 * just hardens the access gate at the route level.
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
		console.error('[/submit] unexpected /me status:', response.status);
		return { state: 'unexpected', message: 'Unexpected response from the gateway.' };
	}
	const body = await response.json().catch(() => null);
	if (body === null || typeof body !== 'object' || (body as { user: unknown }).user === null) {
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
			return fail(401, { message: 'Your session has expired. Please sign in again.' });
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
 * Capture the form values so we can re-render the in-progress
 * form on a validation failure without forcing the user to
 * retype everything. Only string-typed fields survive — file
 * uploads aren't part of the current schema.
 */
function snapshot(form: FormData): Record<string, string> {
	const out: Record<string, string> = {};
	for (const [k, v] of form.entries()) {
		if (typeof v === 'string') {
			out[k] = v;
		}
	}
	return out;
}
