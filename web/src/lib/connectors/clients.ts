/**
 * Per-MCP-client install metadata used by `/connectors/[slug]`.
 *
 * All three target clients accept the same
 * `{"mcpServers": {<slug>: <template>}}` envelope, so we only need
 * to vary the surrounding instructions (where to put the file or
 * which UI affordance to use) — not the snippet itself. This is
 * what keeps `config/connectors.yaml` schema-light: one template
 * per connector, three rendered guides per page.
 *
 * Adding a new client (e.g. Continue, Zed) means appending a single
 * entry here; the per-connector page picks it up automatically.
 */
export interface ConnectorClient {
	/** kebab-case identifier, used as the section's HTML id. */
	slug: string;
	/** Display name shown in the section header. */
	label: string;
	/** One-sentence framing for what the client is. */
	tagline: string;
	/** Per-platform install paths, when the client uses a file. */
	configPaths?: ReadonlyArray<{ platform: string; path: string }>;
	/** UI-driven install steps, when the client uses an extension UI. */
	uiSteps?: readonly string[];
	/**
	 * One-sentence post-paste reload hint (each client picks up new
	 * MCP servers differently — restart, reload window, toggle, etc.).
	 */
	reloadHint: string;
}

export const CONNECTOR_CLIENTS: readonly ConnectorClient[] = [
	{
		slug: 'claude-desktop',
		label: 'Claude Desktop',
		tagline: "Anthropic's desktop app for macOS, Windows, and Linux.",
		configPaths: [
			{
				platform: 'macOS',
				path: '~/Library/Application Support/Claude/claude_desktop_config.json'
			},
			{ platform: 'Windows', path: '%APPDATA%\\Claude\\claude_desktop_config.json' },
			{ platform: 'Linux', path: '~/.config/Claude/claude_desktop_config.json' }
		],
		reloadHint:
			'Quit Claude Desktop completely and reopen it; the new server appears in the tools menu on next launch.'
	},
	{
		slug: 'cursor',
		label: 'Cursor',
		tagline: "Cursor's AI-first IDE — supports MCP via a per-user config file.",
		configPaths: [
			{ platform: 'All platforms (global)', path: '~/.cursor/mcp.json' },
			{ platform: 'All platforms (per project)', path: '<project-root>/.cursor/mcp.json' }
		],
		reloadHint:
			'Open the command palette and run "Developer: Reload Window" — Cursor picks up MCP changes on reload.'
	},
	{
		slug: 'cline',
		label: 'Cline (VS Code)',
		tagline: 'The Cline VS Code extension stores MCP settings inside its extension data dir.',
		uiSteps: [
			'Open the Cline panel in VS Code (Activity Bar → Cline icon).',
			'Click the MCP Servers tab → "Edit MCP Settings".',
			'Paste the snippet below into the `mcpServers` object of the opened JSON file and save.'
		],
		reloadHint:
			'Cline reloads the MCP config on save — no window reload needed; the server shows up under "MCP Servers" within a few seconds.'
	}
];
