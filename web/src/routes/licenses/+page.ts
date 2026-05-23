/**
 * /licenses — enumerates every license currently in use across the
 * dataset corpus, with counts and (when known) a clickable link to
 * the license document. Added in #5b.6 so users and AI agents have a
 * single place to audit what licensing terms apply to the data
 * surfaced via the marketplace.
 *
 * SSR-only load: the dataset list is static-ish (rebuilt at deploy
 * time from config/datasets.yaml), so there's no benefit to client
 * fetching and the page benefits from SSR's SEO crawl.
 */
import { loadAllDatasets } from '$lib/datasets/load';
import type { PageLoad } from './$types';

export const prerender = true;

interface LicenseGroup {
	/** License name verbatim as it appears in the YAML / DB. */
	name: string;
	/** Optional license-document URL. Picked from the first dataset
	 * in the group that carries one — every dataset under the same
	 * license name SHOULD point at the same URL; if a future YAML
	 * edit accidentally diverges, the first wins (deterministic
	 * because the dataset list is sorted by slug at this stage). */
	url?: string;
	/** Datasets sharing this license. */
	datasets: { slug: string; name: string }[];
}

export const load: PageLoad = () => {
	const all = loadAllDatasets();
	const groups = new Map<string, LicenseGroup>();
	// Walk in slug order so the "first dataset with a URL wins"
	// tiebreak is stable across deploys.
	const sorted = [...all].sort((a, b) => a.slug.localeCompare(b.slug));
	for (const d of sorted) {
		const existing = groups.get(d.license);
		if (existing) {
			existing.datasets.push({ slug: d.slug, name: d.name['zh-TW'] });
			if (!existing.url && d.source.licenseUrl) {
				existing.url = d.source.licenseUrl;
			}
		} else {
			groups.set(d.license, {
				name: d.license,
				url: d.source.licenseUrl,
				datasets: [{ slug: d.slug, name: d.name['zh-TW'] }]
			});
		}
	}
	// Order license groups by descending dataset count (the most
	// common license is most useful at the top), then by name as a
	// stable tiebreak.
	const licenses = [...groups.values()].sort((a, b) => {
		if (a.datasets.length !== b.datasets.length) {
			return b.datasets.length - a.datasets.length;
		}
		return a.name.localeCompare(b.name);
	});
	return { licenses };
};
