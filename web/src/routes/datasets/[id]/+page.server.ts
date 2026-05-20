import { error } from '@sveltejs/kit';
import { findDatasetBySlug } from '$lib/datasets/load';
import type { PageServerLoad } from './$types';

/**
 * Resolves the dataset record for /datasets/[id]. 404s cleanly if the
 * id is unknown so the SEO crawl doesn't index empty pages.
 *
 * 5-min stale-while-revalidate matches /domains and /collections so
 * the marketplace surfaces share a single cache rhythm.
 */
export const load: PageServerLoad = ({ params, setHeaders }) => {
	const dataset = findDatasetBySlug(params.id);
	if (!dataset) {
		throw error(404, `Dataset "${params.id}" not found`);
	}
	setHeaders({
		'cache-control': 'public, max-age=300, stale-while-revalidate=300'
	});
	return { dataset };
};
