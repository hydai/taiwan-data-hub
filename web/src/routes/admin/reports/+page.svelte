<script lang="ts">
	import { enhance } from '$app/forms';
	import { REASON_LABELS, REPORT_ACTIONS, type ReportAction } from '$lib/reports/types';
	import type { ActionData, PageData } from './$types';

	let { data, form }: { data: PageData; form: ActionData } = $props();

	// Per-row "which action did the moderator pick before
	// submitting?" so the radio group remembers state when
	// the form re-renders after an action.
	let pendingActions = $state<Record<string, ReportAction>>({});

	function actionFor(id: string): ReportAction {
		return pendingActions[id] ?? 'keep';
	}

	function formatDate(iso: string): string {
		try {
			return new Date(iso).toLocaleString();
		} catch {
			return iso;
		}
	}
</script>

<svelte:head>
	<title>Reports queue · Taiwan Data Hub</title>
</svelte:head>

<section class="mx-auto max-w-4xl space-y-6 px-4 py-8">
	<header>
		<h1 class="text-2xl font-bold text-neutral-900">Reports queue</h1>
		<p class="mt-1 text-sm text-neutral-600">
			Open reports, oldest first. Pick an action and submit to disposition.
		</p>
	</header>

	{#if data.state === 'unauthenticated'}
		<p class="rounded-md border border-amber-300 bg-amber-50 p-4 text-sm text-amber-900">
			Please sign in to access the moderator queue.
		</p>
	{:else if data.state === 'forbidden'}
		<p class="rounded-md border border-rose-300 bg-rose-50 p-4 text-sm text-rose-900">
			You don't have the moderator role required to view this queue.
		</p>
	{:else if data.state === 'unavailable'}
		<p class="rounded-md border border-amber-300 bg-amber-50 p-4 text-sm text-amber-900">
			{data.message}
		</p>
	{:else if data.state === 'unexpected'}
		<p class="rounded-md border border-rose-300 bg-rose-50 p-4 text-sm text-rose-900">
			{data.message}
		</p>
	{:else if data.state === 'ok' && data.reports.length === 0}
		<p
			class="rounded-md border border-neutral-200 bg-neutral-50 p-6 text-center text-sm text-neutral-600"
		>
			Queue empty — no open reports right now.
		</p>
	{:else if data.state === 'ok'}
		{#if form?.resolved}
			<p
				class="rounded-md border border-emerald-300 bg-emerald-50 p-3 text-sm text-emerald-900"
				role="status"
			>
				Resolved report {form.resolved.id} as “{form.resolved.action}”.
			</p>
		{:else if form?.message}
			<p
				class="rounded-md border border-rose-300 bg-rose-50 p-3 text-sm text-rose-900"
				role="alert"
			>
				{form.message}
			</p>
		{/if}

		<ol class="space-y-4">
			{#each data.reports as report (report.id)}
				<li class="rounded-md border border-neutral-200 bg-white p-4">
					<header
						class="flex flex-wrap items-center justify-between gap-2 text-xs text-neutral-600"
					>
						<span>
							<strong class="text-neutral-900">{REASON_LABELS[report.reason]}</strong>
							· {report.target_kind}
							<code class="rounded bg-neutral-100 px-1 py-0.5 text-neutral-700"
								>{report.target_id}</code
							>
						</span>
						<time>{formatDate(report.created_at)}</time>
					</header>
					{#if report.body}
						<p class="mt-2 text-sm whitespace-pre-wrap text-neutral-700">{report.body}</p>
					{:else}
						<p class="mt-2 text-sm text-neutral-500 italic">No additional context from reporter.</p>
					{/if}
					<form
						method="POST"
						action="?/resolve"
						use:enhance
						class="mt-3 flex flex-wrap items-end gap-3 border-t border-neutral-100 pt-3"
					>
						<input type="hidden" name="id" value={report.id} />
						<fieldset class="text-xs">
							<legend class="font-medium text-neutral-700">Action</legend>
							<div class="mt-1 flex gap-2">
								{#each REPORT_ACTIONS as a (a)}
									<label class="flex items-center gap-1">
										<input
											type="radio"
											name="action"
											value={a}
											checked={actionFor(report.id) === a}
											onchange={() => (pendingActions[report.id] = a)}
										/>
										{a.replace('_', ' ')}
									</label>
								{/each}
							</div>
						</fieldset>
						<label class="flex-1 text-xs">
							Note (optional)
							<input
								name="resolution_note"
								placeholder="Internal note for the audit log"
								class="mt-1 w-full rounded-md border border-neutral-300 p-1 text-sm focus-visible:ring-2 focus-visible:ring-primary-500 focus-visible:outline-none"
							/>
						</label>
						<button
							type="submit"
							class="rounded-md bg-neutral-900 px-3 py-1 text-xs text-white hover:bg-neutral-800"
						>
							Resolve
						</button>
					</form>
				</li>
			{/each}
		</ol>
	{/if}
</section>
