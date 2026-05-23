/**
 * Helpers for talking to the gateway's `/api/v1/submissions`
 * surface from SvelteKit (#5a.1). Mirrors the structure of
 * `$lib/account/gateway.ts` so the two surfaces share
 * conventions: pure-functional, no module-level state, URL
 * builders + runtime narrowing for each response shape.
 */

import {
	SUBMISSION_KINDS,
	SUBMISSION_STATUSES,
	type CreateSubmissionResponse,
	type SubmissionKind,
	type SubmissionPayload,
	type SubmissionStatus,
	type SubmissionSummary
} from './types';

/** Build the absolute URL for the submissions collection. */
export function submissionsUrl(base: string): string {
	return `${base}/api/v1/submissions`;
}

/** Build the absolute URL for a single submission by id. */
export function submissionByIdUrl(base: string, id: string): string {
	return `${base}/api/v1/submissions/${encodeURIComponent(id)}`;
}

/**
 * Narrow a JSON-decoded value into a [`SubmissionSummary`]
 * or return `null` on shape mismatch. Allows the caller to
 * surface a clear "unexpected gateway response" message
 * instead of letting a runtime crash render an empty page.
 */
export function parseSubmissionSummary(value: unknown): SubmissionSummary | null {
	if (value === null || typeof value !== 'object') {
		return null;
	}
	const v = value as Record<string, unknown>;
	if (typeof v.id !== 'string') return null;
	if (!isKind(v.kind)) return null;
	if (!isStatus(v.status)) return null;
	if (typeof v.title !== 'string') return null;
	if (typeof v.created_at !== 'string') return null;
	if (typeof v.updated_at !== 'string') return null;
	const payload = parsePayload(v.payload);
	if (payload === null) return null;
	// Optional fields — only validate type when present.
	if (v.reviewed_at !== undefined && typeof v.reviewed_at !== 'string') return null;
	if (v.reviewed_by !== undefined && typeof v.reviewed_by !== 'string') return null;
	if (v.review_reason !== undefined && typeof v.review_reason !== 'string') return null;
	return {
		id: v.id,
		kind: v.kind,
		status: v.status,
		title: v.title,
		payload,
		created_at: v.created_at,
		updated_at: v.updated_at,
		reviewed_at: v.reviewed_at as string | undefined,
		reviewed_by: v.reviewed_by as string | undefined,
		review_reason: v.review_reason as string | undefined
	};
}

export function parseSubmissionSummaryArray(value: unknown): SubmissionSummary[] | null {
	if (!Array.isArray(value)) {
		return null;
	}
	const out: SubmissionSummary[] = [];
	for (const entry of value) {
		const parsed = parseSubmissionSummary(entry);
		if (parsed === null) return null;
		out.push(parsed);
	}
	return out;
}

export function parseCreateSubmissionResponse(value: unknown): CreateSubmissionResponse | null {
	if (value === null || typeof value !== 'object') {
		return null;
	}
	const v = value as Record<string, unknown>;
	if (typeof v.id !== 'string') return null;
	if (!isStatus(v.status)) return null;
	return { id: v.id, status: v.status };
}

function isKind(value: unknown): value is SubmissionKind {
	return typeof value === 'string' && (SUBMISSION_KINDS as readonly string[]).includes(value);
}

function isStatus(value: unknown): value is SubmissionStatus {
	return typeof value === 'string' && (SUBMISSION_STATUSES as readonly string[]).includes(value);
}

function parsePayload(value: unknown): SubmissionPayload | null {
	if (value === null || typeof value !== 'object') return null;
	const v = value as Record<string, unknown>;
	const kind = v.kind;
	if (!isKind(kind)) return null;
	switch (kind) {
		case 'dataset':
			if (
				typeof v.title !== 'string' ||
				typeof v.description !== 'string' ||
				typeof v.source_url !== 'string' ||
				typeof v.license !== 'string' ||
				typeof v.domain_slug !== 'string'
			) {
				return null;
			}
			return {
				kind,
				title: v.title,
				description: v.description,
				source_url: v.source_url,
				license: v.license,
				domain_slug: v.domain_slug
			};
		case 'tool':
			if (
				typeof v.name !== 'string' ||
				typeof v.description !== 'string' ||
				typeof v.repo_url !== 'string' ||
				typeof v.language !== 'string'
			) {
				return null;
			}
			return {
				kind,
				name: v.name,
				description: v.description,
				repo_url: v.repo_url,
				language: v.language
			};
		case 'connector':
			if (
				typeof v.name !== 'string' ||
				typeof v.description !== 'string' ||
				typeof v.repo_url !== 'string' ||
				typeof v.license !== 'string'
			) {
				return null;
			}
			return {
				kind,
				name: v.name,
				description: v.description,
				repo_url: v.repo_url,
				license: v.license
			};
		case 'playground':
			if (
				typeof v.name !== 'string' ||
				typeof v.description !== 'string' ||
				typeof v.demo_url !== 'string'
			) {
				return null;
			}
			// `repo_url` is optional on this kind.
			if (v.repo_url !== undefined && v.repo_url !== null && typeof v.repo_url !== 'string') {
				return null;
			}
			return {
				kind,
				name: v.name,
				description: v.description,
				demo_url: v.demo_url,
				repo_url: typeof v.repo_url === 'string' ? v.repo_url : null
			};
	}
}
