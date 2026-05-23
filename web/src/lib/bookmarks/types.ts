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
