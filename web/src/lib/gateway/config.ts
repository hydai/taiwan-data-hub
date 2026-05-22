/**
 * Helpers for the gateway's `/api/v1/config` + `/api/v1/me`
 * endpoints (#4.8). Used by `+layout.server.ts` to drive
 * auth-conditional rendering: personal mode hides the login
 * UI; multi-user mode renders signin / signup CTAs when
 * anonymous and user identity + sign-out when authenticated.
 *
 * Cross-origin deploys (web on one host, gateway on another)
 * need the session cookie forwarded into the `/api/v1/me`
 * fetch; same-origin SSR through SvelteKit's `event.fetch`
 * carries cookies automatically. The session cookie name is
 * kept in lockstep with `auth::SESSION_COOKIE_NAME` via the
 * `FORWARDED_COOKIE_NAME` constant in `account/gateway.ts`.
 */

import type { GatewayMode } from './types';

export interface ConfigResponse {
	mode: GatewayMode;
}

export interface MeUser {
	user_id: string;
	session_created_at: string;
	session_expires_at: string;
}

export interface MeResponse {
	user: MeUser | null;
}

/**
 * Narrow an arbitrary JSON value into a [`ConfigResponse`].
 * Returns `null` for any shape mismatch — caller falls back
 * to a safe default (`mode: 'personal'`) so the layout still
 * renders if the gateway is briefly returning HTML during a
 * startup race.
 */
export function parseConfigResponse(value: unknown): ConfigResponse | null {
	if (value === null || typeof value !== 'object') {
		return null;
	}
	const v = value as Record<string, unknown>;
	if (v.mode === 'personal' || v.mode === 'multi-user') {
		return { mode: v.mode };
	}
	return null;
}

/**
 * Narrow an arbitrary JSON value into a [`MeResponse`].
 * `user: null` is the documented anonymous shape; the
 * authenticated branch checks for the three documented user
 * fields. Returns `null` on shape mismatch so the caller can
 * surface a typed error.
 */
export function parseMeResponse(value: unknown): MeResponse | null {
	if (value === null || typeof value !== 'object') {
		return null;
	}
	const v = value as Record<string, unknown>;
	if (!('user' in v)) {
		return null;
	}
	if (v.user === null) {
		return { user: null };
	}
	if (typeof v.user !== 'object') {
		return null;
	}
	const u = v.user as Record<string, unknown>;
	if (typeof u.user_id !== 'string') return null;
	if (typeof u.session_created_at !== 'string') return null;
	if (typeof u.session_expires_at !== 'string') return null;
	return {
		user: {
			user_id: u.user_id,
			session_created_at: u.session_created_at,
			session_expires_at: u.session_expires_at
		}
	};
}
