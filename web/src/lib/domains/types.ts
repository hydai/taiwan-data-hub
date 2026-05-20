/**
 * Shape of a single domain entry in `config/domains.yaml`.
 *
 * Kept narrow on purpose: this is the read-only client view, not the
 * full canonical schema that the backend writes/migrates. New optional
 * fields (e.g. tags, owners) can be added without breaking consumers.
 */
export type DomainKind = 'topical' | 'meta' | 'horizontal';

export interface DomainI18n {
	'zh-TW': string;
	en?: string;
}

export interface Domain {
	slug: string;
	kind: DomainKind;
	sort_order: number;
	name: DomainI18n;
	description?: DomainI18n;
	/**
	 * Editorial list of representative agent-friendly questions this
	 * domain answers (rendered on /domains/[slug]). Optional —
	 * domains without populated entries don't render the section.
	 */
	typical_questions?: readonly DomainI18n[];
}

/** A domain enriched with the per-domain dataset count for the marketplace UI. */
export interface DomainCardData extends Domain {
	/** Number of datasets registered under this domain. */
	count: number;
}

/** Group key used to render the section dividers on /domains. */
export interface DomainGroup {
	kind: DomainKind;
	/** zh-TW heading shown above the grid. */
	heading: string;
	/** en sub-heading shown small under the zh-TW heading. */
	subheading: string;
	domains: DomainCardData[];
}
