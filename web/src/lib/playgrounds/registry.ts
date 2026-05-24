import {
	PLAYGROUND_STATUS_SET,
	type Playground,
	type PlaygroundManifest,
	type PlaygroundStatus
} from './types';

/**
 * Discover every playground from `/playgrounds/*` at build time.
 *
 * Vite's `import.meta.glob` resolves the glob at compile time, so
 * the filesystem walk happens once during `pnpm build` rather than
 * on every request. The eager + raw imports inline the file
 * contents into the bundle — sized appropriately because manifests
 * are small JSON and the HTML/JS payloads are user-authored
 * playground source.
 *
 * Why the leading `/playgrounds/` glob path?
 *   The glob is resolved relative to the Vite project root
 *   (`web/vite.config.ts` → `web/`). Walking up one level to the
 *   monorepo root is the only way to reach the sibling
 *   `playgrounds/` directory; the `/../playgrounds/*` shape works
 *   because Vite supports glob paths that start with `/` (relative
 *   to project root) and traverses outward with `..`. The
 *   alternative — symlinking `playgrounds/` into `web/static/` —
 *   would lose per-response CSP headers.
 */
const MANIFEST_MODULES = import.meta.glob<PlaygroundManifest>('/../playgrounds/*/manifest.json', {
	eager: true,
	import: 'default'
});

const INDEX_HTML_MODULES = import.meta.glob<string>('/../playgrounds/*/index.html', {
	eager: true,
	query: '?raw',
	import: 'default'
});

const ASSET_MODULES = import.meta.glob<string>('/../playgrounds/*/*', {
	eager: true,
	query: '?raw',
	import: 'default'
});

const SLUG_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

/**
 * Slugs we don't surface in the marketplace index. `_template` is
 * the in-tree reference implementation and self-test; it's still
 * routable directly (so `/playgrounds/_template` works for
 * playground authors copying from it) but it shouldn't clutter the
 * public index.
 */
const HIDDEN_FROM_INDEX_SLUGS = new Set(['_template']);

interface RegistryEntry {
	playground: Playground;
	indexHtml: string;
	/** Map of `filename → file contents` for assets in the slug dir. */
	assets: Map<string, string>;
}

/**
 * Asset extensions the framework serves from a playground directory.
 * Restricted to text-encodable formats because the build-time loader
 * uses `?raw` (UTF-8 string), which would corrupt binary content
 * like PNG / JPG / WOFF. If a playground needs binary assets in a
 * future iteration, add a parallel `?url` glob + a binary-aware
 * serving path; for now reject so the failure is loud at build,
 * not silent at request time.
 *
 * Declared BEFORE `buildRegistry()` runs so `collectAssets` (which
 * is hoisted but references this set) doesn't hit the const's
 * temporal dead zone during module evaluation.
 */
const ALLOWED_ASSET_EXTENSIONS: ReadonlySet<string> = new Set([
	'js',
	'mjs',
	'css',
	'json',
	'svg',
	'txt',
	'html'
]);

function extensionOf(filename: string): string {
	const dot = filename.lastIndexOf('.');
	return dot < 0 ? '' : filename.slice(dot + 1).toLowerCase();
}

const REGISTRY: ReadonlyMap<string, RegistryEntry> = buildRegistry();

function buildRegistry(): Map<string, RegistryEntry> {
	const map = new Map<string, RegistryEntry>();
	const slugFromManifestPath = (path: string): string => {
		// path looks like "/../playgrounds/<slug>/manifest.json"
		const parts = path.split('/');
		return parts[parts.length - 2];
	};

	for (const [path, manifest] of Object.entries(MANIFEST_MODULES)) {
		const slug = slugFromManifestPath(path);
		validateSlug(slug, path);
		const indexPath = path.replace(/\/manifest\.json$/, '/index.html');
		const indexHtml = INDEX_HTML_MODULES[indexPath];
		if (typeof indexHtml !== 'string') {
			throw new Error(
				`playgrounds/${slug}: index.html missing (expected at ${indexPath}). ` +
					`Every playground must ship a manifest.json AND an index.html.`
			);
		}
		validateManifest(slug, manifest);
		const assets = collectAssets(slug);
		map.set(slug, {
			playground: { slug, ...manifest },
			indexHtml,
			assets
		});
	}
	return map;
}

function validateSlug(slug: string, path: string): void {
	if (!SLUG_RE.test(slug) && slug !== '_template') {
		throw new Error(
			`playgrounds: directory name "${slug}" (from ${path}) must be kebab-case ` +
				`(matches ${SLUG_RE}) or the reserved name "_template"`
		);
	}
}

function validateManifest(slug: string, manifest: unknown): asserts manifest is PlaygroundManifest {
	if (!manifest || typeof manifest !== 'object') {
		throw new Error(`playgrounds/${slug}: manifest.json must be an object`);
	}
	const m = manifest as Record<string, unknown>;
	assertI18nField(slug, m.title_i18n, 'title_i18n');
	assertI18nField(slug, m.description_i18n, 'description_i18n');
	if (!Array.isArray(m.tags)) {
		throw new Error(`playgrounds/${slug}: manifest.tags must be an array`);
	}
	for (let i = 0; i < m.tags.length; i += 1) {
		if (typeof m.tags[i] !== 'string' || !SLUG_RE.test(m.tags[i] as string)) {
			throw new Error(`playgrounds/${slug}: manifest.tags[${i}] must be a kebab-case string`);
		}
	}
	if (typeof m.status !== 'string' || !PLAYGROUND_STATUS_SET.has(m.status as PlaygroundStatus)) {
		throw new Error(
			`playgrounds/${slug}: manifest.status must be one of ${[...PLAYGROUND_STATUS_SET].join(' | ')}`
		);
	}
}

function assertI18nField(slug: string, value: unknown, field: string): void {
	if (!value || typeof value !== 'object') {
		throw new Error(`playgrounds/${slug}: manifest.${field} must be an object`);
	}
	const v = value as Record<string, unknown>;
	if (typeof v['zh-TW'] !== 'string' || v['zh-TW'].length === 0) {
		throw new Error(`playgrounds/${slug}: manifest.${field}['zh-TW'] is required`);
	}
	if (v.en !== undefined && (typeof v.en !== 'string' || v.en.length === 0)) {
		throw new Error(
			`playgrounds/${slug}: manifest.${field}.en must be a non-empty string when present`
		);
	}
}

function collectAssets(slug: string): Map<string, string> {
	// Vite's glob result keys lose the leading slash from the glob
	// pattern (the glob is e.g. `/../playgrounds/*/*` but the
	// returned key is `../playgrounds/_template/app.js`). The
	// prefix MUST match the stored shape — leading `/` here meant
	// `startsWith` never returned true and the asset map was always
	// empty, surfacing as 404s for every playground asset.
	const prefix = `../playgrounds/${slug}/`;
	const out = new Map<string, string>();
	for (const [path, body] of Object.entries(ASSET_MODULES)) {
		if (!path.startsWith(prefix)) continue;
		const filename = path.slice(prefix.length);
		// Skip the two files the framework treats specially. index.html
		// is served by `getPlaygroundIndexHtml`; manifest.json is
		// build-time-only metadata that must not leak into the iframe.
		if (filename === 'index.html' || filename === 'manifest.json') continue;
		// Reject path traversal in filenames as a defence-in-depth
		// belt — Vite's glob shouldn't ever produce these, but the
		// downstream router uses the filename verbatim.
		if (filename.includes('/') || filename.includes('..')) continue;
		const ext = extensionOf(filename);
		if (!ALLOWED_ASSET_EXTENSIONS.has(ext)) {
			throw new Error(
				`playgrounds/${slug}: asset "${filename}" has unsupported extension ".${ext}". ` +
					`Allowed text-only extensions: ${[...ALLOWED_ASSET_EXTENSIONS].join(', ')}. ` +
					`Binary assets (PNG, JPG, WOFF, …) would be corrupted by the ?raw loader; ` +
					`they need a binary-aware path that doesn't exist yet.`
			);
		}
		out.set(filename, body);
	}
	return out;
}

/** All playgrounds, sorted by slug for stable display ordering. */
export function loadPlaygrounds(): Playground[] {
	return [...REGISTRY.values()]
		.map((e) => e.playground)
		.filter((p) => !HIDDEN_FROM_INDEX_SLUGS.has(p.slug))
		.sort((a, b) => a.slug.localeCompare(b.slug));
}

/** Single playground by slug (includes hidden ones). */
export function getPlayground(slug: string): Playground | null {
	return REGISTRY.get(slug)?.playground ?? null;
}

export function getPlaygroundIndexHtml(slug: string): string | null {
	return REGISTRY.get(slug)?.indexHtml ?? null;
}

export function getPlaygroundAsset(slug: string, filename: string): string | null {
	return REGISTRY.get(slug)?.assets.get(filename) ?? null;
}

/** Every playground slug, including hidden ones. Used by sitemap + prerender. */
export function allPlaygroundSlugs(): string[] {
	return [...REGISTRY.keys()];
}
