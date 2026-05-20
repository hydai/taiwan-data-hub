import { loadCollections } from '$lib/collections/load';
import type { PageServerLoad } from './$types';

/**
 * SSR load for /collections. Matches the /domains caching policy
 * (5-min max-age + 5-min SWR) so the marketplace surfaces share
 * a single cache rhythm — easier for ops to reason about.
 */
export const load: PageServerLoad = ({ setHeaders }) => {
	setHeaders({
		'cache-control': 'public, max-age=300, stale-while-revalidate=300'
	});
	return { collections: loadCollections() };
};
