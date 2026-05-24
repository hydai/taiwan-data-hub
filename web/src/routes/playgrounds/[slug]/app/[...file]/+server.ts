import { error } from '@sveltejs/kit';
import { playgroundCspHeader } from '$lib/playgrounds/csp';
import {
	getPlaygroundAsset,
	getPlaygroundIndexHtml,
	getPlayground
} from '$lib/playgrounds/registry';
import shimSource from '$lib/playgrounds/shim.js?raw';
import type { RequestHandler } from './$types';

/**
 * Serves the playground content (HTML, JS, CSS, assets) with the
 * strict CSP that defines the iframe sandbox contract. The path
 * `/playgrounds/<slug>/app/<file>` reads `playgrounds/<slug>/<file>`
 * from the build-time registry.
 *
 * Reserved filenames:
 *   - `` (empty / trailing-slash) → `index.html`
 *   - `index.html`                → the playground's index, served
 *                                    with the strict CSP
 *   - `__framework.js`            → the framework shim (the same
 *                                    bytes for every playground —
 *                                    no per-slug variation, but
 *                                    served under each slug's path
 *                                    so the index.html can use a
 *                                    relative `./__framework.js`
 *                                    reference)
 */
export const prerender = false;

const CONTENT_TYPE_BY_EXT: Readonly<Record<string, string>> = {
	html: 'text/html; charset=utf-8',
	js: 'application/javascript; charset=utf-8',
	mjs: 'application/javascript; charset=utf-8',
	css: 'text/css; charset=utf-8',
	json: 'application/json; charset=utf-8',
	svg: 'image/svg+xml',
	txt: 'text/plain; charset=utf-8'
};

function contentTypeFor(filename: string): string {
	const dot = filename.lastIndexOf('.');
	if (dot < 0) return 'application/octet-stream';
	const ext = filename.slice(dot + 1).toLowerCase();
	return CONTENT_TYPE_BY_EXT[ext] ?? 'application/octet-stream';
}

function applyCspHeaders(response: Response): Response {
	const headers = new Headers(response.headers);
	headers.set('content-security-policy', playgroundCspHeader());
	// Defence-in-depth: prevent the iframe response from being
	// reframed off-host (CSP frame-ancestors covers the same ground,
	// but X-Frame-Options is still respected by older intermediaries).
	headers.set('x-frame-options', 'SAMEORIGIN');
	headers.set('x-content-type-options', 'nosniff');
	headers.set('referrer-policy', 'no-referrer');
	return new Response(response.body, {
		status: response.status,
		statusText: response.statusText,
		headers
	});
}

export const GET: RequestHandler = ({ params }) => {
	const { slug, file } = params;
	if (!getPlayground(slug)) {
		throw error(404, `Playground "${slug}" not found`);
	}
	const filename = file === '' || file === undefined ? 'index.html' : file;
	// Reject path traversal — `[...file]` accepts `/`, which would
	// let an attacker request e.g. `../other-slug/secret`. We only
	// ever look up by a single flat filename in the per-slug
	// registry; refuse anything else.
	if (filename.includes('/') || filename.includes('..') || filename.startsWith('.')) {
		if (filename !== '__framework.js') {
			throw error(404, `Invalid playground asset path "${filename}"`);
		}
	}

	const resolved = resolvePayload(slug, filename);
	if (!resolved) {
		throw error(404, `Playground "${slug}" has no asset "${filename}"`);
	}

	const res = new Response(resolved.body, {
		status: 200,
		headers: { 'content-type': resolved.contentType }
	});
	return applyCspHeaders(res);
};

function resolvePayload(
	slug: string,
	filename: string
): { body: string; contentType: string } | null {
	if (filename === 'index.html') {
		const body = getPlaygroundIndexHtml(slug);
		return body === null ? null : { body, contentType: CONTENT_TYPE_BY_EXT.html };
	}
	if (filename === '__framework.js') {
		return { body: shimSource, contentType: CONTENT_TYPE_BY_EXT.js };
	}
	const body = getPlaygroundAsset(slug, filename);
	return body === null ? null : { body, contentType: contentTypeFor(filename) };
}
