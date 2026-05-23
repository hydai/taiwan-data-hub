<!--
	Report button (#5a.6). Mounted alongside the
	edit/delete affordances on a comment row (and the
	submission detail view). Opens an inline disclosure
	with the reason picker + optional context textarea;
	submitting POSTs to `/api/v1/reports` and collapses
	with a one-line confirmation.

	The component is polymorphic over `target_kind` so it
	reuses for both comments and submissions; the
	`onReported` callback lets the parent component
	immediately reflect the new hidden state when the
	auto-hide threshold trips.
-->
<script lang="ts">
	import {
		REASON_LABELS,
		REPORT_BODY_MAX_LEN,
		REPORT_REASONS,
		type ReportReason,
		type ReportSubmitResponse,
		type ReportTargetKind
	} from '$lib/reports/types';

	let {
		targetKind,
		targetId,
		onReported
	}: {
		targetKind: ReportTargetKind;
		targetId: string;
		onReported?: (response: ReportSubmitResponse) => void;
	} = $props();

	let open = $state(false);
	let reason = $state<ReportReason>('spam');
	let body = $state('');
	let inFlight = $state(false);
	let error = $state<string | null>(null);
	let confirmation = $state<string | null>(null);

	function reset(): void {
		reason = 'spam';
		body = '';
		error = null;
	}

	function cancel(): void {
		open = false;
		reset();
	}

	async function submit(): Promise<void> {
		if (inFlight) return;
		const trimmed = body.trim();
		if (trimmed.length > REPORT_BODY_MAX_LEN) {
			error = `Context too long (max ${REPORT_BODY_MAX_LEN} characters).`;
			return;
		}
		inFlight = true;
		error = null;
		try {
			const res = await fetch('/api/v1/reports', {
				method: 'POST',
				credentials: 'include',
				headers: {
					accept: 'application/json',
					'content-type': 'application/json'
				},
				body: JSON.stringify({
					target_kind: targetKind,
					target_id: targetId,
					reason,
					body: trimmed.length === 0 ? null : trimmed
				})
			});
			if (!res.ok) {
				const envelope = (await res.json().catch(() => null)) as {
					message?: string;
				} | null;
				if (res.status === 401) {
					error = 'Please sign in to file a report.';
				} else {
					error = envelope?.message ?? `Failed to submit report (${res.status}).`;
				}
				return;
			}
			const payload = (await res.json().catch(() => null)) as ReportSubmitResponse | null;
			if (payload === null) {
				error = 'Gateway returned an unexpected response.';
				return;
			}
			confirmation = payload.freshly_hidden
				? 'Reported — this content has been hidden pending moderator review.'
				: 'Reported — thanks. A moderator will take a look.';
			open = false;
			reset();
			onReported?.(payload);
		} catch (e) {
			console.error('[report] submit failed:', e);
			error = 'Network error — please try again.';
		} finally {
			inFlight = false;
		}
	}
</script>

{#if confirmation}
	<span class="text-xs text-neutral-500" role="status">{confirmation}</span>
{:else if !open}
	<button
		type="button"
		onclick={() => (open = true)}
		class="text-xs underline underline-offset-2 hover:text-neutral-700"
	>
		Report
	</button>
{:else}
	<form
		class="mt-2 space-y-2 rounded-md border border-neutral-200 p-3 text-sm"
		onsubmit={(e) => {
			e.preventDefault();
			submit();
		}}
	>
		<fieldset>
			<legend class="text-xs font-medium text-neutral-700">Why are you reporting this?</legend>
			<div class="mt-1 grid grid-cols-2 gap-1">
				{#each REPORT_REASONS as r (r)}
					<label class="flex items-center gap-1 text-xs">
						<input type="radio" name="reason" value={r} bind:group={reason} />
						{REASON_LABELS[r]}
					</label>
				{/each}
			</div>
		</fieldset>
		<label class="block text-xs">
			Additional context (optional)
			<textarea
				bind:value={body}
				maxlength={REPORT_BODY_MAX_LEN}
				rows="2"
				placeholder="Anything a moderator should know"
				class="mt-1 w-full rounded-md border border-neutral-300 p-2 text-sm focus-visible:ring-2 focus-visible:ring-primary-500 focus-visible:outline-none"
			></textarea>
		</label>
		{#if error}
			<p class="text-xs text-rose-600" role="alert">{error}</p>
		{/if}
		<div class="flex items-center gap-2">
			<button
				type="submit"
				disabled={inFlight}
				class="rounded-md bg-rose-600 px-3 py-1 text-xs text-white hover:bg-rose-700 disabled:cursor-not-allowed disabled:opacity-50"
			>
				Submit report
			</button>
			<button
				type="button"
				onclick={cancel}
				disabled={inFlight}
				class="text-xs underline underline-offset-2 hover:text-neutral-700 disabled:cursor-not-allowed disabled:opacity-50"
			>
				Cancel
			</button>
		</div>
	</form>
{/if}
