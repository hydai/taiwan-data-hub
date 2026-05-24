/**
 * Shapes for `playgrounds/<slug>/manifest.json` entries.
 *
 * Each playground subdirectory ships a manifest that the framework
 * discovers at build time (`registry.ts`). Slug is derived from the
 * directory name, not stored in the manifest, so the on-disk layout
 * and the marketplace URL are guaranteed to match.
 */
export interface PlaygroundI18n {
	'zh-TW': string;
	en?: string;
}

export const PLAYGROUND_STATUSES = ['stable', 'beta', 'experimental'] as const;
export type PlaygroundStatus = (typeof PLAYGROUND_STATUSES)[number];
export const PLAYGROUND_STATUS_SET: ReadonlySet<PlaygroundStatus> = new Set(PLAYGROUND_STATUSES);

export interface PlaygroundManifest {
	title_i18n: PlaygroundI18n;
	description_i18n: PlaygroundI18n;
	tags: readonly string[];
	status: PlaygroundStatus;
}

export interface Playground extends PlaygroundManifest {
	slug: string;
}
