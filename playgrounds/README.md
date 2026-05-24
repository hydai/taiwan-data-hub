# `playgrounds/` — interactive demo apps

Each subdirectory here is a self-contained playground served under
`/playgrounds/<slug>/` on the marketplace site. The framework
introduced in **M6 #6.3** sandboxes every playground inside an
`<iframe>` with a strict CSP so the only way for a playground to
reach the gateway is through the framework-provided helpers.

## Authoring a new playground

1. Pick a kebab-case slug, e.g. `company-360`. Reserved: `_template`.
2. Create `playgrounds/<slug>/` with these files:

   ```
   playgrounds/<slug>/
       manifest.json     # metadata (see schema below)
       index.html        # entry point loaded inside the iframe
       app.js            # main script — referenced by index.html
       (other assets)    # any extra .js, .mjs, .css, .json, .svg,
                         #   .txt, or .html; served as-is.
   ```

   **Text-only assets for now.** The build-time loader reads each
   file as a UTF-8 string (`?raw`), which would corrupt PNG / JPG /
   WOFF / other binary content. Restricting the contract to text
   formats keeps the framework simple; binary support can ship in a
   future iteration with a parallel `?url` loader.

3. **No inline `<script>` tags.** The CSP forbids them. Put logic in
   `app.js` and load it via `<script src="./app.js"></script>`.
4. **No `<link>` to external origins.** CSS, fonts, images must all be
   served from the same `/playgrounds/<slug>/app/` prefix.
5. **No direct `fetch()` against the gateway.** The iframe runs with
   a unique opaque origin (the consequence of
   `sandbox="allow-scripts"` without `allow-same-origin`), so any
   `fetch('/api/v1/...')` will be blocked by CORS. Use the framework
   helpers instead — see [`Framework API`](#framework-api).

## `manifest.json` schema

```json
{
  "title_i18n": { "zh-TW": "公司 360", "en": "Company 360" },
  "description_i18n": { "zh-TW": "三庫聯查公司資訊", "en": "Three-DB company lookup" },
  "tags": ["company", "judicial"],
  "status": "stable"
}
```

| Field | Required | Notes |
|---|---|---|
| `title_i18n.zh-TW` | yes | source language |
| `title_i18n.en` | recommended | falls back to zh-TW if absent |
| `description_i18n.zh-TW` | yes | one paragraph; shown on the index card |
| `description_i18n.en` | recommended | falls back to zh-TW if absent |
| `tags` | yes | array of kebab-case tags, used for index filtering |
| `status` | yes | one of `stable`, `beta`, `experimental` |

The loader validates these at build time; a malformed manifest fails
the build, not first request.

## Framework API

A small global `tdh` is injected by `index.html` (via the framework's
shim — see `web/src/lib/playgrounds/shim.js`). It exposes:

### `tdh.getState<T>(): Promise<T | null>`

Returns the initial state decoded from the share link's `?state=`
query parameter, or `null` if none. Resolves once after the parent
finishes the init handshake.

### `tdh.setState(value: unknown): void`

Updates the share-link state. The parent re-encodes and replaces the
URL query string so the user can copy the bar to share. Throttle this
in your app — every call posts a message and rewrites history.

### `tdh.fetch(path: string, init?: RequestInit): Promise<Response>`

Proxies a fetch through the parent frame. `path` MUST be a
gateway-relative path starting with `/api/v1/`. The parent rejects
any other path; the iframe never gets a chance to hit
arbitrary URLs.

## Iframe security model

- `sandbox="allow-scripts"` — scripts run, but the iframe has a
  unique opaque origin: no DOM access to the parent, no cookies, no
  `localStorage`, no forms, no top-navigation, no popups.
- `Content-Security-Policy` on the playground response: see
  `web/src/lib/playgrounds/csp.ts`. `default-src 'none'` baseline,
  scripts and styles from `'self'` only, no `unsafe-inline`, no
  third-party origins.
- The framing page's `postMessage` handler verifies `event.source`
  against the iframe's `contentWindow` reference and rejects
  unrecognised message types so the iframe can't trick the parent
  into running arbitrary fetches.

## Local development

```bash
pnpm --filter web dev
# open http://localhost:3000/playgrounds/_template
```

The `_template` playground is shipped with the framework as a
self-test and reference implementation. Use it as a starting point
for new playgrounds: copy the directory, rename to your slug, edit
the manifest, replace `app.js` with your code.
