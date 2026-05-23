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
 * undefined.
 */
export function parseRatingView(value: unknown): RatingView | null {
	if (value === null || typeof value !== 'object') return null;
	const v = value as Record<string, unknown>;
	if (v.avg_score !== null && typeof v.avg_score !== 'number') return null;
	if (typeof v.count !== 'number') return null;
	if (v.viewer_score !== null && typeof v.viewer_score !== 'number') return null;
	if (v.last_refreshed_at !== undefined && typeof v.last_refreshed_at !== 'string') return null;
	return {
		avg_score: typeof v.avg_score === 'number' ? v.avg_score : null,
		count: v.count,
		viewer_score: typeof v.viewer_score === 'number' ? v.viewer_score : null,
		last_refreshed_at: typeof v.last_refreshed_at === 'string' ? v.last_refreshed_at : undefined
	};
}
