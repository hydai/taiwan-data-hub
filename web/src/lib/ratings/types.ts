/**
 * Shapes mirroring `gateway::ratings_routes` (#5a.5).
 */

import { COMMENT_TARGET_KINDS, type CommentTargetKind } from '$lib/comments/types';

export const RATING_TARGET_KINDS = COMMENT_TARGET_KINDS;
export type RatingTargetKind = CommentTargetKind;

export const SCORE_MIN = 1;
export const SCORE_MAX = 5;

export interface RatingView {
	/** `null` when there are no ratings yet. */
	avg_score: number | null;
	count: number;
	/** `null` for anonymous viewers or those who haven't rated. */
	viewer_score: number | null;
	last_refreshed_at?: string;
}

export interface UpsertRatingResponse {
	id: string;
	target_kind: RatingTargetKind;
	target_id: string;
	score: number;
	created_at: string;
	updated_at: string;
}

/**
 * Runtime narrower for the GET view response. Returns
 * `null` on shape mismatch so callers can surface an
 * "unexpected response" state instead of rendering
 * undefined. Also rejects `NaN`/`Infinity`/out-of-range
 * values that pass `typeof === 'number'` but would render
 * as garbage (`NaN.toFixed(2)` → `"NaN"`).
 */
export function parseRatingView(value: unknown): RatingView | null {
	if (value === null || typeof value !== 'object') return null;
	const v = value as Record<string, unknown>;
	// avg_score: null or a finite number. The server-side
	// CHECK constraint rejects any rating outside
	// `[SCORE_MIN, SCORE_MAX]`, so the average inherits
	// the same bounds whenever there's at least one
	// rating — enforced below in the count/avg invariant.
	if (v.avg_score !== null) {
		if (typeof v.avg_score !== 'number' || !Number.isFinite(v.avg_score)) return null;
	}
	// count: non-negative integer.
	if (typeof v.count !== 'number' || !Number.isInteger(v.count) || v.count < 0) return null;
	// Enforce the avg/count invariant the server promises:
	// `count == 0` must come with `avg_score == null` (no
	// ratings → no average), and `count > 0` must come
	// with a numeric avg in `[SCORE_MIN, SCORE_MAX]` —
	// the SQL CHECK forces every rating into that range,
	// so the average inherits it. Reject either drift so
	// a gateway shape regression can't leak misleading
	// values (e.g., "0.00 ★ · 3 ratings") into the UI.
	if (v.count === 0 && v.avg_score !== null) return null;
	if (v.count > 0) {
		if (v.avg_score === null) return null;
		if (typeof v.avg_score !== 'number' || v.avg_score < SCORE_MIN || v.avg_score > SCORE_MAX)
			return null;
	}
	// viewer_score: null or an integer in [SCORE_MIN, SCORE_MAX].
	if (v.viewer_score !== null) {
		if (
			typeof v.viewer_score !== 'number' ||
			!Number.isInteger(v.viewer_score) ||
			v.viewer_score < SCORE_MIN ||
			v.viewer_score > SCORE_MAX
		)
			return null;
		// A viewer's own rating implies their row is part
		// of the aggregate, so `count` must be >= 1 and
		// `avg_score` must be a number. Reject the
		// inconsistent shape — otherwise the UI could
		// render "5 ★ (your rating)" next to "No ratings
		// yet" on a gateway drift.
		if (v.count === 0 || v.avg_score === null) return null;
	}
	if (v.last_refreshed_at !== undefined && typeof v.last_refreshed_at !== 'string') return null;
	return {
		avg_score: typeof v.avg_score === 'number' ? v.avg_score : null,
		count: v.count,
		viewer_score: typeof v.viewer_score === 'number' ? v.viewer_score : null,
		last_refreshed_at: typeof v.last_refreshed_at === 'string' ? v.last_refreshed_at : undefined
	};
}
