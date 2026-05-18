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

**Pre-alpha · design phase.** Implementation has not started yet. The full
system design lives in [`docs/DESIGN.md`](docs/DESIGN.md). Eight milestones
(M0–M7) decompose the work into 80 sub-issues, summing to roughly 6–9 months
for a 2–3 person team to reach v1.0.

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
| **M0** Foundations | Repo, CI, Docker, healthchecks | not started |
| **M1** MCP MVP | 5 base MCP tools + data.gov.tw ingest | not started |
| **M2** Marketplace UI | 20 domains, dataset detail, collections | not started |
| **M3** Rich MCP + Utility Wave 1 | Rich tools + 20 TW utility tools + hot cache | not started |
| **M4** Auth + Personal/Multi-user Mode | Email + OAuth + mode switch | not started |
| **M5a / M5b** Community + Multi-source ETL | UGC + TWSE/MOEA/CWA/Fishery | not started |
| **M6** Connectors + Playground + Utility Wave 2 | 8 connectors + 5 playgrounds + 33 utility tools | not started |
| **M7** Discovery + REST + i18n | `/llms.txt`, `/.well-known/*`, OpenAPI, 5 languages | not started |

**MVP (v0.1)** = M0 + M1 + M2, estimated 6–8 weeks for 2 people.

## License

Apache License, Version 2.0. See [`LICENSE`](LICENSE).
