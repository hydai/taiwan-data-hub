/**
 * Shapes mirroring `gateway::moderation_routes` (#5a.2).
 * The wire format is JSON; runtime narrowing lives in
 * `gateway.ts` to keep TypeScript's structural typing from
 * silently accepting shape drift.
 */

import type { SubmissionKind, SubmissionStatus, SubmissionPayload } from '$lib/submissions/types';

/**
 * Moderator-side row shape. Differs from
 * `$lib/submissions/types::SubmissionSummary` only in that
 * `user_id` is included — moderators need to see who
 * authored each pending row.
 */
export interface ModerationSubmission {
	id: string;
	user_id: string;
	kind: SubmissionKind;
	status: SubmissionStatus;
	title: string;
	payload: SubmissionPayload;
	created_at: string;
	updated_at: string;
	reviewed_at?: string;
	reviewed_by?: string;
	review_reason?: string;
}

export interface DecisionResponse {
	submission: ModerationSubmission;
	audit_log_id: string;
}
