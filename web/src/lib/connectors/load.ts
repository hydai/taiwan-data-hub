import { parse as parseYaml } from 'yaml';
import connectorsYaml from '../../../../config/connectors.yaml?raw';
import { CONNECTOR_STATUS_SET, type Connector, type ConnectorStatus } from './types';

/**
 * Slug regex — same kebab-case convention as the other YAML-driven
 * marketplace surfaces (domains, collections). Keeping the shape
 * identical means contributors only have to learn one rule.
 */
const SLUG_RE = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;

/**
 * Env-var placeholder regex. Values inside `mcp_config_template.env`
 * MUST look like `<UPPER_SNAKE>` so a contributor can't accidentally
 * paste a real secret into the YAML — git history is forever and a
 * leaked production token is far worse than a strict validator. Real
 * users substitute the placeholder for their actual secret after
 * pasting the snippet into their MCP client config.
 */
const ENV_PLACEHOLDER_RE = /^<[A-Z][A-Z0-9_]*>$/;

/**
 * Env-var NAME regex. Same shape as the placeholder body — POSIX
 * UPPER_SNAKE — so the rendered JSON snippet is always paste-safe
 * regardless of shell.
 */
const ENV_NAME_RE = /^[A-Z][A-Z0-9_]*$/;

/**
 * Allowed schemes for `homepage_url`. https-only — http would make
 * the rendered card a clickable mixed-content link and erode trust
 * in the showcase as a whole.
 */
function isHttpsUrl(value: string): boolean {
	try {
		const u = new URL(value);
		return u.protocol === 'https:';
	} catch {
		return false;
	}
}

/**
 * Narrows `parseYaml`'s `unknown` into `Connector[]` with field-level
 * checks. Throws an error pointing at `config/connectors.yaml` on
 * the first malformed entry so a bad commit fails fast at module
 * load (i.e. at SSR-prerender / build time) rather than at first
 * request.
 */
function assertValidConnectors(value: unknown): asserts value is Connector[] {
	if (!Array.isArray(value)) {
		throw new Error('config/connectors.yaml: top-level value must be an array');
	}
	const seenSlugs = new Set<string>();
	for (let i = 0; i < value.length; i += 1) {
		const raw = value[i];
		if (!raw || typeof raw !== 'object') {
			throw new Error(`config/connectors.yaml[${i}]: entry must be an object`);
		}
		const r = raw as Record<string, unknown>;
		const tag = typeof r.slug === 'string' ? r.slug : `index ${i}`;
		if (typeof r.slug !== 'string' || r.slug.length === 0) {
			throw new Error(`config/connectors.yaml[${i}]: slug must be a non-empty string`);
		}
		if (!SLUG_RE.test(r.slug)) {
			throw new Error(
				`config/connectors.yaml (${tag}): slug must be kebab-case (matches ${SLUG_RE})`
			);
		}
		if (seenSlugs.has(r.slug)) {
			throw new Error(`config/connectors.yaml: duplicate slug "${r.slug}"`);
		}
		seenSlugs.add(r.slug);
		// `typeof === 'number'` admits NaN and Infinity (YAML can parse
		// .nan / .inf), both of which make the downstream sort
		// (a.sort_order - b.sort_order) return NaN and produce undefined
		// ordering. Number.isFinite excludes both.
		if (typeof r.sort_order !== 'number' || !Number.isFinite(r.sort_order)) {
			throw new Error(`config/connectors.yaml (${tag}): sort_order must be a finite number`);
		}
		assertI18nField(r.name_i18n, 'name_i18n', tag);
		assertI18nField(r.description_i18n, 'description_i18n', tag);
		assertI18nField(r.install_instructions_i18n, 'install_instructions_i18n', tag);
		if (typeof r.homepage_url !== 'string' || !isHttpsUrl(r.homepage_url)) {
			throw new Error(`config/connectors.yaml (${tag}): homepage_url must be an https:// URL`);
		}
		if (typeof r.token_required !== 'boolean') {
			throw new Error(`config/connectors.yaml (${tag}): token_required must be a boolean`);
		}
		if (typeof r.status !== 'string' || !CONNECTOR_STATUS_SET.has(r.status as ConnectorStatus)) {
			throw new Error(
				`config/connectors.yaml (${tag}): status must be one of ${[...CONNECTOR_STATUS_SET].join(' | ')}`
			);
		}
		assertValidConfigTemplate(r.mcp_config_template, tag);
		// Cross-field invariant: if the template declares any env vars,
		// the card MUST advertise a token requirement (and vice versa).
		// Drift between the two would silently mislead the user about
		// what they need to obtain before installing.
		const declaresEnv =
			typeof r.mcp_config_template === 'object' &&
			r.mcp_config_template !== null &&
			'env' in r.mcp_config_template;
		if (declaresEnv !== r.token_required) {
			throw new Error(
				`config/connectors.yaml (${tag}): token_required (${r.token_required}) ` +
					`disagrees with whether mcp_config_template declares an env block (${declaresEnv}). ` +
					`Set them to match.`
			);
		}
	}
}

function assertI18nField(value: unknown, field: string, tag: string): void {
	if (!value || typeof value !== 'object') {
		throw new Error(`config/connectors.yaml (${tag}): ${field} must be an object`);
	}
	const v = value as Record<string, unknown>;
	if (typeof v['zh-TW'] !== 'string' || v['zh-TW'].length === 0) {
		throw new Error(`config/connectors.yaml (${tag}): ${field}['zh-TW'] is required`);
	}
	if (v.en !== undefined && (typeof v.en !== 'string' || v.en.length === 0)) {
		throw new Error(
			`config/connectors.yaml (${tag}): ${field}.en must be a non-empty string when present`
		);
	}
}

function assertValidConfigTemplate(value: unknown, tag: string): void {
	if (!value || typeof value !== 'object') {
		throw new Error(`config/connectors.yaml (${tag}): mcp_config_template must be an object`);
	}
	const t = value as Record<string, unknown>;
	if (typeof t.command !== 'string' || t.command.length === 0) {
		throw new Error(
			`config/connectors.yaml (${tag}): mcp_config_template.command must be a non-empty string`
		);
	}
	if (!Array.isArray(t.args) || t.args.length === 0) {
		throw new Error(
			`config/connectors.yaml (${tag}): mcp_config_template.args must be a non-empty array`
		);
	}
	for (let j = 0; j < t.args.length; j += 1) {
		if (typeof t.args[j] !== 'string') {
			throw new Error(
				`config/connectors.yaml (${tag}): mcp_config_template.args[${j}] must be a string`
			);
		}
	}
	if (t.env !== undefined) {
		if (typeof t.env !== 'object' || t.env === null || Array.isArray(t.env)) {
			throw new Error(
				`config/connectors.yaml (${tag}): mcp_config_template.env must be an object map`
			);
		}
		const env = t.env as Record<string, unknown>;
		const envKeys = Object.keys(env);
		if (envKeys.length === 0) {
			throw new Error(
				`config/connectors.yaml (${tag}): mcp_config_template.env must have at least one key when present`
			);
		}
		for (const key of envKeys) {
			if (!ENV_NAME_RE.test(key)) {
				throw new Error(
					`config/connectors.yaml (${tag}): env key "${key}" must be UPPER_SNAKE_CASE`
				);
			}
			const v = env[key];
			if (typeof v !== 'string' || !ENV_PLACEHOLDER_RE.test(v)) {
				throw new Error(
					`config/connectors.yaml (${tag}): env["${key}"] must be a placeholder like <${key}>; ` +
						`real secrets must NEVER be committed to the YAML`
				);
			}
		}
	}
}

const PARSED: unknown = parseYaml(connectorsYaml);
assertValidConnectors(PARSED);
const RAW_CONNECTORS: Connector[] = PARSED;

/**
 * Load all connectors in display order. Sorted by `sort_order`
 * (ascending) so the YAML's intentional ordering is the single
 * source of truth for what users see first.
 */
export function loadConnectors(): Connector[] {
	return [...RAW_CONNECTORS].sort((a, b) => a.sort_order - b.sort_order);
}
