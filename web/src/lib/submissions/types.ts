/**
 * Shapes mirroring `gateway::submissions_routes` (#5a.1). The
 * gateway is the canonical type-source; TypeScript's structural
 * typing won't catch drift on its own, so the runtime
 * narrowing helpers in `gateway.ts` are the actual guardrail.
 * If the Rust-side response shape changes, parsing falls back
 * to `null` and the page surfaces an "unexpected response"
 * state rather than a silent mis-render.
 */

/**
 * One of four kinds the submission form supports. The wire
 * strings match the `submissions_kind_known` CHECK constraint
 * in migration 0013 and the `SubmissionKind::as_str` mapping
 * in the Rust crate.
 */
export const SUBMISSION_KINDS = ['dataset', 'tool', 'connector', 'playground'] as const;
export type SubmissionKind = (typeof SUBMISSION_KINDS)[number];

/**
 * Lifecycle states matching the `submissions_status_known`
 * CHECK constraint. `pending` is the only state the form
 * itself creates; the others surface on the "my submissions"
 * list once a moderator (or the author themselves, via
 * withdraw) moves the row.
 */
export const SUBMISSION_STATUSES = ['pending', 'approved', 'rejected', 'withdrawn'] as const;
export type SubmissionStatus = (typeof SUBMISSION_STATUSES)[number];

/**
 * Per-kind payload shape. Mirrors `auth::SubmissionPayload` on
 * the Rust side — keep the field lists in lockstep with the
 * `validate_and_normalize` rules there.
 */
export type SubmissionPayload =
	| {
			kind: 'dataset';
			title: string;
			description: string;
			source_url: string;
			license: string;
			domain_slug: string;
	  }
	| {
			kind: 'tool';
			name: string;
			description: string;
			repo_url: string;
			language: string;
	  }
	| {
			kind: 'connector';
			name: string;
			description: string;
			repo_url: string;
			license: string;
	  }
	| {
			kind: 'playground';
			name: string;
			description: string;
			demo_url: string;
			repo_url: string | null;
	  };

/**
 * Server-returned shape for a single submission. The
 * decision triple (`reviewed_at` / `reviewed_by` /
 * `review_reason`) is omitted from the JSON when the row is
 * still `pending` — we model that with `undefined` here
 * rather than `null` so a "field absent" structurally differs
 * from "moderator explicitly cleared it".
 */
export interface SubmissionSummary {
	id: string;
	kind: SubmissionKind;
	status: SubmissionStatus;
	title: string;
	payload: SubmissionPayload;
	created_at: string;
	updated_at: string;
	reviewed_at?: string;
	reviewed_by?: string;
	review_reason?: string;
}

/**
 * `POST /api/v1/submissions` happy-path body. The gateway
 * echoes the assigned id + initial status so the SvelteKit
 * form can redirect to the detail / list view immediately.
 */
export interface CreateSubmissionResponse {
	id: string;
	status: SubmissionStatus;
}

/**
 * Maximum lengths the Rust validator enforces. Mirrored here
 * for client-side feedback BEFORE the network round trip —
 * the server is the source of truth on the actual cap, but
 * the form can refuse over-limit text immediately so a slow
 * 400 doesn't gate UX.
 */
export const SUBMISSION_FIELD_LIMITS = {
	name: 120,
	description: 2048,
	url: 2048
} as const;
