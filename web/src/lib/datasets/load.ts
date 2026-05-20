import { parse as parseYaml } from 'yaml';
import datasetsYaml from '../../../../config/datasets.yaml?raw';
import { loadCollections } from '$lib/collections/load';
import { loadDomainGroups } from '$lib/domains/load';
import type {
	Dataset,
	DatasetResource,
	Format,
	ResourceKind,
	Tier,
	UpdateFrequency
} from './types';

const SLUG_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

const VALID_TIERS: ReadonlySet<Tier> = new Set(['gold', 'silver', 'bronze']);
const VALID_FORMATS: ReadonlySet<Format> = new Set([
	'csv',
	'json',
	'geojson',
	'xlsx',
	'parquet',
	'xml'
]);
const VALID_UPDATE_FREQS: ReadonlySet<UpdateFrequency> = new Set([
	'daily',
	'weekly',
	'monthly',
	'quarterly',
	'yearly'
]);
const VALID_RESOURCE_KINDS: ReadonlySet<ResourceKind> = new Set(['download', 'api']);

function asNonEmptyString(value: unknown): string | null {
	return typeof value === 'string' && value.length > 0 ? value : null;
}

function assertValidResource(
	resource: unknown,
	tag: string,
	idx: number
): asserts resource is DatasetResource {
	if (!resource || typeof resource !== 'object') {
		throw new Error(`config/datasets.yaml (${tag}): resources[${idx}] must be an object`);
	}
	const r = resource as Record<string, unknown>;
	if (typeof r.kind !== 'string' || !VALID_RESOURCE_KINDS.has(r.kind as ResourceKind)) {
		throw new Error(
			`config/datasets.yaml (${tag}): resources[${idx}].kind must be one of download|api`
		);
	}
	if (!asNonEmptyString(r.label)) {
		throw new Error(
			`config/datasets.yaml (${tag}): resources[${idx}].label must be a non-empty string`
		);
	}
	if (!asNonEmptyString(r.url)) {
		throw new Error(
			`config/datasets.yaml (${tag}): resources[${idx}].url must be a non-empty string`
		);
	}
}

/**
 * Narrows `parseYaml`'s `unknown` into `Dataset[]` with field-level
 * checks. Same fail-fast philosophy as the domain / collection
 * loaders — throw at module load with a `config/datasets.yaml`-tagged
 * message rather than crash on a missing field at first request.
 */
function assertValidDatasets(value: unknown): asserts value is Dataset[] {
	if (!Array.isArray(value)) {
		throw new Error('config/datasets.yaml: top-level value must be an array');
	}
	const seenSlugs = new Set<string>();
	for (let i = 0; i < value.length; i += 1) {
		const raw = value[i];
		if (!raw || typeof raw !== 'object') {
			throw new Error(`config/datasets.yaml[${i}]: entry must be an object`);
		}
		const r = raw as Record<string, unknown>;
		const tag = typeof r.slug === 'string' ? r.slug : `index ${i}`;
		if (!asNonEmptyString(r.slug) || !SLUG_RE.test(r.slug as string)) {
			throw new Error(`config/datasets.yaml[${i}]: slug must be a kebab-case non-empty string`);
		}
		if (seenSlugs.has(r.slug as string)) {
			throw new Error(`config/datasets.yaml: duplicate slug "${r.slug}"`);
		}
		seenSlugs.add(r.slug as string);
		if (!asNonEmptyString(r.domain_slug) || !SLUG_RE.test(r.domain_slug as string)) {
			throw new Error(`config/datasets.yaml (${tag}): domain_slug must be a kebab-case slug`);
		}
		if (typeof r.sort_order !== 'number' || !Number.isFinite(r.sort_order)) {
			throw new Error(`config/datasets.yaml (${tag}): sort_order must be a finite number`);
		}
		const name = r.name as Record<string, unknown> | undefined;
		if (!name || !asNonEmptyString(name['zh-TW'])) {
			throw new Error(`config/datasets.yaml (${tag}): name['zh-TW'] is required`);
		}
		if (name.en !== undefined && !asNonEmptyString(name.en)) {
			throw new Error(
				`config/datasets.yaml (${tag}): name.en must be a non-empty string when present`
			);
		}
		const desc = r.description as Record<string, unknown> | undefined;
		if (!desc || !asNonEmptyString(desc['zh-TW'])) {
			throw new Error(`config/datasets.yaml (${tag}): description['zh-TW'] is required`);
		}
		if (desc.en !== undefined && !asNonEmptyString(desc.en)) {
			throw new Error(
				`config/datasets.yaml (${tag}): description.en must be a non-empty string when present`
			);
		}
		if (typeof r.tier !== 'string' || !VALID_TIERS.has(r.tier as Tier)) {
			throw new Error(`config/datasets.yaml (${tag}): tier must be one of gold|silver|bronze`);
		}
		if (typeof r.format !== 'string' || !VALID_FORMATS.has(r.format as Format)) {
			throw new Error(
				`config/datasets.yaml (${tag}): format must be one of csv|json|geojson|xlsx|parquet|xml`
			);
		}
		if (!asNonEmptyString(r.license)) {
			throw new Error(`config/datasets.yaml (${tag}): license must be a non-empty string`);
		}
		const source = r.source as Record<string, unknown> | undefined;
		if (!source || !asNonEmptyString(source.publisher) || !asNonEmptyString(source.url)) {
			throw new Error(
				`config/datasets.yaml (${tag}): source.publisher and source.url are required`
			);
		}
		if (typeof r.updated !== 'string' || !VALID_UPDATE_FREQS.has(r.updated as UpdateFrequency)) {
			throw new Error(
				`config/datasets.yaml (${tag}): updated must be one of daily|weekly|monthly|quarterly|yearly`
			);
		}
		if (!Array.isArray(r.resources) || r.resources.length === 0) {
			throw new Error(`config/datasets.yaml (${tag}): resources must be a non-empty array`);
		}
		for (let j = 0; j < r.resources.length; j += 1) {
			assertValidResource(r.resources[j], tag, j);
		}
	}
}

/**
 * Cross-YAML invariants:
 *   1. Every dataset's `domain_slug` must reference an existing domain
 *      in `config/domains.yaml`. A dangling FK would render a card
 *      under a non-existent domain heading and route to a 404 slug.
 *   2. Every `anchor_datasets` slug in `config/collections.yaml` must
 *      reference an existing dataset here. A broken anchor would
 *      render as a dead link on /collections.
 */
function assertCrossReferences(datasets: readonly Dataset[]): void {
	const domainSlugs = new Set(loadDomainGroups().flatMap((g) => g.domains.map((d) => d.slug)));
	for (const d of datasets) {
		if (!domainSlugs.has(d.domain_slug)) {
			throw new Error(
				`config/datasets.yaml (${d.slug}): domain_slug "${d.domain_slug}" not found in config/domains.yaml`
			);
		}
	}

	const datasetSlugs = new Set(datasets.map((d) => d.slug));
	for (const collection of loadCollections()) {
		for (const anchor of collection.anchor_datasets) {
			if (!datasetSlugs.has(anchor)) {
				throw new Error(
					`config/collections.yaml (${collection.slug}): anchor_dataset "${anchor}" not found in config/datasets.yaml`
				);
			}
		}
	}
}

const PARSED: unknown = parseYaml(datasetsYaml);
assertValidDatasets(PARSED);
const RAW_DATASETS: Dataset[] = PARSED;
assertCrossReferences(RAW_DATASETS);

/** All datasets in display order (sort_order ascending). */
export function loadAllDatasets(): Dataset[] {
	return [...RAW_DATASETS].sort((a, b) => a.sort_order - b.sort_order);
}

/** Datasets filtered to a single domain, sorted by sort_order. */
export function loadDatasetsForDomain(domainSlug: string): Dataset[] {
	return RAW_DATASETS.filter((d) => d.domain_slug === domainSlug).sort(
		(a, b) => a.sort_order - b.sort_order
	);
}

/** Find a single dataset by slug, or `null` if not found. */
export function findDatasetBySlug(slug: string): Dataset | null {
	return RAW_DATASETS.find((d) => d.slug === slug) ?? null;
}
