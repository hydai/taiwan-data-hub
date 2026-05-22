/**
 * Placeholder `/signout` endpoint (#4.8). The header's sign-out
 * `<form method="POST">` posts here. The gateway-side logout
 * endpoint that revokes the session cookie ships in the
 * follow-up; until then this just redirects back to home so
 * the button doesn't dead-end the user.
 *
 * Redirect target is resolved via `$app/paths` so it honours
 * any `paths.base` config (subpath deployments) — a literal
 * `'/'` would send users to the domain root and bypass the
 * app's mount point.
 */

import { resolve } from '$app/paths';
import { redirect } from '@sveltejs/kit';
import type { RequestHandler } from './$types';

/**
 * POST is the documented sign-out verb (the form in
 * `Header.svelte` posts here). 303 is the canonical "POST
 * succeeded, GO HERE next" redirect status.
 */
export const POST: RequestHandler = () => {
	throw redirect(303, resolve('/'));
};

/**
 * GET fallback so a link-prefetcher or hand-crafted curl
 * doesn't error out. Same 303 to home.
 */
export const GET: RequestHandler = () => {
	throw redirect(303, resolve('/'));
};
