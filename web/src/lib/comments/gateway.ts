/**
 * Client helpers for `/api/v1/comments` (#5a.3).
 */

import { COMMENT_TARGET_KINDS, type CommentTargetKind, type RenderedComment } from './types';

/**
 * URL builders are intentionally same-origin (root-relative).
 * Browser fetches resolve against the page origin so the
 * host-only session cookie (no `Domain=` attribute) is sent
 * automatically. Production routes `/api/v1/*` to the gateway
 * via the reverse proxy; dev does the same via the vite
 * `server.proxy` config. The internal `GATEWAY_HTTP_URL` never
 * reaches the browser.
 */
export function commentsUrl(): string {
	return `/api/v1/comments`;
}

/** Build the URL for an individual comment (edit / delete). */
export function commentByIdUrl(id: string): string {
	return `/api/v1/comments/${encodeURIComponent(id)}`;
}

/** Build the list URL with the required query params. */
export function commentsListUrl(targetKind: CommentTargetKind, targetId: string): string {
	const params = new URLSearchParams({
		target_kind: targetKind,
		target_id: targetId
	});
	return `${commentsUrl()}?${params.toString()}`;
}

/**
 * Narrow a JSON-decoded value into a [`RenderedComment`] or
 * return `null` on shape mismatch.
 */
export function parseRenderedComment(value: unknown): RenderedComment | null {
	if (value === null || typeof value !== 'object') return null;
	const v = value as Record<string, unknown>;
	if (typeof v.id !== 'string') return null;
	if (!isKind(v.target_kind)) return null;
	if (typeof v.target_id !== 'string') return null;
	if (v.depth !== 0 && v.depth !== 1) return null;
	if (typeof v.body_html !== 'string') return null;
	if (typeof v.created_at !== 'string') return null;
	if (typeof v.is_deleted !== 'boolean') return null;
	if (typeof v.is_hidden !== 'boolean') return null;
	// Optional fields validated only when present.
	if (v.parent_id !== undefined && typeof v.parent_id !== 'string') return null;
	if (v.user_id !== undefined && typeof v.user_id !== 'string') return null;
	if (v.body_md !== undefined && typeof v.body_md !== 'string') return null;
	if (v.edited_at !== undefined && typeof v.edited_at !== 'string') return null;
	if (v.deleted_at !== undefined && typeof v.deleted_at !== 'string') return null;
	return {
		id: v.id,
		target_kind: v.target_kind,
		target_id: v.target_id,
		parent_id: v.parent_id as string | undefined,
		user_id: v.user_id as string | undefined,
		depth: v.depth as 0 | 1,
		body_md: v.body_md as string | undefined,
		body_html: v.body_html,
		created_at: v.created_at,
		edited_at: v.edited_at as string | undefined,
		deleted_at: v.deleted_at as string | undefined,
		is_deleted: v.is_deleted,
		is_hidden: v.is_hidden
	};
}

export function parseRenderedCommentArray(value: unknown): RenderedComment[] | null {
	if (!Array.isArray(value)) return null;
	const out: RenderedComment[] = [];
	for (const entry of value) {
		const parsed = parseRenderedComment(entry);
		if (parsed === null) return null;
		out.push(parsed);
	}
	return out;
}

function isKind(value: unknown): value is CommentTargetKind {
	return typeof value === 'string' && (COMMENT_TARGET_KINDS as readonly string[]).includes(value);
}
