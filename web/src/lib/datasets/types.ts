/**
 * Type shape for `config/datasets.yaml` entries. Mirrors the patterns
 * in `$lib/domains/types.ts` and `$lib/collections/types.ts` so the
 * three marketplace surfaces stay structurally consistent.
 *
 * The Dataset type is intentionally the *client view* (read-only).
 * When M3 wires the real Postgres-backed dataset table, the storage
 * crate will own its own canonical row type; this client type only
 * exposes what the marketplace UI renders.
 */
export type Tier = 'gold' | 'silver' | 'bronze';
export type Format = 'csv' | 'json' | 'geojson' | 'xlsx' | 'parquet' | 'xml';
export type UpdateFrequency = 'daily' | 'weekly' | 'monthly' | 'quarterly' | 'yearly';
export type ResourceKind = 'download' | 'api';

export interface DatasetI18n {
	'zh-TW': string;
	en?: string;
}

export interface DatasetSource {
	publisher: string;
	url: string;
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
