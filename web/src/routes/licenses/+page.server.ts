/**
 * /licenses — enumerates every license currently in use across the
 * dataset corpus, with counts and (when known) a clickable link to
 * the license document. Added in #5b.6 so users and AI agents have a
 * single place to audit what licensing terms apply to the data
 * surfaced via the marketplace.
 *
 * Server-only load (`+page.server.ts`, `PageServerLoad`): the YAML
 * fixture + parsing pipeline lives in `$lib/datasets/load.ts`,
 * which we don't want to ship into the client bundle. Restricting
 * this load to the server keeps the YAML parser and the entire
 * dataset corpus out of the JS payload — the page receives only
 * the small `licenses` array it actually renders.
 */
import { loadAllDatasets } from '$lib/datasets/load';
import type { PageServerLoad } from './$types';

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

export const load: PageServerLoad = () => {
	const all = loadAllDatasets();
	const groups = new Map<string, LicenseGroup>();
	// Walk in slug order so the "first dataset with a URL wins"
	// tiebreak is stable across deploys.
	const sorted = [...all].sort((a, b) => a.slug.localeCompare(b.slug));
	for (const d of sorted) {
		const existing = groups.get(d.license);
		if (existing) {
			existing.datasets.push({ slug: d.slug, name: d.name['zh-TW'] });
			if (d.source.license_url) {
				if (existing.url && existing.url !== d.source.license_url) {
					// Two datasets under the same license name point at
					// different license_url values. Almost certainly a
					// config typo (the YAML schema invariant is
					// "same license → same URL"). Fail loud at
					// prerender / build so a mistake can't ship a
					// silently-wrong canonical link on /licenses or
					// the per-dataset detail page.
					throw new Error(
						`config/datasets.yaml: divergent license_url within license "${d.license}" — ` +
							`previously seen "${existing.url}", dataset "${d.slug}" declares "${d.source.license_url}". ` +
							`All datasets under the same license name must point at the same license_url.`
					);
				}
				if (!existing.url) {
					existing.url = d.source.license_url;
				}
			}
		} else {
			groups.set(d.license, {
				name: d.license,
				url: d.source.license_url,
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
