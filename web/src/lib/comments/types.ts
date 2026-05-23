/**
 * Shapes mirroring `gateway::comments_routes` (#5a.3).
 */

export const COMMENT_TARGET_KINDS = ['dataset', 'tool', 'connector', 'playground'] as const;
export type CommentTargetKind = (typeof COMMENT_TARGET_KINDS)[number];

/**
 * Sanitised comment shape the API returns. `body_html` is the
 * server-rendered HTML (Markdown → comrak → ammonia); the web
 * layer renders it via `{@html}` because the sanitiser is the
 * load-bearing XSS guard.
 */
export interface RenderedComment {
	id: string;
	target_kind: CommentTargetKind;
	target_id: string;
	parent_id?: string;
	user_id?: string;
	depth: 0 | 1;
	body_md?: string;
	body_html: string;
	created_at: string;
	edited_at?: string;
	deleted_at?: string;
	is_deleted: boolean;
}

/** Max characters (Unicode scalar values) the API accepts. */
export const MAX_COMMENT_BODY_LEN = 8192;
