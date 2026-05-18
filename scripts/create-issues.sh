#!/usr/bin/env bash
#
# Batch-create the 80 sub-issues defined in docs/DESIGN.md §9.
# Idempotent-ish: GitHub does NOT de-dup by title, so re-running creates duplicates.
# Run only ONCE on an empty repo.
#
# Usage:
#   REPO=hydai/taiwan-data-hub bash scripts/create-issues.sh
#
set -euo pipefail

REPO="${REPO:-hydai/taiwan-data-hub}"

# create_issue MILESTONE_NUM "TITLE" "BODY" LABEL1 LABEL2 ...
create_issue() {
  local milestone="$1"; shift
  local title="$1"; shift
  local body="$1"; shift
  local label_args=()
  for l in "$@"; do
    label_args+=( --label "$l" )
  done
  gh issue create -R "$REPO" \
    --title "$title" \
    --body "$body" \
    --milestone "$milestone" \
    "${label_args[@]}" \
    >/dev/null
  echo "  ✓ $title"
}

mkbody() {
  local id="$1"; local milestone="$2"; local dod="$3"; local est="$4"
  printf 'From [docs/DESIGN.md §9 — %s](https://github.com/%s/blob/main/docs/DESIGN.md#9-sub-issues-%%E6%%8B%%86%%E5%%88%%86%%E5%%85%%A8%%E9%%83%%A8-8-%%E5%%80%%8B-milestones).\n\n**Issue ID**: %s\n**Milestone**: %s\n**Estimate**: %s\n\n## Definition of Done\n\n%s\n' \
    "$milestone" "$REPO" "$id" "$milestone" "$est" "$dod"
}

# =============================================================================
# M0 — Foundations (milestone #1)
# =============================================================================
echo "📦 M0 — Foundations"

create_issue "M0 — Foundations" \
  "[#0.1] Bootstrap Cargo workspace with all crates" \
  "$(mkbody 0.1 'M0 — Foundations' 'Create Cargo workspace root with member crates: \`gateway\`, \`mcp-stdio\`, \`etl-worker\`, \`mcp-core\`, \`tools-utility\`, \`tools-data\`, \`connectors\`, \`storage\`, \`auth\`, \`shared\`, \`test-support\`. \`cargo build --release\` passes on all platforms. \`[profile.dev.package.\"*\"] debug = false\` set in Cargo.toml.' 'S')" \
  backend infra est:s

create_issue "M0 — Foundations" \
  "[#0.2] Scaffold SvelteKit 2 + Svelte 5 + Tailwind 4 + shadcn-svelte 1" \
  "$(mkbody 0.2 'M0 — Foundations' 'Scaffold \`web/\` with SvelteKit 2.59 + Svelte 5.55 (Runes) + Tailwind 4 (CSS-first via @theme) + shadcn-svelte 1.x + bits-ui 2.x. \`pnpm dev\` starts on :3000, placeholder home page renders. Use \`@tailwindcss/postcss\` plugin (not the legacy one).' 'S')" \
  frontend est:s

create_issue "M0 — Foundations" \
  "[#0.3] Write docker/compose.yaml with PostgreSQL 18 + healthchecks" \
  "$(mkbody 0.3 'M0 — Foundations' 'docker/compose.yaml runs PostgreSQL 18 + named volume (\`pgdata\`) + healthcheck. Also stub services for \`gateway\`, \`web\`, \`etl-worker\` with build contexts. Three Compose files: \`compose.yaml\` (default), \`compose.dev.yaml\` (bind mounts + cargo watch), \`compose.obs.yaml\` (otel + prometheus profiles). \`docker compose up -d\` exits with all services healthy.' 'M')" \
  infra est:m

create_issue "M0 — Foundations" \
  "[#0.4] Add /healthz (liveness) and /readyz (readiness) endpoints" \
  "$(mkbody 0.4 'M0 — Foundations' 'Gateway returns 200 on /healthz unconditionally; /readyz returns 200 only when PostgreSQL pool is reachable and migration version matches. Both return JSON \`{status, build_sha, version}\`. Compose healthcheck wired to /readyz.' 'XS')" \
  backend est:xs

create_issue "M0 — Foundations" \
  "[#0.5] Set up GitHub Actions CI (fmt / clippy / cargo test / svelte-check / prettier / lighthouse)" \
  "$(mkbody 0.5 'M0 — Foundations' 'PRs trigger: \`cargo fmt --check\`, \`cargo clippy -- -D warnings\`, \`cargo test --release\`, \`pnpm svelte-check\`, \`pnpm prettier --check\`, \`pnpm lint\`, and (after M2) \`lighthouse-ci\`. All jobs green on this initial commit. Use \`cargo-chef\` for build caching, Node 22 LTS.' 'M')" \
  infra est:m

create_issue "M0 — Foundations" \
  "[#0.6] Add CONTRIBUTING.md and CODE_OF_CONDUCT.md" \
  "$(mkbody 0.6 'M0 — Foundations' 'CONTRIBUTING.md covers: DCO sign-off, conventional commits, PR review flow, dev environment setup. CODE_OF_CONDUCT.md uses Contributor Covenant 2.1. LICENSE already in place (Apache-2.0).' 'S')" \
  docs est:s

create_issue "M0 — Foundations" \
  "[#0.7] Add issue templates (bug / feature / dataset-request) + PR template" \
  "$(mkbody 0.7 'M0 — Foundations' 'Three issue templates in .github/ISSUE_TEMPLATE/: bug_report.yml, feature_request.yml, dataset_request.yml. PR template with checklist (tests, docs, lineguard, milestone, breaking changes).' 'XS')" \
  docs infra est:xs

create_issue "M0 — Foundations" \
  "[#0.8] Initial sqlx migration: domains, datasets, dataset_versions, dataset_files" \
  "$(mkbody 0.8 'M0 — Foundations' 'sqlx-cli migration 0001_init.sql creates: \`domains\` (id, slug, kind, name_i18n jsonb, …), \`datasets\` (id uuid pk default uuidv7(), source, source_id, slug, domain_id, title_i18n, tier, license, …), \`dataset_versions\`, \`dataset_files\`. PG 18 native uuidv7() used. \`sqlx migrate run\` succeeds. Seed file populates 20 domains from \`config/domains.yaml\`.' 'S')" \
  backend est:s

# =============================================================================
# M1 — MCP MVP (milestone #2)
# =============================================================================
echo "📦 M1 — MCP MVP"

create_issue "M1 — MCP MVP" \
  "[#1.1] Implement rmcp 1.x-based MCP server skeleton with stdio transport" \
  "$(mkbody 1.1 'M1 — MCP MVP' 'mcp-stdio binary uses rmcp 1.7+ with stdio transport. \`mcp inspector\` connects, lists 0 tools (skeleton only), passes initialize handshake. Wired to mcp-core dispatcher trait so adding tools in later issues is pure registration.' 'M')" \
  mcp backend est:m

create_issue "M1 — MCP MVP" \
  "[#1.2] Add HTTP/SSE transport for MCP (POST /mcp + GET /mcp SSE upgrade)" \
  "$(mkbody 1.2 'M1 — MCP MVP' 'Gateway exposes POST /mcp (Streamable HTTP per MCP 2025-11-25 spec) and GET /mcp (SSE upgrade, backward-compat). Same dispatcher as stdio. \`mcp inspector\` via SSE URL connects. CORS configured per spec.' 'M')" \
  mcp backend est:m

create_issue "M1 — MCP MVP" \
  "[#1.3] Implement list_domains tool (seeded from config/domains.yaml)" \
  "$(mkbody 1.3 'M1 — MCP MVP' 'Tool returns 20 domains with i18n names + dataset_count. Input: \`{locale?: \"zh-TW\"|\"en\"|...}\`. Output validated against schema. mcp-core registry pattern reused for next 4 tools.' 'S')" \
  mcp est:s

create_issue "M1 — MCP MVP" \
  "[#1.4] Build data.gov.tw catalog crawler (metadata only) in connectors/data-gov-tw" \
  "$(mkbody 1.4 'M1 — MCP MVP' 'connectors crate provides \`SourceConnector\` trait + data_gov_tw impl. Crawler hits CKAN-style API, respects ETag/If-Modified-Since, inserts/upserts ~52,960 dataset metadata. Cron-scheduled (nightly 02:00 Asia/Taipei). Schema diff detection stores into dataset_versions. Initial seed runs in < 30 min on a laptop.' 'L')" \
  etl backend est:l

create_issue "M1 — MCP MVP" \
  "[#1.5] Implement search_datasets with PG FTS (tsvector + GIN + zhparser)" \
  "$(mkbody 1.5 'M1 — MCP MVP' 'Tool searches datasets by q (中文/英文)、domain、tier、license、locale. PostgreSQL FTS via zhparser extension. Returns paginated results (limit ≤ 100). Generated tsv column auto-maintained by trigger. Performance: < 100 ms for 50k row corpus.' 'M')" \
  mcp backend est:m

create_issue "M1 — MCP MVP" \
  "[#1.6] Implement get_dataset tool (full metadata + schema + version history)" \
  "$(mkbody 1.6 'M1 — MCP MVP' 'Tool fetches by id (uuid) or slug. Returns full metadata, current schema, all dataset_versions, dataset_files (uri+format+size). Locale-aware i18n fields. Returns proper MCP error if not found.' 'S')" \
  mcp est:s

create_issue "M1 — MCP MVP" \
  "[#1.7] Implement query_rows tool with Polars + sqlparser AST whitelist" \
  "$(mkbody 1.7 'M1 — MCP MVP' 'Tool accepts user SQL string + dataset_id. sqlparser-rs parses → AST → whitelist (SELECT only, no system funcs, forced LIMIT ≤ 10000). Polars LazyFrame scans Parquet (cached) or returns 422 with hint to call materialize_dataset. tokio timeout 5s, memory limit enforced. Comprehensive injection test suite.' 'L')" \
  mcp backend security est:l

create_issue "M1 — MCP MVP" \
  "[#1.8] Implement materialize_dataset tool (signed URL download)" \
  "$(mkbody 1.8 'M1 — MCP MVP' 'Tool produces a presigned URL for CSV/Parquet/JSON export of a dataset. TTL configurable (default 1h). Backed by SeaweedFS S3 API. Records usage in usage_records. Concurrent material requests for same dataset deduplicated.' 'M')" \
  mcp est:m

create_issue "M1 — MCP MVP" \
  "[#1.9] Write MCP integration tests using rmcp test harness + Inspector CI" \
  "$(mkbody 1.9 'M1 — MCP MVP' 'Integration tests cover all 5 tools (happy path + 2 error cases each). CI workflow runs \`npx @modelcontextprotocol/inspector\` weekly cron + on PR against test fixture. Contract test catches rmcp upgrades that break compatibility.' 'M')" \
  mcp backend est:m

create_issue "M1 — MCP MVP" \
  "[#1.10] Document MCP setup for Claude Desktop / Cursor / Cline" \
  "$(mkbody 1.10 'M1 — MCP MVP' 'README adds an MCP Quickstart section with three config snippets (Claude Desktop claude_desktop_config.json, Cursor settings.json, Cline mcp.json). Each snippet copy-pasteable. Screenshots/animated gif of working integration.' 'S')" \
  docs mcp est:s

# =============================================================================
# M2 — Marketplace UI (milestone #3)
# =============================================================================
echo "📦 M2 — Marketplace UI"

create_issue "M2 — Marketplace UI" \
  "[#2.1] Design tokens + Tailwind 4 theme (colors, spacing, typography)" \
  "$(mkbody 2.1 'M2 — Marketplace UI' 'Define design tokens via Tailwind 4 @theme block in app.css. Colors (primary/neutral/semantic), spacing scale, typography (Noto Sans TC for zh, Inter for en). Tokens documented in docs/design-tokens.md.' 'S')" \
  frontend est:s

create_issue "M2 — Marketplace UI" \
  "[#2.2] Build shell layout (header / footer / sidebar / mobile nav)" \
  "$(mkbody 2.2 'M2 — Marketplace UI' 'web/src/routes/+layout.svelte renders responsive shell: top header (logo + nav links + locale switcher + auth status), footer (license/repo/contact), mobile burger menu. Three breakpoints (sm/md/lg) verified.' 'M')" \
  frontend est:m

create_issue "M2 — Marketplace UI" \
  "[#2.3] REST endpoints /api/v1/{domains,datasets,collections} + OpenAPI annotation" \
  "$(mkbody 2.3 'M2 — Marketplace UI' 'Gateway exposes paginated REST endpoints with cursor/offset support, locale query param, filter params (domain/tier/license/format). All handlers annotated with utoipa-axum 0.2 so OpenAPI 3.1 spec generates. Swagger UI at /api/docs.' 'M')" \
  backend est:m

create_issue "M2 — Marketplace UI" \
  "[#2.4] /domains index page with 20 domain cards (SSR + cache headers)" \
  "$(mkbody 2.4 'M2 — Marketplace UI' '/domains route shows 20 domain cards (icon + name + count + scope). SSR with stale-while-revalidate cache header (5 min). Topical / Meta / Horizontal section dividers.' 'S')" \
  frontend est:s

create_issue "M2 — Marketplace UI" \
  "[#2.5] /domains/[slug] page with dataset list + filters (tier/format/license)" \
  "$(mkbody 2.5 'M2 — Marketplace UI' 'Domain detail shows scope, typical questions, anchor datasets, then paginated dataset list. URL-driven filters (tier, format, license) restore on reload. Empty state and loading skeleton.' 'M')" \
  frontend est:m

create_issue "M2 — Marketplace UI" \
  "[#2.6] /datasets/[id] detail page with resources + Copy MCP config button" \
  "$(mkbody 2.6 'M2 — Marketplace UI' 'Dataset detail: title/publisher/tier/license/update_freq, schema (column list), version history, resource files. Big \"Copy MCP config\" button that copies a ready-to-paste config snippet for Claude Desktop/Cursor/Cline.' 'M')" \
  frontend est:m

create_issue "M2 — Marketplace UI" \
  "[#2.7] /collections curated collections page (YAML-driven)" \
  "$(mkbody 2.7 'M2 — Marketplace UI' '/collections lists curated packs (education/healthcare/tourism + extensible). Source = config/collections.yaml committed to repo. Each collection shows curator note + 6 anchor datasets.' 'S')" \
  frontend backend est:s

create_issue "M2 — Marketplace UI" \
  "[#2.8] Implement tier classification logic (popularity + authority + format)" \
  "$(mkbody 2.8 'M2 — Marketplace UI' 'Nightly job computes score = 0.4·update_freq + 0.3·publisher_authority + 0.2·format_quality + 0.1·access_count_norm. Thresholds platinum>0.85, gold>0.7, silver>0.5. \`datasets.tier_override\` honored if set. Config in config/tiers.toml.' 'M')" \
  backend est:m

create_issue "M2 — Marketplace UI" \
  "[#2.9] SEO: sitemap.xml / robots.txt / OG tags per dataset" \
  "$(mkbody 2.9 'M2 — Marketplace UI' 'sitemap.xml regenerates daily (split into chunks if > 50k URLs). robots.txt allows known bot UAs incl. ClaudeBot, blocks /admin /dashboard /account. Every dataset detail page emits OG + Twitter Card meta tags. Validated against Google Rich Result test.' 'S')" \
  frontend est:s

create_issue "M2 — Marketplace UI" \
  "[#2.10] Lighthouse CI gate (perf > 85, a11y > 95)" \
  "$(mkbody 2.10 'M2 — Marketplace UI' 'CI job runs lighthouse-ci against / and /datasets/[id]. Budgets: performance ≥ 85, accessibility ≥ 95, best-practices ≥ 90, SEO ≥ 90. PR-blocking.' 'S')" \
  infra frontend est:s

# =============================================================================
# M3 — Rich MCP + Utility Wave 1 (milestone #4)
# =============================================================================
echo "📦 M3 — Rich MCP + Utility Wave 1"

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.1] Add Polars engine integration in mcp-core (LazyFrame helper API)" \
  "$(mkbody 3.1 'M3 — Rich MCP + Utility Wave 1' 'mcp-core exposes a \`DatasetEngine\` that loads Parquet/CSV/JSON via Polars LazyFrame, applies projection/predicates, enforces row/memory limits. All rich tools build on this.' 'M')" \
  backend est:m

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.2] Implement describe_schema rich tool" \
  "$(mkbody 3.2 'M3 — Rich MCP + Utility Wave 1' 'Tool returns column list with dtype, nullability, sample values (first 5 non-null), distinct cardinality (approx via HyperLogLog), and PG-derived business descriptions if available.' 'M')" \
  mcp est:m

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.3] Implement get_sample rich tool with sampling strategies" \
  "$(mkbody 3.3 'M3 — Rich MCP + Utility Wave 1' 'Tool returns N rows via \`strategy: head|random|stratified\` (stratified needs a stratify_col). Default n=10, max=1000. Uses Polars sampling with reservoir for random.' 'M')" \
  mcp est:m

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.4] Implement join_datasets rich tool (Polars inner/left join)" \
  "$(mkbody 3.4 'M3 — Rich MCP + Utility Wave 1' 'Tool joins two datasets on a key (single-column or multi-column). Supports inner/left/right/outer. Returns row count + paginated rows. Pre-flight check: estimate join size, refuse if > 1M rows without explicit override.' 'L')" \
  mcp backend est:l

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.5] Implement aggregate_dataset rich tool" \
  "$(mkbody 3.5 'M3 — Rich MCP + Utility Wave 1' 'Tool: group_by [cols] + agg [{col, fn: sum|mean|count|median|min|max|stddev}]. Returns aggregated table. Limit on group cardinality (refuse > 100k groups).' 'M')" \
  mcp est:m

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.6] Build hot-dataset cache pipeline (top-N preheat to SeaweedFS Parquet)" \
  "$(mkbody 3.6 'M3 — Rich MCP + Utility Wave 1' 'etl-worker promotes platinum/gold tier datasets and any dataset with > 50 query_rows hits/7d into Parquet cache on SeaweedFS. Demotes after 30 days inactivity. Telemetry: cache hit ratio metric.' 'L')" \
  etl infra est:l

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.7] Utility: Taiwan address normalizer (5-part split)" \
  "$(mkbody 3.7 'M3 — Rich MCP + Utility Wave 1' 'tools-utility crate adds \`normalize_address\` taking a free-form Taiwan address and returning {county, district, road, section, lane, alley, number, floor}. 100+ test cases including 改制後縣市 (e.g. 台中縣→台中市). Exposed via MCP tool \`tw_normalize_address\`.' 'M')" \
  backend est:m

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.8] Utility: ROC year + lunar + 24 solar terms + national holidays" \
  "$(mkbody 3.8 'M3 — Rich MCP + Utility Wave 1' 'Functions: roc_to_gregorian, gregorian_to_roc, gregorian_to_lunar, solar_term_for_date, is_national_holiday. Data source: 內政部公開行事曆. Each exposed as MCP tool.' 'M')" \
  backend est:m

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.9] Utility: TW ID / Tax ID / Residence permit / Passport validation" \
  "$(mkbody 3.9 'M3 — Rich MCP + Utility Wave 1' 'Validate 身分證 (incl. new format), 統一編號 (with new 2023 checksum rule), 居留證 (ARC), 中華民國護照. Each returns {valid: bool, kind, parsed: {…}}. Comprehensive test vectors.' 'S')" \
  backend est:s

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.10] Utility: City / district canonicalizer (handles post-改制 names)" \
  "$(mkbody 3.10 'M3 — Rich MCP + Utility Wave 1' 'Canonicalize any free-form 縣市 / 鄉鎮市區 string to a stable code (e.g. ROC_CITY_NEW_TAIPEI + DIST_BANQIAO). Handles 台中縣→台中市, 台南縣→台南市, 高雄縣→高雄市, 桃園縣→桃園市 historical mappings.' 'S')" \
  backend est:s

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.11] Utility: admin codes / MRT stations / bank codes / postal codes" \
  "$(mkbody 3.11 'M3 — Rich MCP + Utility Wave 1' 'Five lookup tables shipped as data crates: 行政區代碼 (TGOS), MRT stations (TRTC/KRTC/TYM/TMRT), 銀行代碼 (CBC), 郵遞區號 (3 or 5 digit), ROC 縣市代碼. Each exposes a get_by_id and a search MCP tool.' 'M')" \
  backend est:m

create_issue "M3 — Rich MCP + Utility Wave 1" \
  "[#3.12] Utility wave 1 batch: invoice / postal / Taipower meter / 8 more" \
  "$(mkbody 3.12 'M3 — Rich MCP + Utility Wave 1' 'Wave-1 remainder: 統一發票號碼驗證, 郵遞區號搜尋, 台電電號驗證, 自來水水號, 中華電信市話/手機格式, 車牌格式 (4 字 / 6 字), 信用卡 LUHN, IBAN, IATA airport codes. Each independently testable, each an MCP tool. Bulk PR or separate PRs at contributor preference.' 'L')" \
  backend est:l

# =============================================================================
# M4 — Auth + Personal/Multi-user Mode (milestone #5)
# =============================================================================
echo "📦 M4 — Auth + Personal/Multi-user Mode"

create_issue "M4 — Auth + Personal/Multi-user Mode" \
  "[#4.1] Add MODE env var with personal default + startup log" \
  "$(mkbody 4.1 'M4 — Auth + Personal/Multi-user Mode' 'Gateway reads MODE=personal|multi-user from env (default personal). Boot log prints mode + which endpoints are public vs gated. A \`taiwan-data-hub doctor\` CLI subcommand validates config consistency.' 'XS')" \
  backend infra est:xs

create_issue "M4 — Auth + Personal/Multi-user Mode" \
  "[#4.2] Email + password registration / login + magic link recovery (SMTP)" \
  "$(mkbody 4.2 'M4 — Auth + Personal/Multi-user Mode' 'auth crate implements email/password (argon2id), email verification, password reset via magic link. SMTP creds from env (provider-agnostic: works with Resend/Postmark/raw SMTP). Rate-limited to prevent enumeration.' 'M')" \
  backend security est:m

create_issue "M4 — Auth + Personal/Multi-user Mode" \
  "[#4.3] GitHub OAuth flow (callback + state CSRF + PKCE)" \
  "$(mkbody 4.3 'M4 — Auth + Personal/Multi-user Mode' 'OAuth 2.1 with PKCE S256. Client ID/secret from env. Auto-creates user on first login; links to existing user if email matches. Token stored AES-GCM-encrypted in oauth_accounts table. CSRF state validated.' 'M')" \
  backend security est:m

create_issue "M4 — Auth + Personal/Multi-user Mode" \
  "[#4.4] Google OAuth flow (same shape as GitHub)" \
  "$(mkbody 4.4 'M4 — Auth + Personal/Multi-user Mode' 'Identical pattern to #4.3 but for Google OpenID Connect. Verifies id_token signature, extracts email/name/avatar. Refresh token rotation.' 'M')" \
  backend security est:m

create_issue "M4 — Auth + Personal/Multi-user Mode" \
  "[#4.5] Session middleware (JWT in httpOnly cookie + rotation + refresh)" \
  "$(mkbody 4.5 'M4 — Auth + Personal/Multi-user Mode' 'Sessions stored in DB; signed cookie carries opaque session id (not JWT bearer). Cookie attrs: httpOnly, Secure, SameSite=Lax. Sliding window refresh on each request (max 14d total). Logout invalidates server-side.' 'M')" \
  backend security est:m

create_issue "M4 — Auth + Personal/Multi-user Mode" \
  "[#4.6] API key management UI + endpoints (create / revoke / list / rotate)" \
  "$(mkbody 4.6 'M4 — Auth + Personal/Multi-user Mode' 'Account page lists user API keys with last_used_at, scopes, rate-limit tier. Keys shown ONCE on creation (key_prefix stored, full key hashed). Revoke is immediate.' 'M')" \
  frontend backend security est:m

create_issue "M4 — Auth + Personal/Multi-user Mode" \
  "[#4.7] Rate limit middleware (per-IP + per-user, DragonflyDB-backed, 429 + Retry-After)" \
  "$(mkbody 4.7 'M4 — Auth + Personal/Multi-user Mode' 'tower-governor middleware: three layers (IP 60/min, API key per tier, tool-specific query_rows stricter). 429 response includes Retry-After + X-RateLimit-* headers. Backed by DragonflyDB; fallback to PG advisory locks for small deploys.' 'M')" \
  backend security est:m

create_issue "M4 — Auth + Personal/Multi-user Mode" \
  "[#4.8] Auth conditional rendering on frontend (personal mode hides login)" \
  "$(mkbody 4.8 'M4 — Auth + Personal/Multi-user Mode' 'SvelteKit layout fetches /api/v1/me + /api/v1/config; if mode=personal, hides login UI and renders \"personal mode\" badge. If multi-user, shows full login/signup. Cached at SSR.' 'S')" \
  frontend est:s

# =============================================================================
# M5a — Community Features (milestone #6)
# =============================================================================
echo "📦 M5a — Community Features"

create_issue "M5a — Community Features" \
  "[#5a.1] Submission form: dataset / tool / connector / playground (4 types + validation)" \
  "$(mkbody 5a.1 'M5a — Community Features' 'Authenticated users submit one of four types via a multi-step form. Each type has its own JSON Schema enforced both client- (garde on backend) and server-side. Saves as submissions row with status=pending.' 'L')" \
  frontend backend community est:l

create_issue "M5a — Community Features" \
  "[#5a.2] Moderation queue + role-based access (moderator/curator/admin)" \
  "$(mkbody 5a.2 'M5a — Community Features' 'New roles in users.role enum. /admin/moderation lists pending submissions with diff vs current data, approve/reject with reason. Approved submission promoted into datasets/tools/etc. tables. Audit log of all decisions.' 'M')" \
  backend community est:m

create_issue "M5a — Community Features" \
  "[#5a.3] Comments + replies on dataset page (depth 2, markdown, sanitize)" \
  "$(mkbody 5a.3 'M5a — Community Features' 'Threaded comments under datasets (also tools/connectors). Markdown rendering sanitized (ammonia crate). Reply nesting capped at 2 levels. Edit window 5 min after post. Soft-delete with tombstone.' 'M')" \
  frontend backend community est:m

create_issue "M5a — Community Features" \
  "[#5a.4] Bookmarks / favorites (private collections per user)" \
  "$(mkbody 5a.4 'M5a — Community Features' 'Heart button on dataset / tool / connector / playground cards. /account/bookmarks lists by type, supports custom user-defined collections (private). Public collections feature deferred.' 'M')" \
  frontend backend community est:m

create_issue "M5a — Community Features" \
  "[#5a.5] 5-star ratings with anti-spam (1 rating per user per dataset)" \
  "$(mkbody 5a.5 'M5a — Community Features' 'Upsert rating (user_id, dataset_id, score 1-5). Aggregate avg + count cached in datasets table, refreshed nightly + on write. Anti-spam: minimum account age 24h before first rating.' 'S')" \
  backend community est:s

create_issue "M5a — Community Features" \
  "[#5a.6] Report / flag content + moderator dashboard" \
  "$(mkbody 5a.6 'M5a — Community Features' 'Report button on comments and submissions with reason categories. Reports queue visible to moderators. Auto-hide content after N independent reports. Reporter feedback when action taken.' 'M')" \
  community est:m

# =============================================================================
# M5b — Multi-source ETL (milestone #7)
# =============================================================================
echo "📦 M5b — Multi-source ETL"

create_issue "M5b — Multi-source ETL" \
  "[#5b.1] ETL framework: SourceConnector trait + scheduler + retry + DLQ" \
  "$(mkbody 5b.1 'M5b — Multi-source ETL' 'Generalize the existing data.gov.tw crawler into a \`SourceConnector\` trait with methods (list_datasets, fetch_metadata, fetch_data, supports_incremental). tokio-cron-scheduler runs all connectors per config/sources.toml. Failed jobs go to a dead-letter table; retried with exponential backoff.' 'L')" \
  etl backend est:l

create_issue "M5b — Multi-source ETL" \
  "[#5b.2] TWSE connector (listed company daily trades + monthly revenue)" \
  "$(mkbody 5b.2 'M5b — Multi-source ETL' 'Connector for Taiwan Stock Exchange MOPS open data: 上市公司日成交資訊 + 月營收 + 重大訊息. Respects robots.txt and per-page throttle. Persists as dataset with source=twse.' 'L')" \
  etl est:l

create_issue "M5b — Multi-source ETL" \
  "[#5b.3] MOEA Business Registry connector (公司登記 API: full + incremental)" \
  "$(mkbody 5b.3 'M5b — Multi-source ETL' '經濟部商工登記公示資料. Initial full sync (~2M rows) then incremental via 更新日期 query. Cross-link to data.gov.tw 統一編號 datasets via 統編 key.' 'L')" \
  etl est:l

create_issue "M5b — Multi-source ETL" \
  "[#5b.4] CWA (中央氣象署) connector (observations + forecasts, needs API key)" \
  "$(mkbody 5b.4 'M5b — Multi-source ETL' 'Observation stations real-time data + 36h township forecasts + typhoon track. CWA_API_KEY env required; surface clear error if missing. Documented signup flow in docs/sources/cwa.md.' 'M')" \
  etl est:m

create_issue "M5b — Multi-source ETL" \
  "[#5b.5] Fishery (MOA) connector (漁產交易行情)" \
  "$(mkbody 5b.5 'M5b — Multi-source ETL' '漁業署 + 農業部開放資料: 漁產品交易行情 + 漁港進出統計. Daily refresh.' 'M')" \
  etl est:m

create_issue "M5b — Multi-source ETL" \
  "[#5b.6] Provenance & licensing metadata per source" \
  "$(mkbody 5b.6 'M5b — Multi-source ETL' 'Every dataset row stamped with source (enum), source_url, license, fetched_at, license_url. Surface on dataset detail page and in MCP get_dataset response. /licenses page enumerates all licenses in use.' 'S')" \
  etl backend est:s

# =============================================================================
# M6 — Connectors + Playground + Utility Wave 2 (milestone #8)
# =============================================================================
echo "📦 M6 — Connectors + Playground + Utility Wave 2"

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.1] /connectors index page + connector schema" \
  "$(mkbody 6.1 'M6 — Connectors + Playground + Utility Wave 2' 'Page shows 8 connector cards: name + logo + description + token requirement badge + install button. Schema in config/connectors.yaml: slug, name_i18n, description_i18n, install_instructions_i18n, homepage_url, mcp_config_template, status.' 'S')" \
  frontend est:s

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.2] Build 8 connector install guides (Playwright/Chrome DevTools/n8n/Notion/Sentry/Google/Sequential Thinking/Context7)" \
  "$(mkbody 6.2 'M6 — Connectors + Playground + Utility Wave 2' '8 install guides, each with Claude Desktop / Cursor / Cline snippet. Parallelizable across contributors (each connector = one PR + good-first-issue candidate).' 'L')" \
  docs community est:l

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.3] Playground framework: iframe sandbox + share link + strict CSP" \
  "$(mkbody 6.3 'M6 — Connectors + Playground + Utility Wave 2' 'Playgrounds run in iframe with sandbox=\"allow-scripts\" + CSP that blocks egress except to gateway. Each playground app source in playgrounds/{slug}/. Share link encodes playground id + state.' 'M')" \
  frontend security est:m

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.4] Playground 1: Company 360 (3-DB join: registry + judicial + procurement)" \
  "$(mkbody 6.4 'M6 — Connectors + Playground + Utility Wave 2' 'Type a 統編 → see company registry + 司法案件 + 政府採購得標紀錄 in one view. Uses join_datasets MCP tool. Per-company sub-route /playground/company/{taxId}.' 'M')" \
  frontend mcp est:m

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.5] Playground 2: Judicial stats room (judicial_legal domain demo)" \
  "$(mkbody 6.5 'M6 — Connectors + Playground + Utility Wave 2' '司法統計即時室. Time-series of case counts by court / case type / year. Uses aggregate_dataset MCP tool.' 'M')" \
  frontend mcp est:m

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.6] Playground 3: Taiwan rural map (geo_basemap + population)" \
  "$(mkbody 6.6 'M6 — Connectors + Playground + Utility Wave 2' 'MapLibre GL v5 map of 鄉鎮市區 with population choropleth. OSM tiles. Toggle layers: 醫療 / 教育 / 交通.' 'M')" \
  frontend est:m

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.7] Playground 4: Housing price heatmap (realestate_land 4.75M rows)" \
  "$(mkbody 6.7 'M6 — Connectors + Playground + Utility Wave 2' '實價登錄熱圖. Pre-aggregated tiles served by gateway. Time-slider 2012→present. Uses query_rows for drill-down.' 'M')" \
  frontend mcp est:m

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.8] Playground 5: Procurement war room (procurement_subsidy time-series)" \
  "$(mkbody 6.8 'M6 — Connectors + Playground + Utility Wave 2' '政府採購戰情室. Top vendors by contract value, anomaly detection (sudden 10x growth), agency comparison.' 'M')" \
  frontend mcp est:m

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.9] Utility wave 2 batch A: 13 generic tools (geo / stats / time series)" \
  "$(mkbody 6.9 'M6 — Connectors + Playground + Utility Wave 2' 'Generic tools: distance_haversine, point_in_polygon, geocode (via OSM/Nominatim), reverse_geocode, summary_statistics, correlation, linear_regression, decompose_seasonal, percentile, histogram, moving_average, autocorrelation, anomaly_isolation_forest. Each MCP-exposed.' 'L')" \
  backend est:l

create_issue "M6 — Connectors + Playground + Utility Wave 2" \
  "[#6.10] Utility wave 2 batch B: 20 misc tools (PDF extract / URL→Markdown / encoders)" \
  "$(mkbody 6.10 'M6 — Connectors + Playground + Utility Wave 2' 'PDF text/table extraction, URL→Markdown (Readability + html2md), encoders (base64/url/hex/jwt-decode), hash (sha256/blake3), HTML sanitizer, JSON path, regex tester, JSON schema validate, UUID v4/v7 gen, ULID gen, slugify, language detect, timezone convert, holiday between dates, Big5/UTF-8 transcode, traditional/simplified Chinese convert.' 'L')" \
  backend est:l

# =============================================================================
# M7 — Agent Discovery + REST + i18n (milestone #9)
# =============================================================================
echo "📦 M7 — Agent Discovery + REST + i18n"

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.1] Generate /llms.txt from dataset catalog (auto-paginated if > 5 MB)" \
  "$(mkbody 7.1 'M7 — Agent Discovery + REST + i18n' '/llms.txt enumerates all datasets in markdown form, agent-readable. If size > 5 MB, split into /llms-index.txt + /llms-page-N.txt with cross-links. Cached at edge, regenerated nightly + on dataset write.' 'S')" \
  backend est:s

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.2] /.well-known/mcp.json manifest (server URL + auth + tool list)" \
  "$(mkbody 7.2 'M7 — Agent Discovery + REST + i18n' 'Conforms to current MCP discovery spec: server URL, auth metadata pointer (OAuth resource), tool catalogue summary, license. Generated from running registry.' 'XS')" \
  backend mcp est:xs

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.3] /.well-known/agent-card.json (Google A2A) + /.well-known/agent-skills.json" \
  "$(mkbody 7.3 'M7 — Agent Discovery + REST + i18n' 'Two static-ish JSON endpoints generated from registry data. agent-card.json per Google A2A schema; agent-skills.json indexes skill names → MCP tool ids.' 'S')" \
  backend est:s

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.4] /.well-known/api-catalog (RFC 9727) + /.well-known/oauth-protected-resource (RFC 9728)" \
  "$(mkbody 7.4 'M7 — Agent Discovery + REST + i18n' 'RFC 9727 api-catalog lists REST + MCP endpoints with descriptions. RFC 9728 oauth-protected-resource exposes authorization server metadata pointer + resource identifier. Both validated against RFC test vectors.' 'S')" \
  backend security est:s

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.5] OpenAPI 3.1 spec via utoipa-axum (/api/docs Swagger UI)" \
  "$(mkbody 7.5 'M7 — Agent Discovery + REST + i18n' 'utoipa 5.5 generates OpenAPI 3.1 spec from handler annotations. Spec served at /api/openapi.json, Swagger UI at /api/docs, ReDoc at /api/redoc. Spec is contract-tested against actual responses via dredd or schemathesis in CI.' 'M')" \
  backend est:m

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.6] Set up i18n framework (Paraglide-JS 2 + locale detection + URL prefix)" \
  "$(mkbody 7.6 'M7 — Agent Discovery + REST + i18n' 'Paraglide-JS 2.x with paraglideMiddleware. URL strategy /zh-TW/, /en/, /ja/, /ko/, /fr/. Locale detection order: URL > cookie > Accept-Language > default zh-TW. \`getLocale()\` / \`setLocale()\` used throughout (not deprecated v1 names).' 'M')" \
  frontend i18n est:m

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.7] Extract zh-TW source strings + en translation pass" \
  "$(mkbody 7.7 'M7 — Agent Discovery + REST + i18n' 'zh-TW is source-of-truth. \`pnpm i18n:extract\` finds all .svelte/.ts strings, populates messages/zh-TW.json. en translation by maintainer or native reviewer.' 'M')" \
  frontend i18n est:m

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.8] ja / ko / fr translations (one independent issue per locale, parallel)" \
  "$(mkbody 7.8 'M7 — Agent Discovery + REST + i18n' 'Three sibling issues — ja, ko, fr — open and tagged \`good first issue\` for community contributors. Each translates messages/{locale}.json to ≥ 95 % coverage. PR template asks for native-speaker review.' 'L')" \
  i18n good\ first\ issue est:l

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.9] Locale-aware DB metadata (title_i18n / description_i18n jsonb fallback)" \
  "$(mkbody 7.9 'M7 — Agent Discovery + REST + i18n' 'All user-facing strings in DB stored as jsonb {locale: text}. Read pattern: COALESCE(title_i18n->>$lang, title_i18n->>\"zh-TW\"). MCP responses honor requested locale param.' 'M')" \
  backend i18n est:m

create_issue "M7 — Agent Discovery + REST + i18n" \
  "[#7.10] i18n CI check (missing key gate + scanner)" \
  "$(mkbody 7.10 'M7 — Agent Discovery + REST + i18n' 'CI job: paraglide compile + missing-key linter. Block PR if any source string lacks a translation in en (zh-TW source language exempt). Untranslated for ja/ko/fr produces warning not error.' 'S')" \
  infra i18n est:s

echo ""
echo "✅ All 80 issues created."
