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
	// avg_score: null or a finite number in [0, SCORE_MAX].
	if (v.avg_score !== null) {
		if (typeof v.avg_score !== 'number' || !Number.isFinite(v.avg_score)) return null;
		if (v.avg_score < 0 || v.avg_score > SCORE_MAX) return null;
	}
	// count: non-negative integer.
	if (typeof v.count !== 'number' || !Number.isInteger(v.count) || v.count < 0) return null;
	// Enforce the avg/count invariant the server promises:
	// `count == 0` must come with `avg_score == null`
	// (no ratings → no average), and `count > 0` must
	// come with a numeric avg. Reject either drift so a
	// gateway shape regression can't leak a misleading
	// "0.00 ★ · 0 ratings" line into the UI.
	if (v.count === 0 && v.avg_score !== null) return null;
	if (v.count > 0 && v.avg_score === null) return null;
	// viewer_score: null or an integer in [SCORE_MIN, SCORE_MAX].
	if (v.viewer_score !== null) {
		if (
			typeof v.viewer_score !== 'number' ||
			!Number.isInteger(v.viewer_score) ||
			v.viewer_score < SCORE_MIN ||
			v.viewer_score > SCORE_MAX
		)
			return null;
	}
	if (v.last_refreshed_at !== undefined && typeof v.last_refreshed_at !== 'string') return null;
	return {
		avg_score: typeof v.avg_score === 'number' ? v.avg_score : null,
		count: v.count,
		viewer_score: typeof v.viewer_score === 'number' ? v.viewer_score : null,
		last_refreshed_at: typeof v.last_refreshed_at === 'string' ? v.last_refreshed_at : undefined
	};
}
