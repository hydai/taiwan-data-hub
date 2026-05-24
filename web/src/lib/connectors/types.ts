/**
 * Shape of a single connector entry in `config/connectors.yaml`.
 *
 * "Connector" here means an external MCP server we recommend
 * composing with Taiwan Data Hub for AI-agent workflows — NOT
 * Taiwan Data Hub's own data sources (those live in
 * `config/sources.toml` and feed `crates/connectors`).
 *
 * Mirrors `$lib/collections/types.ts` so the marketplace surfaces
 * stay structurally consistent: kebab-case slug + sort_order + i18n
 * map for human-facing fields.
 */
export interface ConnectorI18n {
	'zh-TW': string;
	en?: string;
}

/**
 * Maturity signal shown as a pill on the card. Drives no behaviour
 * beyond visual styling; readers should treat `experimental` as
 * "expect breaking changes" and `beta` as "stable API but feature-
 * incomplete".
 */
export const CONNECTOR_STATUSES = ['stable', 'beta', 'experimental'] as const;
export type ConnectorStatus = (typeof CONNECTOR_STATUSES)[number];
export const CONNECTOR_STATUS_SET: ReadonlySet<ConnectorStatus> = new Set(CONNECTOR_STATUSES);

/**
 * The JSON-renderable block users paste into their MCP client
 * config (Claude Desktop's `claude_desktop_config.json`,
 * Cursor's `~/.cursor/mcp.json`, Cline's settings). The three
 * clients accept the same shape under different top-level keys;
 * #6.2 will format per-client guides on top of this base.
 *
 * `env` values are placeholder strings (e.g. `<NOTION_API_KEY>`)
 * — the loader enforces that so a real secret can never leak via
 * the YAML.
 */
export interface ConnectorMcpConfigTemplate {
	command: string;
	args: readonly string[];
	env?: Readonly<Record<string, string>>;
}

export interface Connector {
	slug: string;
	sort_order: number;
	name_i18n: ConnectorI18n;
	description_i18n: ConnectorI18n;
	install_instructions_i18n: ConnectorI18n;
	homepage_url: string;
	mcp_config_template: ConnectorMcpConfigTemplate;
	token_required: boolean;
	status: ConnectorStatus;
}
