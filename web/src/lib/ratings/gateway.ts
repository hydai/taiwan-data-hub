/**
 * Browser-side URL builders for the ratings surface (#5a.5).
 */

import type { RatingTargetKind } from './types';

export function ratingsUrl(): string {
	return '/api/v1/ratings';
}

export function ratingByTargetUrl(kind: RatingTargetKind, targetId: string): string {
	return `/api/v1/ratings/${encodeURIComponent(kind)}/${encodeURIComponent(targetId)}`;
}
