# CWA (中央氣象署) connector

The CWA connector pulls Taiwan's national observation network, county-level
forecasts, and typhoon tracks from the Central Weather Administration's
open-data hub at <https://opendata.cwa.gov.tw>.

## Why this needs an API key

Unlike the data.gov.tw, TWSE, and MOEA upstreams, **every** request to CWA's
open-data API must carry a valid key. Without one the upstream returns HTTP
401 immediately. Keys are free; CWA uses them for rate-limit accounting and
abuse tracking, not paywalling.

## Sign-up flow (one-time, ~3 minutes)

1. Open <https://opendata.cwa.gov.tw/> and click **登入/註冊** (top-right).
2. Choose **會員註冊**. You'll need:
   - An email address (used for the confirmation link).
   - A password.
   - Your name and a country selection — anything reasonable is accepted.
3. Click the confirmation link in the email CWA sends.
4. Log in. The dashboard's left sidebar has **取得授權碼** (Get
   authorisation key). Click it. The page renders a single string starting
   with `CWB-…` or `CWA-…` — that is your API key.
5. Copy the key. It does not expire on a schedule, but CWA will issue a new
   one and invalidate the old one if you click **重新產生**.

## Wiring the key into Taiwan Data Hub

Set the `CWA_API_KEY` environment variable wherever the ETL worker runs.

For a local dev shell:

```bash
export CWA_API_KEY='CWA-XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX'
cargo run --release -p etl-worker
```

For Docker Compose, add it to the compose file's `environment:` block on the
`etl-worker` service (or a `.env` file the compose stack reads — never check
the key into git).

For Kubernetes / Nomad / systemd, deliver it via the cluster's secret
mechanism. The connector treats an empty `CWA_API_KEY` the same as missing,
so an unset secret will fail boot cleanly rather than silently 401-ing every
6 hours when the cron fires.

## What the boot-time check looks like

If `[sources.cwa] enabled = true` is set in `config/sources.toml` AND no
`CWA_API_KEY` is in the environment, the worker fails immediately:

```
Error: CWA API key missing — set CWA_API_KEY in the environment
       (signup: docs/sources/cwa.md) or pass Builder::api_key for tests
```

This is intentional: a silent 401-loop is worse than a loud boot failure.

## Datasets the catalog walk emits

The walk emits three fixed rows under `source = 'cwa'`:

| `source_id`               | Upstream id   | What it is                                    | Refresh        |
|---------------------------|---------------|-----------------------------------------------|----------------|
| `cwa-observations`        | `O-A0001-001` | Real-time observations from automated stations | hourly         |
| `cwa-township-forecast`   | `F-C0032-001` | 36-hour outlook for all 22 counties           | every 6 hours  |
| `cwa-typhoon-track`       | `W-C0034-005` | Active + recent typhoon track polylines       | as published   |

The per-dataset data pulls (the JSON behind each row) are a follow-up; the
catalog walk itself only emits these metadata rows so the marketplace UI
can surface what CWA offers.

## Cron schedule

Default `cron_utc = "0 0 0,6,12,18 * * * *"` — every 6 hours. CWA observations
are short-cycle data that benefits from a more frequent refresh than the
nightly pass used for the registry sources.

## Rate-limit etiquette

The connector throttles outbound requests to one per second by default
(`Builder::throttle_ms(1000)`), well inside any reasonable interpretation of
CWA's informal "don't hammer" guidance. The robots.txt fetch at boot also
goes through the same throttle, so even bootstrap is polite.

## Key handling — what NOT to do

- **Don't log the key.** The connector wraps it in an `ApiKey` newtype whose
  `Debug` impl renders `"ApiKey(<redacted>)"`. If you reach into the
  internals for debugging, use the explicit `expose()` accessor — that's
  the one place a reviewer can grep for.
- **Don't check the key into git.** `.env` files, secret managers,
  CI variables — anything but a commit.
- **Don't share keys across environments.** Issue separate keys for dev,
  staging, and prod so a leaked dev key doesn't compromise production traffic.
