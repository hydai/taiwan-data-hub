import { loadConnectors } from '$lib/connectors/load';
import type { PageServerLoad } from './$types';

/**
 * SSR load for /connectors. Matches the /collections + /domains
 * caching policy (5-min max-age + 5-min SWR) so all four
 * marketplace surfaces share a single cache rhythm — easier for
 * ops to reason about and to invalidate together.
 *
 * Server-only (`+page.server.ts`): keeps the YAML parser and the
 * static connector corpus out of the JS payload. The page receives
 * only the small `connectors` array it actually renders.
 */
export const load: PageServerLoad = ({ setHeaders }) => {
	setHeaders({
		'cache-control': 'public, max-age=300, stale-while-revalidate=300'
	});
	return { connectors: loadConnectors() };
};
