import { loadPlaygrounds } from '$lib/playgrounds/registry';
import type { PageServerLoad } from './$types';

/**
 * SSR load for `/playgrounds`. Mirrors `/collections` cache rhythm
 * (5-min max-age + 5-min SWR) for consistency across the
 * marketplace surfaces.
 *
 * Server-only: keeps the playground registry — which holds every
 * playground's full `index.html` + asset payload in memory — out
 * of the client bundle.
 */
export const load: PageServerLoad = ({ setHeaders }) => {
	setHeaders({
		'cache-control': 'public, max-age=300, stale-while-revalidate=300'
	});
	return { playgrounds: loadPlaygrounds() };
};
