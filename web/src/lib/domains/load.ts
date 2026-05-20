import { parse as parseYaml } from 'yaml';
// Vite `?raw` inlines the YAML at build time so the deploy doesn't need
// `fs` access to the repo's `config/` directory. Path is relative to
// this file: web/src/lib/domains/ → ../../../../config/domains.yaml.
import domainsYaml from '../../../../config/domains.yaml?raw';
import type { Domain, DomainCardData, DomainGroup, DomainKind } from './types';

const VALID_KINDS: ReadonlySet<DomainKind> = new Set(['topical', 'meta', 'horizontal']);

/**
 * Kebab-case slug regex. Mirrors `SLUG_RE` in
 * `scripts/regen-domain-seed.py` so this runtime guard catches the
 * same shape of bad slug the Python regen script refuses.
 */
const SLUG_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

/**
 * Narrows `parseYaml`'s `unknown` into `Domain[]` with field-level
 * checks. Throws an error pointing at `config/domains.yaml` on the
 * first malformed entry so a bad commit fails fast at module load
 * rather than at first request with a cryptic "cannot read property
 * 'zh-TW' of undefined".
 *
 * Slug validation is strict because slugs serve two invariants
 * simultaneously: (a) URL routing via `resolve('/domains/[slug]')`
 * and (b) Svelte's `{#each ... (key)}` keying. Duplicates would
 * silently corrupt list reactivity; non-kebab slugs would break
 * URLs.
 */
function assertValidDomains(value: unknown): asserts value is Domain[] {
	if (!Array.isArray(value)) {
		throw new Error('config/domains.yaml: top-level value must be an array');
	}
	const seenSlugs = new Set<string>();
	for (let i = 0; i < value.length; i += 1) {
		const raw = value[i];
		if (!raw || typeof raw !== 'object') {
			throw new Error(`config/domains.yaml[${i}]: entry must be an object`);
		}
		const r = raw as Record<string, unknown>;
		const tag = typeof r.slug === 'string' ? r.slug : `index ${i}`;
		if (typeof r.slug !== 'string' || r.slug.length === 0) {
			throw new Error(`config/domains.yaml[${i}]: slug must be a non-empty string`);
		}
		if (!SLUG_RE.test(r.slug)) {
			throw new Error(`config/domains.yaml (${tag}): slug must be kebab-case (matches ${SLUG_RE})`);
		}
		if (seenSlugs.has(r.slug)) {
			throw new Error(`config/domains.yaml: duplicate slug "${r.slug}"`);
		}
		seenSlugs.add(r.slug);
		if (typeof r.kind !== 'string' || !VALID_KINDS.has(r.kind as DomainKind)) {
			throw new Error(`config/domains.yaml (${tag}): kind must be one of topical|meta|horizontal`);
		}
		if (typeof r.sort_order !== 'number') {
			throw new Error(`config/domains.yaml (${tag}): sort_order must be a number`);
		}
		const name = r.name as Record<string, unknown> | undefined;
		if (!name || typeof name['zh-TW'] !== 'string' || name['zh-TW'].length === 0) {
			throw new Error(`config/domains.yaml (${tag}): name['zh-TW'] is required`);
		}
		// typical_questions is optional. If present must be a non-empty
		// array of i18n-shaped objects with required zh-TW strings.
		if (r.typical_questions !== undefined) {
			if (!Array.isArray(r.typical_questions) || r.typical_questions.length === 0) {
				throw new Error(
					`config/domains.yaml (${tag}): typical_questions must be a non-empty array when present`
				);
			}
			for (let k = 0; k < r.typical_questions.length; k += 1) {
				const q = r.typical_questions[k];
				if (!q || typeof q !== 'object') {
					throw new Error(
						`config/domains.yaml (${tag}): typical_questions[${k}] must be an object`
					);
				}
				const qr = q as Record<string, unknown>;
				if (typeof qr['zh-TW'] !== 'string' || qr['zh-TW'].length === 0) {
					throw new Error(
						`config/domains.yaml (${tag}): typical_questions[${k}]['zh-TW'] is required`
					);
				}
				if (qr.en !== undefined && (typeof qr.en !== 'string' || qr.en.length === 0)) {
					throw new Error(
						`config/domains.yaml (${tag}): typical_questions[${k}].en must be a non-empty string when present`
					);
				}
			}
		}
	}
}

/**
 * Parsed once at module load — `config/domains.yaml` is static and
 * shipped with the build via `?raw`. Subsequent requests just read
 * from this cached object; no per-request file I/O or YAML parsing.
 *
 * Validation happens here so deploys fail at startup if the YAML
 * drifts from the schema, rather than at first request.
 */
const PARSED: unknown = parseYaml(domainsYaml);
assertValidDomains(PARSED);
const RAW_DOMAINS: Domain[] = PARSED;

/** Section ordering on the marketplace index. */
const GROUP_ORDER: readonly DomainKind[] = ['topical', 'meta', 'horizontal'] as const;

const GROUP_HEADINGS: Record<DomainKind, { heading: string; subheading: string }> = {
	topical: { heading: '主題領域', subheading: 'Topical' },
	meta: { heading: '後設資料', subheading: 'Meta' },
	horizontal: { heading: '橫向資料', subheading: 'Horizontal' }
};

/** Find a single domain by slug, or `null` if not found. */
export function findDomainBySlug(slug: string): Domain | null {
	return RAW_DOMAINS.find((d) => d.slug === slug) ?? null;
}

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
