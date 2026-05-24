import { error } from '@sveltejs/kit';
import { loadConnectors } from '$lib/connectors/load';
import { CONNECTOR_CLIENTS } from '$lib/connectors/clients';
import type { EntryGenerator, PageServerLoad } from './$types';

/**
 * `/connectors/[slug]` — per-connector install guide added in #6.2.
 *
 * Prerendered: the YAML is build-time data and prerendering keeps the
 * pages snappy + means the YAML parser ships nowhere near the
 * client bundle. `entries()` enumerates every connector slug so the
 * Vite adapter knows exactly which pages to materialise.
 *
 * 404s on unknown slugs via SvelteKit's `error(404, …)` so prerender
 * doesn't silently emit empty pages for slugs that don't exist.
 */
export const prerender = true;

export const entries: EntryGenerator = () => {
	return loadConnectors().map((c) => ({ slug: c.slug }));
};

export const load: PageServerLoad = ({ params }) => {
	const connector = loadConnectors().find((c) => c.slug === params.slug);
	if (!connector) {
		throw error(404, `Connector "${params.slug}" not found`);
	}
	return { connector, clients: CONNECTOR_CLIENTS };
};
