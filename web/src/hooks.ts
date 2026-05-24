/**
 * Universal SvelteKit hooks — Paraglide v2 needs a `reroute`
 * hook so the SvelteKit router resolves a locale-prefixed URL
 * (e.g. `/en/datasets`) to the underlying route module
 * (`/datasets`).
 *
 * On the server `paraglideMiddleware` in `hooks.server.ts`
 * already rewrites `event.request`, but `reroute` is the
 * universal hook that also runs in the client. Without it,
 * client-side navigation to `/en/foo` would 404 because the
 * router would look for a `/en/foo/+page.svelte` that
 * doesn't exist.
 */

import { deLocalizeUrl } from '$lib/paraglide/runtime';
import type { Reroute } from '@sveltejs/kit';

export const reroute: Reroute = ({ url }) => deLocalizeUrl(url).pathname;
