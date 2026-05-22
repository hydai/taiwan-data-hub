/**
 * Helpers for talking to the gateway's `/v1/api-keys` surface
 * from SvelteKit server-side load + actions.
 *
 * Pure-functional shape so it's straightforward to unit-test by
 * passing in a `fetch` stub — no module-level state, no
 * singletons.
 */

import type { ApiKeySummary, IssuedApiKey } from './types';

/**
 * Resolve the gateway's HTTP base URL. The `+page.server.ts`
 * file owns the precise env loading; this helper just normalises
 * a trailing-slash so callers can do `${base}/v1/api-keys`
 * without worrying about double slashes.
 */
export function normaliseGatewayBase(raw: string | undefined): string {
	const base = (raw ?? '').trim();
	if (base.length === 0) {
		// Empty base means "use the same origin" — leave it
		// blank so `fetch('/v1/api-keys', ...)` resolves against
		// the page request's own origin. SvelteKit's server-side
		// `fetch` will then forward cookies automatically when
		// the request is same-origin.
		return '';
	}
	return base.replace(/\/+$/, '');
}

/** Build the absolute URL for the api-keys collection. */
export function apiKeysUrl(base: string): string {
	return `${base}/v1/api-keys`;
}

/** Build the absolute URL for a single key by id. */
export function apiKeyByIdUrl(base: string, id: string): string {
	return `${base}/v1/api-keys/${encodeURIComponent(id)}`;
}

/** Build the absolute URL for the rotate action on a key. */
export function rotateApiKeyUrl(base: string, id: string): string {
	return `${base}/v1/api-keys/${encodeURIComponent(id)}/rotate`;
}

/**
 * Forward the inbound request's `Cookie:` header so SvelteKit's
 * server-side `fetch` carries the user's session cookie through
 * to the gateway. SvelteKit's `event.fetch` does this
 * automatically for same-origin requests; this helper exists for
 * the cross-origin case (compose deployment where `web` and
 * `gateway` run on different domains).
 */
export function withCookieHeader(headers: Headers, cookieHeader: string | null): Headers {
	const copy = new Headers(headers);
	if (cookieHeader) {
		copy.set('cookie', cookieHeader);
	}
	return copy;
}

/**
 * Distinguish the three error shapes a SvelteKit load needs to
 * handle distinctly:
 *
 * - `unauthenticated` — 401 from the gateway: render "please log
 *   in" rather than the keys list.
 * - `unavailable` — gateway is down / connection refused / 5xx:
 *   render "service temporarily unavailable" but keep the page
 *   shell visible.
 * - `unexpected` — anything else: surface a generic error to the
 *   user and log to the server console for triage.
 */
export type GatewayErrorKind = 'unauthenticated' | 'unavailable' | 'unexpected';

export interface GatewayErrorBody {
	error: string;
	message: string;
}

/**
 * Coerce an arbitrary JSON-decoded value into a
 * [`GatewayErrorBody`] when it has the right shape, returning
 * `null` for anything else (so callers can fall back to a
 * generic message rather than echoing untrusted strings into the
 * UI).
 */
export function parseGatewayErrorBody(value: unknown): GatewayErrorBody | null {
	if (value === null || typeof value !== 'object') {
		return null;
	}
	const v = value as Record<string, unknown>;
	if (typeof v.error !== 'string' || typeof v.message !== 'string') {
		return null;
	}
	return { error: v.error, message: v.message };
}

/**
 * Map an HTTP status to the [`GatewayErrorKind`] the page will
 * branch on. Centralised so a future status code (e.g. 403 when
 * scopes land in #4.7+) only needs to be added in one place.
 */
export function classifyGatewayStatus(status: number): GatewayErrorKind {
	if (status === 401) {
		return 'unauthenticated';
	}
	if (status >= 500 && status < 600) {
		return 'unavailable';
	}
	return 'unexpected';
}

/**
 * Narrow `unknown` (the JSON-parsed gateway response) into
 * `ApiKeySummary[]`. Returns `null` when the shape doesn't match
 * so the caller can surface a clear "unexpected response" error
 * instead of letting a runtime `.map` crash render an empty
 * page.
 */
export function parseApiKeySummaryArray(value: unknown): ApiKeySummary[] | null {
	if (!Array.isArray(value)) {
		return null;
	}
	const out: ApiKeySummary[] = [];
	for (const entry of value) {
		const parsed = parseApiKeySummary(entry);
		if (parsed === null) {
			return null;
		}
		out.push(parsed);
	}
	return out;
}

/** Narrow a single api-key summary or return `null` on mismatch. */
export function parseApiKeySummary(value: unknown): ApiKeySummary | null {
	if (value === null || typeof value !== 'object') {
		return null;
	}
	const v = value as Record<string, unknown>;
	if (typeof v.id !== 'string') return null;
	if (typeof v.name !== 'string') return null;
	if (typeof v.key_prefix !== 'string') return null;
	if (!Array.isArray(v.scopes) || !v.scopes.every((s) => typeof s === 'string')) return null;
	if (typeof v.rate_limit_tier !== 'string') return null;
	if (typeof v.created_at !== 'string') return null;
	if (v.last_used_at !== null && typeof v.last_used_at !== 'string') return null;
	if (v.revoked_at !== null && typeof v.revoked_at !== 'string') return null;
	return {
		id: v.id,
		name: v.name,
		key_prefix: v.key_prefix,
		scopes: v.scopes as string[],
		rate_limit_tier: v.rate_limit_tier,
		created_at: v.created_at,
		last_used_at: v.last_used_at as string | null,
		revoked_at: v.revoked_at as string | null
	};
}

/** Narrow the one-time creation response. */
export function parseIssuedApiKey(value: unknown): IssuedApiKey | null {
	if (value === null || typeof value !== 'object') {
		return null;
	}
	const v = value as Record<string, unknown>;
	if (typeof v.id !== 'string') return null;
	if (typeof v.cleartext !== 'string') return null;
	if (typeof v.key_prefix !== 'string') return null;
	return { id: v.id, cleartext: v.cleartext, key_prefix: v.key_prefix };
}
