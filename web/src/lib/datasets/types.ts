/**
 * Type shape for `config/datasets.yaml` entries. Mirrors the patterns
 * in `$lib/domains/types.ts` and `$lib/collections/types.ts` so the
 * three marketplace surfaces stay structurally consistent.
 *
 * The Dataset type is intentionally the *client view* (read-only).
 * When M3 wires the real Postgres-backed dataset table, the storage
 * crate will own its own canonical row type; this client type only
 * exposes what the marketplace UI renders.
 *
 * Single-source-of-truth pattern: the readonly tuple of valid values
 * doubles as the derived type union and as the runtime list for
 * validation + display ordering. Adding a new enum value means
 * editing one array; the type union, the validator Set, and every
 * consumer (load.ts, filter.ts, components) update transparently.
 */
export const TIERS = ['gold', 'silver', 'bronze'] as const;
export type Tier = (typeof TIERS)[number];
export const TIER_SET: ReadonlySet<Tier> = new Set(TIERS);

export const FORMATS = ['csv', 'json', 'geojson', 'xlsx', 'parquet', 'xml'] as const;
export type Format = (typeof FORMATS)[number];
export const FORMAT_SET: ReadonlySet<Format> = new Set(FORMATS);

export const UPDATE_FREQUENCIES = ['daily', 'weekly', 'monthly', 'quarterly', 'yearly'] as const;
export type UpdateFrequency = (typeof UPDATE_FREQUENCIES)[number];
export const UPDATE_FREQUENCY_SET: ReadonlySet<UpdateFrequency> = new Set(UPDATE_FREQUENCIES);

export const RESOURCE_KINDS = ['download', 'api'] as const;
export type ResourceKind = (typeof RESOURCE_KINDS)[number];
export const RESOURCE_KIND_SET: ReadonlySet<ResourceKind> = new Set(RESOURCE_KINDS);

export interface DatasetI18n {
	'zh-TW': string;
	en?: string;
}

export interface DatasetSource {
	publisher: string;
	url: string;
	/**
	 * Canonical URL for the license document declared in
	 * `Dataset.license`. Optional — not every license has a
	 * stable web home, and some upstream sources omit it.
	 * Added in #5b.6 so the dataset detail page can render a
	 * clickable license link and the /licenses page can group
	 * datasets by shared license URL.
	 *
	 * Snake-case to match the other config/*.yaml keys
	 * (`domain_slug`, `sort_order`, `anchor_datasets`) and
	 * the SQL column name (`datasets.license_url`).
	 */
	license_url?: string;
}

export interface DatasetResource {
	kind: ResourceKind;
	label: string;
	url: string;
}

export interface Dataset {
	slug: string;
	domain_slug: string;
	sort_order: number;
	name: DatasetI18n;
	description: DatasetI18n;
	tier: Tier;
	format: Format;
	license: string;
	source: DatasetSource;
	updated: UpdateFrequency;
	resources: readonly DatasetResource[];
}
