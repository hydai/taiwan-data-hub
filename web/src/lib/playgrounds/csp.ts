/**
 * Strict Content-Security-Policy for playground responses.
 *
 * The CSP enforces what the iframe sandbox attribute can't:
 *
 *   - `default-src 'none'`           — every fetch type is opt-in
 *   - `script-src 'self'`            — no inline `<script>`, no eval,
 *                                      no third-party scripts
 *   - `style-src 'self'`             — no inline `<style>`, no third
 *                                      -party stylesheets
 *   - `img-src 'self' data:`         — local assets + small data
 *                                      URLs (icons embedded in CSS)
 *   - `font-src 'self'`              — local fonts only
 *   - `connect-src 'none'`           — the iframe MAY NOT fetch
 *                                      directly. All API calls go
 *                                      through the parent via
 *                                      postMessage; this CSP rule
 *                                      makes that contract
 *                                      enforceable rather than
 *                                      polite. The sandbox's opaque
 *                                      origin already breaks same-
 *                                      origin fetch, but explicit
 *                                      `'none'` here means even a
 *                                      mis-configured CORS doesn't
 *                                      open a hole.
 *   - `frame-ancestors 'self'`       — only our framing page may
 *                                      embed the playground
 *   - `base-uri 'none'`              — block `<base href>` rewrites
 *   - `form-action 'none'`           — block `<form action>` posts
 *
 * Authors of new playgrounds: read `playgrounds/README.md` for the
 * contract this CSP enforces. No inline scripts. No third-party
 * origins. All `fetch`-like operations through `tdh.fetch()`.
 */
export function playgroundCspHeader(): string {
	return [
		"default-src 'none'",
		"script-src 'self'",
		"style-src 'self'",
		"img-src 'self' data:",
		"font-src 'self'",
		"connect-src 'none'",
		"frame-ancestors 'self'",
		"base-uri 'none'",
		"form-action 'none'"
	].join('; ');
}
