import { parse as parseYaml } from 'yaml';
import collectionsYaml from '../../../../config/collections.yaml?raw';
import type { Collection } from './types';

/**
 * Slug regex — same shape as the domains validator + the Python
 * regen script. Kept in lockstep so a slug that round-trips through
 * one validator passes the other.
 */
const SLUG_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

/** Required size of `anchor_datasets` per the M2 #2.7 DoD. */
const REQUIRED_ANCHOR_COUNT = 6;

/**
 * Narrows `parseYaml`'s `unknown` into `Collection[]` with field-level
 * checks. Throws an error pointing at `config/collections.yaml` on
 * the first malformed entry so a bad commit fails fast at module
 * load rather than at first request.
 */
function assertValidCollections(value: unknown): asserts value is Collection[] {
	if (!Array.isArray(value)) {
		throw new Error('config/collections.yaml: top-level value must be an array');
	}
	const seenSlugs = new Set<string>();
	for (let i = 0; i < value.length; i += 1) {
		const raw = value[i];
		if (!raw || typeof raw !== 'object') {
			throw new Error(`config/collections.yaml[${i}]: entry must be an object`);
		}
		const r = raw as Record<string, unknown>;
		const tag = typeof r.slug === 'string' ? r.slug : `index ${i}`;
		if (typeof r.slug !== 'string' || r.slug.length === 0) {
			throw new Error(`config/collections.yaml[${i}]: slug must be a non-empty string`);
		}
		if (!SLUG_RE.test(r.slug)) {
			throw new Error(
				`config/collections.yaml (${tag}): slug must be kebab-case (matches ${SLUG_RE})`
			);
		}
		if (seenSlugs.has(r.slug)) {
			throw new Error(`config/collections.yaml: duplicate slug "${r.slug}"`);
		}
		seenSlugs.add(r.slug);
		if (typeof r.sort_order !== 'number') {
			throw new Error(`config/collections.yaml (${tag}): sort_order must be a number`);
		}
		const name = r.name as Record<string, unknown> | undefined;
		if (!name || typeof name['zh-TW'] !== 'string' || name['zh-TW'].length === 0) {
			throw new Error(`config/collections.yaml (${tag}): name['zh-TW'] is required`);
		}
		const note = r.curator_note as Record<string, unknown> | undefined;
		if (!note || typeof note['zh-TW'] !== 'string' || note['zh-TW'].length === 0) {
			throw new Error(`config/collections.yaml (${tag}): curator_note['zh-TW'] is required`);
		}
		const anchors = r.anchor_datasets;
		if (!Array.isArray(anchors) || anchors.length !== REQUIRED_ANCHOR_COUNT) {
			throw new Error(
				`config/collections.yaml (${tag}): anchor_datasets must have exactly ${REQUIRED_ANCHOR_COUNT} entries`
			);
		}
		// Anchors are keyed in the UI by their slug value (Svelte
		// {#each ... (slug)}). A duplicate inside one collection would
		// silently corrupt keyed-each rendering, so fail loudly here.
		const seenAnchors = new Set<string>();
		for (let j = 0; j < anchors.length; j += 1) {
			const a = anchors[j];
			if (typeof a !== 'string' || !SLUG_RE.test(a)) {
				throw new Error(
					`config/collections.yaml (${tag}): anchor_datasets[${j}] must be a kebab-case dataset slug`
				);
			}
			if (seenAnchors.has(a)) {
				throw new Error(`config/collections.yaml (${tag}): duplicate anchor dataset slug "${a}"`);
			}
			seenAnchors.add(a);
		}
	}
}

const PARSED: unknown = parseYaml(collectionsYaml);
assertValidCollections(PARSED);
const RAW_COLLECTIONS: Collection[] = PARSED;

/**
 * Load all collections in display order. Sorted by `sort_order`
 * (ascending) so the YAML's intentional ordering is the single
 * source of truth for what users see first.
 */
export function loadCollections(): Collection[] {
	return [...RAW_COLLECTIONS].sort((a, b) => a.sort_order - b.sort_order);
}
