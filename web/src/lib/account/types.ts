/**
 * Shape mirroring `gateway::api_keys_routes::ApiKeySummary`. The
 * gateway never emits the cleartext key or the SHA-256 hash; this
 * type intentionally has no field for either so a future drift on
 * the Rust side (an accidental `Serialize` derive) shows up as a
 * compile-time mismatch the moment a tsc run touches this file.
 */
export interface ApiKeySummary {
	id: string;
	name: string;
	key_prefix: string;
	scopes: string[];
	rate_limit_tier: string;
	created_at: string;
	last_used_at: string | null;
	revoked_at: string | null;
}

/**
 * One-time response carrying the cleartext key. Used for the
 * Account page modal that shows the key exactly once; we drop the
 * `cleartext` field from in-memory state the moment the user
 * dismisses the modal.
 */
export interface IssuedApiKey {
	id: string;
	cleartext: string;
	key_prefix: string;
}

/**
 * Rate-limit tier values mirroring `auth::ALLOWED_TIERS`. Kept in
 * lockstep with the migration's `mcp_api_keys_tier_allowed`
 * CHECK constraint via the
 * `allowed_tiers_set_matches_migration_check` unit test in the
 * gateway crate; any drift here without an accompanying update on
 * the Rust side will be caught when the page POSTs an invalid
 * tier and the gateway returns 400.
 */
export const RATE_LIMIT_TIERS = ['free', 'pro', 'enterprise'] as const;
export type RateLimitTier = (typeof RATE_LIMIT_TIERS)[number];
