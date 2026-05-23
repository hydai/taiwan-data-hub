/**
 * Browser-side URL builders for the bookmarks / collections
 * surface (#5a.4). Same-origin paths so the host-only
 * session cookie reaches the gateway via the reverse proxy
 * (Caddy in prod, vite proxy in dev).
 */

import type { BookmarkTargetKind } from './types';

export function bookmarksUrl(kind?: BookmarkTargetKind): string {
	return kind ? `/api/v1/bookmarks?kind=${encodeURIComponent(kind)}` : '/api/v1/bookmarks';
}

export function collectionsUrl(): string {
	return '/api/v1/collections';
}

export function collectionByIdUrl(id: string): string {
	return `/api/v1/collections/${encodeURIComponent(id)}`;
}

export function collectionItemsUrl(id: string): string {
	return `/api/v1/collections/${encodeURIComponent(id)}/items`;
}

export function collectionItemUrl(id: string, kind: BookmarkTargetKind, targetId: string): string {
	return `/api/v1/collections/${encodeURIComponent(id)}/items/${encodeURIComponent(
		kind
	)}/${encodeURIComponent(targetId)}`;
}
