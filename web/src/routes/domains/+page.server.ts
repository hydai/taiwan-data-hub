import { loadDomainGroups } from '$lib/domains/load';
import type { PageServerLoad } from './$types';

/**
 * SSR load for the /domains marketplace index.
 *
 * Cache-Control is set to a 5-minute max-age + 5-minute
 * stale-while-revalidate window, matching the M2 #2.4 DoD. The
 * underlying data is static (YAML in the repo), so we could set
 * a much longer TTL — keeping it modest leaves room for live
 * dataset-count plumbing in #2.3 / M3 to invalidate naturally.
 */
export const load: PageServerLoad = ({ setHeaders }) => {
	setHeaders({
		'cache-control': 'public, max-age=300, stale-while-revalidate=300'
	});
	return { groups: loadDomainGroups() };
};
