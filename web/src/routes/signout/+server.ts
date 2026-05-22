/**
 * Placeholder `/signout` endpoint (#4.8). The header's sign-out
 * `<form method="POST">` posts here. The gateway-side logout
 * endpoint that revokes the session cookie ships in the
 * follow-up; until then this just redirects back to home so
 * the button doesn't dead-end the user.
 */

import { redirect } from '@sveltejs/kit';
import type { RequestHandler } from './$types';

/**
 * POST is the documented sign-out verb (the form in
 * `Header.svelte` posts here). 303 is the canonical "POST
 * succeeded, GO HERE next" redirect status.
 */
export const POST: RequestHandler = () => {
	throw redirect(303, '/');
};

/**
 * GET fallback so a link-prefetcher or hand-crafted curl
 * doesn't error out. Same 303 to home.
 */
export const GET: RequestHandler = () => {
	throw redirect(303, '/');
};
