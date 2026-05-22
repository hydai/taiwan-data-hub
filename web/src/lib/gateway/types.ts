/**
 * Operating-mode discriminant exposed to every layout
 * consumer. The strings match `shared::Mode::as_str` on the
 * Rust side — a new variant on the Rust side surfaces as a
 * new string here, and the discriminated-union below will
 * fail to typecheck consumers that don't handle it.
 */
export type GatewayMode = 'personal' | 'multi-user';
