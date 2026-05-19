# Taiwan Data Hub

> An open-source, self-hostable MCP service hub for Taiwan public data.
> 完全開源、可自託管的台灣公開資料 MCP 服務聚合平台。

[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-design-orange.svg)](docs/DESIGN.md)

## What is this?

Taiwan Data Hub aggregates Taiwan public data sources (data.gov.tw, TWSE, MOEA,
CWA, …) and exposes them through the Model Context Protocol (MCP), so AI agents
like Claude Desktop, Cursor, and Cline can query Taiwan data with a single
configuration line.

It is a fully open-source alternative to `hub.twinkleai.tw`, with these
differentiators:

- 🆓 **Free forever** — no per-tool-call billing, no API quotas
- 🏠 **Self-hosted** — `docker compose up` and you own the data
- 🤝 **Community-driven** — submit datasets, tools, connectors, and playground demos
- 🔌 **Multi-source** — not just data.gov.tw; TWSE, MOEA, CWA, and more
- 🌏 **i18n** — zh-TW, en, ja, ko, fr
- 👤 **Personal mode** — toggle auth off for a single-user local install

## Status

**Pre-alpha.** M0 Foundations complete. M1 MCP MVP in progress — the stdio
and Streamable HTTP transports are live and `list_domains` is the first tool
to ship. Subsequent M1 work brings the `data.gov.tw` crawler online and
adds the four remaining base tools. The full system design lives in
[`docs/DESIGN.md`](docs/DESIGN.md); eight milestones (M0–M7) decompose the
work into 80 sub-issues, summing to roughly 6–9 months for a 2–3 person team
to reach v1.0.

If you want to follow along or contribute, watch this repository.

## Tech stack

| Layer | Choice | Why |
|---|---|---|
| Backend | Rust (Axum 0.8 + Polars 0.53 + sqlx + PostgreSQL 18) | release-build size, performance, type safety |
| MCP | `rmcp` 1.x (Streamable HTTP + stdio), aligned with MCP spec 2025-11-25 | official Rust MCP SDK |
| Frontend | SvelteKit 2 + Svelte 5 (Runes) + Tailwind 4 + shadcn-svelte 1 | small bundles, fast SSR |
| ETL | tokio-cron-scheduler + Polars | streaming, schema-diff aware |
| Object storage | SeaweedFS or Garage (S3-compatible) | MinIO archived 2026-04 |
| Auth | Email + OAuth (GitHub / Google) + OAuth 2.1 + DCR | dual-track, switchable to personal mode |
| Deploy | Docker Compose with profiles (`default` / `full` / `obs` / `dev`) | run on any VPS |

Full version-pinned dependency table: [`docs/DESIGN.md` §5](docs/DESIGN.md).

## Roadmap

| Milestone | Scope | Status |
|---|---|---|
| **M0** Foundations | Repo, CI, Docker, healthchecks | complete |
| **M1** MCP MVP | 5 base MCP tools + data.gov.tw ingest | in progress |
| **M2** Marketplace UI | 20 domains, dataset detail, collections | not started |
| **M3** Rich MCP + Utility Wave 1 | Rich tools + 20 TW utility tools + hot cache | not started |
| **M4** Auth + Personal/Multi-user Mode | Email + OAuth + mode switch | not started |
| **M5a / M5b** Community + Multi-source ETL | UGC + TWSE/MOEA/CWA/Fishery | not started |
| **M6** Connectors + Playground + Utility Wave 2 | 8 connectors + 5 playgrounds + 33 utility tools | not started |
| **M7** Discovery + REST + i18n | `/llms.txt`, `/.well-known/*`, OpenAPI, 5 languages | not started |

**MVP (v0.1)** = M0 + M1 + M2, estimated 6–8 weeks for 2 people.

## MCP Quickstart

Taiwan Data Hub ships two MCP transports off the same dispatcher:

- **stdio** (`mcp-stdio` binary) — the universal mode; works with every
  MCP client today. The client spawns the binary and talks JSON-RPC over
  stdin/stdout.
- **Streamable HTTP** (`gateway` binary at `/mcp`) — the new MCP 2025-11-25
  transport. Supported by recent versions of Claude Desktop, Cursor,
  Cline, and the official Inspector. Use this when you want one shared
  server process for several clients.

### 1. Build the binaries

```bash
git clone https://github.com/hydai/taiwan-data-hub.git
cd taiwan-data-hub
cargo build --release --locked -p mcp-stdio -p gateway
# Resulting binaries:
#   target/release/mcp-stdio
#   target/release/gateway
```

`--locked` matches CI and avoids silently updating `Cargo.lock` to
the latest semver-compatible versions. Drop it only if you're
intentionally bumping a dependency.

Use the absolute path from the snippets below — most MCP clients won't
resolve `~` or `$HOME` inside the config JSON.

### 2. Verify with the MCP Inspector (optional)

If you'll use the **Streamable HTTP** transport — for the Inspector
HTTP check below, *or* for any client wired with the `url:` config
in step 3 — start the gateway in a separate terminal and keep it
running. **Stdio** clients spawn `mcp-stdio` themselves, so you can
skip this step entirely if every client uses the `command:` config.

```bash
# macOS / Linux (bash / zsh)
GATEWAY_ADDR=127.0.0.1:8080 ./target/release/gateway
```

```powershell
# Windows (PowerShell)
$env:GATEWAY_ADDR = "127.0.0.1:8080"
.\target\release\gateway.exe
```

```cmd
:: Windows (CMD)
set GATEWAY_ADDR=127.0.0.1:8080
.\target\release\gateway.exe
```

Then run the Inspector. The `--cli` flag runs it headlessly and
prints a single JSON-RPC result — handy for verification and CI:

```bash
# stdio
npx -y @modelcontextprotocol/inspector --cli \
  /ABSOLUTE/PATH/TO/target/release/mcp-stdio \
  --method tools/list

# Streamable HTTP
npx -y @modelcontextprotocol/inspector --cli \
  http://127.0.0.1:8080/mcp --transport http \
  --method tools/list
```

Both should return the current tool list (one entry today:
`list_domains`).

For interactive exploration (browser UI, call any tool, see logs),
drop `--cli`:

```bash
# stdio — Inspector spawns mcp-stdio for you
npx @modelcontextprotocol/inspector cargo run --release -p mcp-stdio

# Streamable HTTP — point Inspector at the running gateway
npx @modelcontextprotocol/inspector http://127.0.0.1:8080/mcp
```

The interactive form opens `http://localhost:6274` and is the form
referenced in [`docs/DESIGN.md`](docs/DESIGN.md) §11.1 *端到端驗收*.

### 3. Wire your client

Pick the snippet for your client and replace `/ABSOLUTE/PATH/TO/...`
with the actual path from step 1.

#### Claude Desktop

Config file:

- **macOS:** `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Windows:** `%APPDATA%\Claude\claude_desktop_config.json`

stdio (macOS / Linux):

```json
{
  "mcpServers": {
    "taiwan-data-hub": {
      "command": "/ABSOLUTE/PATH/TO/target/release/mcp-stdio"
    }
  }
}
```

stdio (Windows — JSON requires backslashes to be doubled):

```json
{
  "mcpServers": {
    "taiwan-data-hub": {
      "command": "C:\\ABSOLUTE\\PATH\\TO\\target\\release\\mcp-stdio.exe"
    }
  }
}
```

Streamable HTTP (Claude Desktop 1.x+ with HTTP-transport support):

```json
{
  "mcpServers": {
    "taiwan-data-hub": {
      "url": "http://127.0.0.1:8080/mcp"
    }
  }
}
```

Restart Claude Desktop and look for the 🛠️ tools indicator on the
chat input.

#### Cursor

Current Cursor builds keep MCP servers in a dedicated file:

- **Global:** `~/.cursor/mcp.json`
- **Per-project:** `<project>/.cursor/mcp.json`

The *Settings → MCP* panel in the UI writes the same file. Older
Cursor versions embedded MCP servers in the VS Code-style
`settings.json` — if you're on a pre-1.0 build, look for an `mcp`
block there instead and migrate it to `mcp.json`.

stdio:

```json
{
  "mcpServers": {
    "taiwan-data-hub": {
      "command": "/ABSOLUTE/PATH/TO/target/release/mcp-stdio"
    }
  }
}
```

Streamable HTTP:

```json
{
  "mcpServers": {
    "taiwan-data-hub": {
      "url": "http://127.0.0.1:8080/mcp"
    }
  }
}
```

Reload the Cursor window after saving.

#### Cline (VS Code extension)

Cline stores MCP servers in `cline_mcp_settings.json` under the VS
Code extension's globalStorage — *not* a generic `mcp.json`. Open
the Cline panel → ☰ menu → *MCP Servers* → *Edit* to have Cline open
the correct file for your OS.

The on-disk path on macOS is:

```text
~/Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json
```

On Linux replace `~/Library/Application Support` with
`~/.config`; on Windows it lives under `%APPDATA%\Code\User\globalStorage\...`.

stdio:

```json
{
  "mcpServers": {
    "taiwan-data-hub": {
      "command": "/ABSOLUTE/PATH/TO/target/release/mcp-stdio",
      "disabled": false,
      "autoApprove": []
    }
  }
}
```

Streamable HTTP:

```json
{
  "mcpServers": {
    "taiwan-data-hub": {
      "url": "http://127.0.0.1:8080/mcp",
      "disabled": false,
      "autoApprove": []
    }
  }
}
```

### Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Client shows 0 tools | Path uses `~` or `$HOME` | Replace with the absolute path |
| `Failed to spawn process` | Binary not built | Re-run `cargo build --release --locked -p mcp-stdio` |
| HTTP config times out | Gateway not running, or wrong port | Start the gateway (see step 2) |
| `Bad Request: missing Host header` | Reverse proxy or raw-socket client stripped `Host:`. Browsers and modern HTTP clients (`curl`, `httpie`, `reqwest`, fetch) always set it automatically. | Send the header explicitly — `curl -H "Host: 127.0.0.1:8080" ...` — or fix the proxy config |
| Server reports `name: "rmcp"` | Old build before the identity fix | `cargo build --release --locked` against `main` |

### Screenshots

Working-integration screenshots and a recorded demo gif are tracked
as a manual follow-up to this PR; they require running the proprietary
Claude Desktop / Cursor / Cline clients which can't be reproduced from
CI. Contributions welcome — see [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

Apache License, Version 2.0. See [`LICENSE`](LICENSE).
