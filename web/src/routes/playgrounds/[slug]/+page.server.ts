import { error } from '@sveltejs/kit';
import { allPlaygroundSlugs, getPlayground } from '$lib/playgrounds/registry';
import type { EntryGenerator, PageServerLoad } from './$types';

/**
 * `/playgrounds/[slug]` — framing page that embeds the playground
 * iframe and wires the postMessage host.
 *
 * NOT prerendered: the framing page is light, but its URL carries
 * the `?state=…` share-link query string. Prerendering would freeze
 * a single URL; keeping it SSR lets each request emit the right
 * canonical URL via `MetaTags`.
 */
export const prerender = false;

export const entries: EntryGenerator = () => {
	// Used only when crawling for the sitemap — every slug,
	// including `_template`, gets a routable entry.
	return allPlaygroundSlugs().map((slug) => ({ slug }));
};

export const load: PageServerLoad = ({ params }) => {
	const playground = getPlayground(params.slug);
	if (!playground) {
		throw error(404, `Playground "${params.slug}" not found`);
	}
	return { playground };
};
