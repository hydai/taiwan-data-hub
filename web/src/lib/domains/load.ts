import { parse as parseYaml } from 'yaml';
// Vite `?raw` inlines the YAML at build time so the deploy doesn't need
// `fs` access to the repo's `config/` directory. Path is relative to
// this file: web/src/lib/domains/ → ../../../../config/domains.yaml.
import domainsYaml from '../../../../config/domains.yaml?raw';
import type { Domain, DomainCardData, DomainGroup, DomainKind } from './types';

/**
 * Parsed once at module load — `config/domains.yaml` is static and
 * shipped with the build via `?raw`. Subsequent requests just read
 * from this cached object; no per-request file I/O or YAML parsing.
 */
const RAW_DOMAINS: Domain[] = parseYaml(domainsYaml);

/** Section ordering on the marketplace index. */
const GROUP_ORDER: readonly DomainKind[] = ['topical', 'meta', 'horizontal'] as const;

const GROUP_HEADINGS: Record<DomainKind, { heading: string; subheading: string }> = {
	topical: { heading: '主題領域', subheading: 'Topical' },
	meta: { heading: '後設資料', subheading: 'Meta' },
	horizontal: { heading: '橫向資料', subheading: 'Horizontal' }
};

/**
 * Load + group the 20 marketplace domains. Counts are zeroed for now —
 * real dataset counts come online once the storage layer ships
 * (#2.3 / M3). The schema is in place so the UI doesn't need to
 * change when the data does.
 */
export function loadDomainGroups(): DomainGroup[] {
	const enriched: DomainCardData[] = RAW_DOMAINS.map((d) => ({ ...d, count: 0 })).sort(
		(a, b) => a.sort_order - b.sort_order
	);

	return GROUP_ORDER.map((kind) => ({
		kind,
		heading: GROUP_HEADINGS[kind].heading,
		subheading: GROUP_HEADINGS[kind].subheading,
		domains: enriched.filter((d) => d.kind === kind)
	})).filter((group) => group.domains.length > 0);
}
