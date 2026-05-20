/**
 * Shape of a single collection entry in `config/collections.yaml`.
 * Mirrors `$lib/domains/types.ts` so the two marketplace surfaces stay
 * structurally consistent.
 */
export interface CollectionI18n {
	'zh-TW': string;
	en?: string;
}

export interface Collection {
	slug: string;
	sort_order: number;
	name: CollectionI18n;
	curator_note: CollectionI18n;
	/** Exactly 6 dataset slugs per the M2 #2.7 DoD. */
	anchor_datasets: readonly string[];
}
