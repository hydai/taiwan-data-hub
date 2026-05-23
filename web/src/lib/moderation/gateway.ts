/**
 * Client helpers for `/api/v1/admin/submissions` (#5a.2).
 * Mirrors the structure of `$lib/account/gateway.ts` +
 * `$lib/submissions/gateway.ts`.
 */

import { parseSubmissionSummary } from '$lib/submissions/gateway';
import type { SubmissionKind } from '$lib/submissions/types';
import type { DecisionResponse, ModerationSubmission } from './types';

/** Build the URL for the moderation collection. */
export function moderationListUrl(base: string, kind?: SubmissionKind): string {
	const url = `${base}/api/v1/admin/submissions`;
	return kind ? `${url}?kind=${encodeURIComponent(kind)}` : url;
}

/** Build the URL for an individual moderator action. */
export function moderationDecisionUrl(
	base: string,
	id: string,
	action: 'approve' | 'reject'
): string {
	return `${base}/api/v1/admin/submissions/${encodeURIComponent(id)}/${action}`;
}

/**
 * Narrow a moderator-side row. The author-side summary parser
 * already validates every common field; this wrapper just
 * adds the `user_id` check that the moderator surface adds on
 * top.
 */
export function parseModerationSubmission(value: unknown): ModerationSubmission | null {
	const base = parseSubmissionSummary(value);
	if (base === null) return null;
	const v = value as Record<string, unknown>;
	if (typeof v.user_id !== 'string') return null;
	return { ...base, user_id: v.user_id };
}

export function parseModerationSubmissionArray(value: unknown): ModerationSubmission[] | null {
	if (!Array.isArray(value)) return null;
	const out: ModerationSubmission[] = [];
	for (const entry of value) {
		const parsed = parseModerationSubmission(entry);
		if (parsed === null) return null;
		out.push(parsed);
	}
	return out;
}

export function parseDecisionResponse(value: unknown): DecisionResponse | null {
	if (value === null || typeof value !== 'object') return null;
	const v = value as Record<string, unknown>;
	const submission = parseModerationSubmission(v.submission);
	if (submission === null) return null;
	if (typeof v.audit_log_id !== 'string') return null;
	return { submission, audit_log_id: v.audit_log_id };
}
