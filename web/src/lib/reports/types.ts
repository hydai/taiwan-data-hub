/**
 * Shapes mirroring `gateway::reports_routes` (#5a.6).
 */

export const REPORT_TARGET_KINDS = ['comment', 'submission'] as const;
export type ReportTargetKind = (typeof REPORT_TARGET_KINDS)[number];

export const REPORT_REASONS = [
	'spam',
	'harassment',
	'off_topic',
	'illegal',
	'inaccurate',
	'other'
] as const;
export type ReportReason = (typeof REPORT_REASONS)[number];

export const REPORT_ACTIONS = ['hide', 'keep', 'delete', 'warn_author'] as const;
export type ReportAction = (typeof REPORT_ACTIONS)[number];

export const REPORT_BODY_MAX_LEN = 2048;

/** Human-readable labels for the radio group + queue. */
export const REASON_LABELS: Record<ReportReason, string> = {
	spam: 'Spam',
	harassment: 'Harassment',
	off_topic: 'Off-topic',
	illegal: 'Illegal content',
	inaccurate: 'Inaccurate / misleading data',
	other: 'Other'
};

export interface ReportSubmitResponse {
	id: string;
	reporter_count: number;
	freshly_hidden: boolean;
}

export interface Report {
	id: string;
	reporter_id?: string;
	target_kind: ReportTargetKind;
	target_id: string;
	reason: ReportReason;
	body?: string;
	created_at: string;
	resolved_at?: string;
	resolved_by?: string;
	action_taken?: ReportAction;
	resolution_note?: string;
}

/**
 * Runtime narrower for the GET responses. Returns `null`
 * on shape mismatch so the consumer can degrade to an
 * "unexpected response" state.
 */
export function parseReport(value: unknown): Report | null {
	if (value === null || typeof value !== 'object') return null;
	const v = value as Record<string, unknown>;
	if (typeof v.id !== 'string') return null;
	if (v.reporter_id !== undefined && typeof v.reporter_id !== 'string') return null;
	if (typeof v.target_kind !== 'string') return null;
	if (!(REPORT_TARGET_KINDS as readonly string[]).includes(v.target_kind)) return null;
	if (typeof v.target_id !== 'string') return null;
	if (typeof v.reason !== 'string') return null;
	if (!(REPORT_REASONS as readonly string[]).includes(v.reason)) return null;
	if (v.body !== undefined && typeof v.body !== 'string') return null;
	if (typeof v.created_at !== 'string') return null;
	if (v.resolved_at !== undefined && typeof v.resolved_at !== 'string') return null;
	if (v.resolved_by !== undefined && typeof v.resolved_by !== 'string') return null;
	if (v.action_taken !== undefined) {
		if (typeof v.action_taken !== 'string') return null;
		if (!(REPORT_ACTIONS as readonly string[]).includes(v.action_taken)) return null;
	}
	if (v.resolution_note !== undefined && typeof v.resolution_note !== 'string') return null;
	return {
		id: v.id,
		reporter_id: typeof v.reporter_id === 'string' ? v.reporter_id : undefined,
		target_kind: v.target_kind as ReportTargetKind,
		target_id: v.target_id,
		reason: v.reason as ReportReason,
		body: typeof v.body === 'string' ? v.body : undefined,
		created_at: v.created_at,
		resolved_at: typeof v.resolved_at === 'string' ? v.resolved_at : undefined,
		resolved_by: typeof v.resolved_by === 'string' ? v.resolved_by : undefined,
		action_taken: typeof v.action_taken === 'string' ? (v.action_taken as ReportAction) : undefined,
		resolution_note: typeof v.resolution_note === 'string' ? v.resolution_note : undefined
	};
}

export function parseReportArray(value: unknown): Report[] | null {
	if (!Array.isArray(value)) return null;
	const out: Report[] = [];
	for (const entry of value) {
		const parsed = parseReport(entry);
		if (parsed === null) return null;
		out.push(parsed);
	}
	return out;
}
