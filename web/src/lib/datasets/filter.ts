import {
	FORMATS,
	FORMAT_SET,
	TIERS,
	TIER_SET,
	type Dataset,
	type Format,
	type Tier
} from './types';

/**
 * URL-driven filter shape for /domains/[slug]. The single source of
 * truth is the URL's `searchParams`; the page reads them, applies
 * `applyFilters`, and renders. No client `$state` for filter values.
 */
export interface DatasetFilters {
	tier: Tier | null;
	format: Format | null;
	license: string | null;
}

/**
 * Parse the three filter dimensions (tier, format, license) from a
 * URL's searchParams. Unknown or malformed values are silently
 * dropped (returns null for that dimension) — defensive design so
 * crawler probes / hand-crafted URLs can't 500 the page. License is
 * free-form so we keep any non-empty string; the page only renders
 * actually-seen-in-data licenses as clickable pills, so a typed-in
 * unknown license just returns an empty list (intended behaviour).
 *
 * The pagination `page` param is parsed separately in
 * `+page.server.ts` and isn't part of `DatasetFilters`.
 */
export function parseFilters(searchParams: URLSearchParams): DatasetFilters {
	const rawTier = searchParams.get('tier');
	const rawFormat = searchParams.get('format');
	const rawLicense = searchParams.get('license');

	return {
		tier: rawTier && TIER_SET.has(rawTier as Tier) ? (rawTier as Tier) : null,
		format: rawFormat && FORMAT_SET.has(rawFormat as Format) ? (rawFormat as Format) : null,
		license: rawLicense && rawLicense.length > 0 ? rawLicense : null
	};
}

/** Apply filters to a dataset list. Null dimensions match everything. */
export function applyFilters(datasets: readonly Dataset[], filters: DatasetFilters): Dataset[] {
	return datasets.filter((d) => {
		if (filters.tier && d.tier !== filters.tier) return false;
		if (filters.format && d.format !== filters.format) return false;
		if (filters.license && d.license !== filters.license) return false;
		return true;
	});
}

/** Distinct values seen in a dataset list, used to render filter pills. */
export interface FilterFacets {
	tiers: readonly Tier[];
	formats: readonly Format[];
	licenses: readonly string[];
}

export function deriveFacets(datasets: readonly Dataset[]): FilterFacets {
	const tiers = new Set<Tier>();
	const formats = new Set<Format>();
	const licenses = new Set<string>();
	for (const d of datasets) {
		tiers.add(d.tier);
		formats.add(d.format);
		licenses.add(d.license);
	}
	// Stable display order: tier follows quality order (TIERS),
	// format follows the type-union enum order (FORMATS), license
	// sorted alphabetically. Both arrays are the single source of
	// truth in types.ts.
	return {
		tiers: TIERS.filter((t) => tiers.has(t)),
		formats: FORMATS.filter((f) => formats.has(f)),
		licenses: [...licenses].sort()
	};
}

/**
 * Build a relative URL with the given filter + page values. Used by
 * the filter pills and pagination links to keep server-driven
 * navigation working without JS-built URL strings sprinkled across
 * the template.
 *
 * `value` of null clears the dimension; otherwise sets it. `page` of
 * 1 (or undefined) drops the param entirely.
 */
export function buildFilterUrl(
	current: DatasetFilters,
	overrides: Partial<DatasetFilters> & { page?: number }
): string {
	const params = new URLSearchParams();
	const next: DatasetFilters = { ...current, ...overrides };
	if (next.tier) params.set('tier', next.tier);
	if (next.format) params.set('format', next.format);
	if (next.license) params.set('license', next.license);
	if (overrides.page && overrides.page > 1) params.set('page', String(overrides.page));
	const qs = params.toString();
	// Empty-string href resolves to the current URL *including* its
	// existing query string in HTML — clicking an active filter to
	// clear it would silently leave the user on the same filtered
	// page. Returning `?` produces a same-path navigation with an
	// empty query, which browsers strip on display.
	return qs.length > 0 ? `?${qs}` : '?';
}
