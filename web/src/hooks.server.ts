/**
 * SvelteKit server-side hooks — Paraglide v2 locale middleware
 * lives here per #7.6 / the v2 migration (`paraglideMiddleware`
 * replaces v1's request-scoped magic from
 * `@inlang/paraglide-sveltekit`).
 *
 * The middleware reads the active locale from the request (URL
 * prefix, cookie, `Accept-Language`, then base locale per the
 * `strategy` in `vite.config.ts`), rewrites the request URL so
 * `/zh-TW/foo` becomes `/foo` for the SvelteKit router, and
 * stashes the resolved locale so `getLocale()` calls anywhere
 * downstream see the same value.
 *
 * `app.html` substitutes `%paraglide.lang%` with the resolved
 * locale via `transformPageChunk` so the initial SSR document
 * carries the right `<html lang>`. Client-side navigation
 * across locales doesn't re-render `app.html`, so
 * `+layout.svelte` runs an `$effect` that re-syncs
 * `document.documentElement.lang` from the live Paraglide
 * runtime — between the two paths the `lang` attribute always
 * matches the active locale.
 */

import type { Handle } from '@sveltejs/kit';
import { paraglideMiddleware } from '$lib/paraglide/server';

export const handle: Handle = ({ event, resolve }) =>
	paraglideMiddleware(event.request, ({ request: localizedRequest, locale }) => {
		event.request = localizedRequest;
		return resolve(event, {
			transformPageChunk: ({ html }) => html.replace('%paraglide.lang%', locale)
		});
	});
