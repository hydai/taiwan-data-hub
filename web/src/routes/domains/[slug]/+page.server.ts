import { error } from '@sveltejs/kit';
import { findDomainBySlug } from '$lib/domains/load';
import { loadDatasetsForDomain } from '$lib/datasets/load';
import { applyFilters, deriveFacets, parseFilters } from '$lib/datasets/filter';
import type { PageServerLoad } from './$types';

/**
 * Page size for the dataset list. Picked so most domains fit on one
 * page (the busiest seed domain has 6 entries today) while leaving
 * headroom for M3 to populate more. Pagination links only appear
 * when the filtered list exceeds this.
 */
const PAGE_SIZE = 12;

const ANCHOR_COUNT = 3;

/**
 * SSR load for /domains/[slug]. Reads filters from the URL (single
 * source of truth — bookmarkable, shareable, survives reload),
 * applies them server-side, and paginates the result. Invalid
 * filter values are silently dropped (see `parseFilters`).
 *
 * 5-min stale-while-revalidate cache matches the rest of the
 * marketplace surfaces.
 */
export const load: PageServerLoad = ({ params, url, setHeaders }) => {
	const domain = findDomainBySlug(params.slug);
	if (!domain) {
		throw error(404, `Domain "${params.slug}" not found`);
	}

	const allInDomain = loadDatasetsForDomain(domain.slug);
	const filters = parseFilters(url.searchParams);
	const filtered = applyFilters(allInDomain, filters);

	const rawPage = Number(url.searchParams.get('page'));
	const totalPages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
	const page = Number.isFinite(rawPage) && rawPage >= 1 ? Math.min(rawPage, totalPages) : 1;
	const pageDatasets = filtered.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE);

	// Facets come from the domain's *unfiltered* list so a user can
	// always see every dimension this domain supports — never a
	// "filter pill disappears after you click it" surprise.
	const facets = deriveFacets(allInDomain);
	const anchors = allInDomain.slice(0, ANCHOR_COUNT);

	setHeaders({
		'cache-control': 'public, max-age=300, stale-while-revalidate=300'
	});

	return {
		domain,
		anchors,
		datasets: pageDatasets,
		totalDatasets: allInDomain.length,
		filteredCount: filtered.length,
		page,
		totalPages,
		pageSize: PAGE_SIZE,
		filters,
		facets
	};
};
