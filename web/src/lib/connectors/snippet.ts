import type { Connector } from './types';

/**
 * Render the YAML-supplied config template as the
 * `{"mcpServers": {<slug>: <template>}}` envelope every supported
 * MCP client (Claude Desktop, Cursor, Cline) accepts.
 *
 * Two-space indent keeps the snippet copy-paste friendly in narrow
 * card widths. The `env` key is omitted when absent so
 * `token_required: false` cards render a tighter snippet (instead
 * of `"env": {}`).
 *
 * Lives in its own module so both `/connectors` (cards) and
 * `/connectors/[slug]` (install guide) render identical text —
 * if the envelope shape ever needs to change, one edit covers
 * both surfaces.
 */
export function renderInstallSnippet(c: Connector): string {
	const inner: Record<string, unknown> = {
		command: c.mcp_config_template.command,
		args: [...c.mcp_config_template.args]
	};
	if (c.mcp_config_template.env) {
		inner.env = { ...c.mcp_config_template.env };
	}
	return JSON.stringify({ mcpServers: { [c.slug]: inner } }, null, 2);
}
