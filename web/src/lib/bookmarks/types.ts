/**
 * Shapes mirroring `gateway::bookmarks_routes` (#5a.4).
 */

import { COMMENT_TARGET_KINDS, type CommentTargetKind } from '$lib/comments/types';

/**
 * Bookmark/collection targets reuse the same polymorphic set
 * as comments (datasets / tools / connectors / playgrounds).
 * Re-export through this module so consumers can import
 * `BookmarkTargetKind` without crossing into `comments`.
 */
export const BOOKMARK_TARGET_KINDS = COMMENT_TARGET_KINDS;
export type BookmarkTargetKind = CommentTargetKind;

export interface Bookmark {
	id: string;
	target_kind: BookmarkTargetKind;
	target_id: string;
	created_at: string;
}

export interface ToggleResponse {
	outcome: 'bookmarked' | 'removed';
	id?: string;
}

export interface Collection {
	id: string;
	name: string;
	description?: string;
	created_at: string;
	updated_at: string;
}

export interface CollectionItem {
	target_kind: BookmarkTargetKind;
	target_id: string;
	added_at: string;
}

export const COLLECTION_NAME_MAX_LEN = 80;
export const COLLECTION_DESCRIPTION_MAX_LEN = 2048;

/**
 * Runtime narrowing for the bookmark response. TypeScript's
 * structural typing won't catch a gateway shape drift; this
 * helper rejects anything that doesn't match the expected
 * keys so the page surfaces an "unexpected response" state
 * rather than rendering `undefined`.
 */
export function parseBookmark(value: unknown): Bookmark | null {
	if (value === null || typeof value !== 'object') return null;
	const v = value as Record<string, unknown>;
	if (typeof v.id !== 'string') return null;
	if (typeof v.target_kind !== 'string') return null;
	if (!(BOOKMARK_TARGET_KINDS as readonly string[]).includes(v.target_kind)) return null;
	if (typeof v.target_id !== 'string') return null;
	if (typeof v.created_at !== 'string') return null;
	return {
		id: v.id,
		target_kind: v.target_kind as BookmarkTargetKind,
		target_id: v.target_id,
		created_at: v.created_at
	};
}

export function parseBookmarkArray(value: unknown): Bookmark[] | null {
	if (!Array.isArray(value)) return null;
	const out: Bookmark[] = [];
	for (const entry of value) {
		const parsed = parseBookmark(entry);
		if (parsed === null) return null;
		out.push(parsed);
	}
	return out;
}

export function parseCollection(value: unknown): Collection | null {
	if (value === null || typeof value !== 'object') return null;
	const v = value as Record<string, unknown>;
	if (typeof v.id !== 'string') return null;
	if (typeof v.name !== 'string') return null;
	if (v.description !== undefined && typeof v.description !== 'string') return null;
	if (typeof v.created_at !== 'string') return null;
	if (typeof v.updated_at !== 'string') return null;
	return {
		id: v.id,
		name: v.name,
		description: typeof v.description === 'string' ? v.description : undefined,
		created_at: v.created_at,
		updated_at: v.updated_at
	};
}

export function parseCollectionArray(value: unknown): Collection[] | null {
	if (!Array.isArray(value)) return null;
	const out: Collection[] = [];
	for (const entry of value) {
		const parsed = parseCollection(entry);
		if (parsed === null) return null;
		out.push(parsed);
	}
	return out;
}
