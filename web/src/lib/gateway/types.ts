/**
 * Operating-mode discriminant exposed to every layout
 * consumer. The two string values must stay in lockstep with
 * `shared::Mode::as_str` on the Rust side — TypeScript does
 * NOT auto-sync with Rust, so adding a new variant to the
 * enum requires:
 *
 *   1. Update `Mode::as_str` (Rust) to emit the new string.
 *   2. Add the new string to this union (TS).
 *   3. Update `parseConfigResponse` in `./config.ts` to
 *      accept the new variant.
 *
 * Until that lockstep update lands, an unknown mode string
 * from the gateway falls back to `'personal'` in
 * `+layout.server.ts` (via `parseConfigResponse` returning
 * `null` on unrecognised strings + the layout's fail-safe
 * default). That hides the auth UI on a misconfigured deploy
 * rather than crashing the layout — safer default, but it
 * does mean a Rust-side variant addition without the TS
 * update will look like a "personal mode" deploy to the
 * frontend.
 */
export type GatewayMode = 'personal' | 'multi-user';
