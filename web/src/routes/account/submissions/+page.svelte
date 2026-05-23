<script lang="ts">
	import { enhance } from '$app/forms';
	import { resolve } from '$app/paths';
	import type { ActionData, PageData } from './$types';
	import type { SubmissionStatus, SubmissionSummary } from '$lib/submissions/types';

	const { data, form }: { data: PageData; form: ActionData | null } = $props();

	const statusStyle: Record<SubmissionStatus, string> = {
		pending: 'border-amber-400/40 bg-amber-100/40 text-amber-900 dark:bg-amber-500/10',
		approved: 'border-emerald-400/40 bg-emerald-100/40 text-emerald-900 dark:bg-emerald-500/10',
		rejected: 'border-rose-400/40 bg-rose-100/40 text-rose-900 dark:bg-rose-500/10',
		withdrawn: 'border-border bg-muted/40 text-muted-foreground'
	};

	const formatDate = (iso: string): string => {
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		// Same locale + Asia/Taipei anchoring as the API keys page
		// (#4.6) — keeps SSR/hydration in lockstep without a
		// flicker when the client takes over.
		return d.toLocaleString('zh-TW', {
			year: 'numeric',
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit',
			timeZone: 'Asia/Taipei'
		});
	};

	const isPending = (row: SubmissionSummary): boolean => row.status === 'pending';
</script>

<svelte:head>
	<title>My submissions — Account · Taiwan Data Hub</title>
	<meta
		name="description"
		content="View the status of submissions you contributed to Taiwan Data Hub: pending, approved, rejected, withdrawn."
	/>
</svelte:head>

<section class="mx-auto max-w-4xl px-4 py-8 sm:px-6 lg:px-8">
	<header class="mb-8">
		<h1 class="text-2xl font-semibold tracking-tight">My submissions</h1>
		<p class="text-muted-foreground mt-2 text-sm">
			Track the moderation status of your contributions. Pending submissions can be withdrawn from
			this page; approved or rejected rows are final.
		</p>
	</header>

	{#if data.state === 'unauthenticated'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Please sign in</p>
			<p class="text-muted-foreground mt-2">
				You need an authenticated session to view your submissions.
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
	{:else if data.state === 'ok' && data.submissions.length === 0}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">No submissions yet.</p>
			<p class="text-muted-foreground mt-2">
				<a href={resolve('/submit')} class="underline underline-offset-2">Submit a contribution</a>
				to add a dataset, tool, connector, or playground.
			</p>
		</div>
	{:else if data.state === 'ok'}
		{#if form?.message}
			<p
				class="border-destructive/40 bg-destructive/10 text-destructive mb-4 rounded-md border p-3 text-sm"
				role="alert"
			>
				{form.message}
			</p>
		{/if}
		{#if form?.withdrew}
			<p class="border-border bg-muted/40 mb-4 rounded-md border p-3 text-sm" role="status">
				Submission withdrawn.
			</p>
		{/if}
		<ul class="space-y-3" data-testid="submissions">
			{#each data.submissions as row (row.id)}
				<li
					class="border-border bg-card flex flex-col gap-3 rounded-md border p-4 sm:flex-row sm:items-center sm:justify-between"
				>
					<div class="min-w-0 flex-1">
						<p class="flex items-center gap-2">
							<span class="text-muted-foreground text-xs capitalize">{row.kind}</span>
							<span
								class="inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {statusStyle[
									row.status
								]}">{row.status}</span
							>
						</p>
						<p class="mt-1 truncate font-medium" title={row.title}>{row.title}</p>
						<p class="text-muted-foreground mt-1 text-xs">
							Submitted {formatDate(row.created_at)} · id {row.id}
						</p>
						{#if row.status === 'rejected' && row.review_reason}
							<p class="text-muted-foreground mt-2 text-xs">
								Moderator note: {row.review_reason}
							</p>
						{/if}
					</div>
					{#if isPending(row)}
						<form method="POST" action="?/withdraw" use:enhance>
							<input type="hidden" name="id" value={row.id} />
							<button
								type="submit"
								class="border-border hover:bg-muted/40 focus-visible:ring-ring rounded-md border px-3 py-2 text-xs font-medium focus-visible:ring-2 focus-visible:outline-none"
								>Withdraw</button
							>
						</form>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</section>
