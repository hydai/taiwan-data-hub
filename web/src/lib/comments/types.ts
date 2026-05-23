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
	/**
	 * `true` when the comment has been hidden by community
	 * reports or a moderator. `body_html` already carries
	 * the placeholder text; the UI also branches on this
	 * to suppress Edit / Delete / Reply / Report
	 * affordances on hidden rows.
	 */
	is_hidden: boolean;
}

/** Max characters (Unicode scalar values) the API accepts. */
export const MAX_COMMENT_BODY_LEN = 8192;

/**
 * Placeholder HTML the backend substitutes for a
 * comment whose `hidden_at` is set. Mirrors the string
 * in `auth::comments::render_row`. The frontend uses
 * the same constant when it has to optimistically flip
 * `body_html` after an auto-hide trip so the two
 * sources can't drift.
 */
export const HIDDEN_COMMENT_HTML = '<p>[hidden by community reports]</p>';
