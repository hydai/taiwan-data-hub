<script lang="ts">
	import { enhance } from '$app/forms';
	import { resolve } from '$app/paths';
	import type { ActionData, PageData } from './$types';
	import { SUBMISSION_KINDS, type SubmissionKind } from '$lib/submissions/types';
	import type { ModerationSubmission } from '$lib/moderation/types';

	const { data, form }: { data: PageData; form: ActionData | null } = $props();

	const formatDate = (iso: string): string => {
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		return d.toLocaleString('zh-TW', {
			year: 'numeric',
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit',
			timeZone: 'Asia/Taipei'
		});
	};

	const kindLabel: Record<SubmissionKind, string> = {
		dataset: 'Dataset',
		tool: 'Tool',
		connector: 'Connector',
		playground: 'Playground'
	};

	// One reject-input row at a time keeps the UI legible.
	// Tracks the submission id whose reject form is expanded.
	let rejectOpen = $state<string | null>(null);
	function toggleReject(id: string): void {
		rejectOpen = rejectOpen === id ? null : id;
	}

	function payloadEntries(row: ModerationSubmission): [string, string][] {
		const out: [string, string][] = [];
		for (const [k, v] of Object.entries(row.payload)) {
			if (k === 'kind') continue;
			if (typeof v === 'string') out.push([k, v]);
			else if (v === null) out.push([k, '(none)']);
			else out.push([k, JSON.stringify(v)]);
		}
		return out;
	}
</script>

<svelte:head>
	<title>Moderation queue — Admin · Taiwan Data Hub</title>
	<meta name="description" content="Approve or reject pending community submissions." />
</svelte:head>

<section class="mx-auto max-w-4xl px-4 py-8 sm:px-6 lg:px-8">
	<header class="mb-8">
		<h1 class="text-2xl font-semibold tracking-tight">Moderation queue</h1>
		<p class="text-muted-foreground mt-2 text-sm">
			Review pending community submissions. Approve to publish; reject with a reason. Every decision
			is appended to the audit log.
		</p>
	</header>

	{#if data.state === 'unauthenticated'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Please sign in</p>
			<p class="text-muted-foreground mt-2">A moderator session is required.</p>
		</div>
	{:else if data.state === 'forbidden'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Moderator role required</p>
			<p class="text-muted-foreground mt-2">
				Your account does not have permission to access the moderation queue.
			</p>
		</div>
	{:else if data.state === 'unavailable'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Service temporarily unavailable</p>
			<p class="text-muted-foreground mt-2">{data.message}</p>
		</div>
	{:else if data.state === 'unexpected'}
		<div
			class="border-destructive/40 bg-destructive/10 text-destructive rounded-md border p-6 text-sm"
		>
			<p class="font-medium">Something went wrong</p>
			<p class="mt-2">{data.message}</p>
		</div>
	{:else if data.state === 'ok'}
		<nav class="mb-4 flex flex-wrap items-center gap-2 text-sm" aria-label="Kind filter">
			<span class="text-muted-foreground">Filter:</span>
			<a
				class="border-border rounded-full border px-2 py-1 {data.kindFilter === null
					? 'bg-muted/50'
					: ''}"
				href={resolve('/admin/moderation')}>All</a
			>
			{#each SUBMISSION_KINDS as k (k)}
				<a
					class="border-border rounded-full border px-2 py-1 capitalize {data.kindFilter === k
						? 'bg-muted/50'
						: ''}"
					href={resolve(`/admin/moderation?kind=${k}`)}>{kindLabel[k]}</a
				>
			{/each}
		</nav>
		{#if form?.message}
			<p
				class="border-destructive/40 bg-destructive/10 text-destructive mb-4 rounded-md border p-3 text-sm"
				role="alert"
			>
				{form.message}
			</p>
		{/if}
		{#if form?.decided}
			<p class="border-border bg-muted/40 mb-4 rounded-md border p-3 text-sm" role="status">
				Decision recorded: submission {form.decided.id} → {form.decided.status}.
			</p>
		{/if}
		{#if data.submissions.length === 0}
			<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
				<p class="font-medium">Queue empty.</p>
				<p class="text-muted-foreground mt-2">
					No pending submissions{data.kindFilter ? ` of kind ${data.kindFilter}` : ''}.
				</p>
			</div>
		{:else}
			<ul class="space-y-3" data-testid="moderation-queue">
				{#each data.submissions as row (row.id)}
					<li class="border-border bg-card rounded-md border p-4">
						<div class="flex flex-wrap items-center justify-between gap-2">
							<div>
								<p class="flex items-center gap-2 text-xs">
									<span class="text-muted-foreground capitalize">{row.kind}</span>
									<span class="border-border rounded-full border px-2 py-0.5">{row.status}</span>
								</p>
								<p class="mt-1 font-medium" title={row.title}>{row.title}</p>
								<p class="text-muted-foreground mt-1 text-xs">
									Submitted {formatDate(row.created_at)} by {row.user_id} · id {row.id}
								</p>
							</div>
							<div class="flex items-center gap-2">
								<form method="POST" action="?/approve" use:enhance>
									<input type="hidden" name="id" value={row.id} />
									<button
										type="submit"
										class="border-border hover:bg-muted/40 focus-visible:ring-ring rounded-md border px-3 py-2 text-xs font-medium focus-visible:ring-2 focus-visible:outline-none"
										>Approve</button
									>
								</form>
								<button
									type="button"
									onclick={() => toggleReject(row.id)}
									class="border-border hover:bg-muted/40 focus-visible:ring-ring rounded-md border px-3 py-2 text-xs font-medium focus-visible:ring-2 focus-visible:outline-none"
									>Reject…</button
								>
							</div>
						</div>
						<dl class="mt-3 grid grid-cols-1 gap-1 text-sm sm:grid-cols-2">
							{#each payloadEntries(row) as [k, v] (k)}
								<div class="contents">
									<dt class="text-muted-foreground text-xs tracking-wide uppercase">{k}</dt>
									<dd class="break-words">{v}</dd>
								</div>
							{/each}
						</dl>
						{#if rejectOpen === row.id}
							<form
								method="POST"
								action="?/reject"
								use:enhance
								class="mt-3 flex flex-col gap-2 sm:flex-row"
							>
								<input type="hidden" name="id" value={row.id} />
								<input
									name="reason"
									required
									placeholder="Reason (required)"
									class="border-border focus-visible:ring-ring flex-1 rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
								/>
								<button
									type="submit"
									class="bg-destructive text-destructive-foreground hover:bg-destructive/90 focus-visible:ring-ring rounded-md px-3 py-2 text-sm font-medium focus-visible:ring-2 focus-visible:outline-none"
									>Confirm reject</button
								>
							</form>
						{/if}
					</li>
				{/each}
			</ul>
		{/if}
	{/if}
</section>
