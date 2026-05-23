<script lang="ts">
	import { enhance } from '$app/forms';
	import { resolve } from '$app/paths';
	import type { ActionData, PageData } from './$types';
	import { SUBMISSION_KINDS, type SubmissionKind } from '$lib/submissions/types';

	const { data, form }: { data: PageData; form: ActionData | null } = $props();

	// The action returns a discriminated union. Narrow it once
	// up front so the template branches don't have to repeat
	// the `in` checks — `created` on success, `message` + the
	// optional `kind`/`values` snapshot on failure.
	const failure = $derived(
		form !== null && form !== undefined && 'message' in form
			? (form as { message: string; kind?: SubmissionKind; values?: Record<string, string> })
			: null
	);
	const created = $derived(
		form !== null && form !== undefined && 'created' in form ? form.created : null
	);

	// Multi-step state. Step 1 = pick a kind, step 2 = fill
	// per-kind fields, step 3 = success view. `step` + `kind`
	// are tracked separately so a "change type" click only
	// switches the form schema without unmounting the page.
	// Field values entered on step 2 are NOT preserved across
	// a back-to-step-1 navigation — once the user clicks
	// "change type", the next render mounts a fresh form. The
	// only path that re-populates step 2 inputs is the action
	// failure branch, which echoes `failure.values` from the
	// server snapshot.
	let step = $state<1 | 2 | 3>(1);
	let kind = $state<SubmissionKind | null>(null);

	// Replay the snapshot only when the failure corresponds to
	// the currently-selected kind. Without the kind-match guard
	// a "change type" click would prefill overlapping fields
	// (description, repo_url, …) from the prior kind's snapshot,
	// which contradicts the "fresh form on type change" intent.
	const values = $derived<Record<string, string>>(
		failure?.kind === kind ? (failure?.values ?? {}) : {}
	);

	// Sync local step + kind with the action return so a failed
	// submit lands back on step 2 with the user's prior input,
	// and a successful submit auto-progresses to step 3. Using
	// `$effect` (rather than a reactive `$state` initializer)
	// is the Svelte 5-idiomatic way to react to a prop change
	// without capturing only the initial value.
	$effect(() => {
		if (created) {
			step = 3;
		} else if (failure?.kind) {
			step = 2;
			kind = failure.kind;
		}
	});

	const kindLabels: Record<SubmissionKind, { title: string; blurb: string }> = {
		dataset: {
			title: 'Dataset',
			blurb: 'Public data source that should join the catalog (Open Data, CSV, API…).'
		},
		tool: {
			title: 'Tool',
			blurb: 'Custom MCP tool you want to register for AI agents to call.'
		},
		connector: {
			title: 'Connector',
			blurb: 'New `SourceConnector` impl pulling fresh data into the hub.'
		},
		playground: {
			title: 'Playground',
			blurb: 'Live demo or notebook showcasing a query/visualization.'
		}
	};

	function pickKind(k: SubmissionKind): void {
		kind = k;
		step = 2;
	}

	function backToStepOne(): void {
		step = 1;
	}
</script>

<svelte:head>
	<title>Submit — Taiwan Data Hub</title>
	<meta
		name="description"
		content="Contribute a dataset, tool, connector, or playground to Taiwan Data Hub for moderation."
	/>
</svelte:head>

<section class="mx-auto max-w-3xl px-4 py-8 sm:px-6 lg:px-8">
	<header class="mb-8">
		<h1 class="text-2xl font-semibold tracking-tight">Contribute to Taiwan Data Hub</h1>
		<p class="text-muted-foreground mt-2 text-sm">
			Submissions land in moderation and become public once a curator approves. Pick the type that
			best matches your contribution to get the right fields.
		</p>
	</header>

	{#if data.state === 'unauthenticated'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Please sign in to submit</p>
			<p class="text-muted-foreground mt-2">
				You need an authenticated session to contribute. Sign in from the home page and return to
				this form.
			</p>
		</div>
	{:else if data.state === 'unavailable' || data.state === 'unexpected'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Submissions are currently unavailable</p>
			<p class="text-muted-foreground mt-2">{data.message}</p>
		</div>
	{:else if step === 1}
		{#if failure?.message}
			<p
				class="border-destructive/40 bg-destructive/10 text-destructive mb-4 rounded-md border p-3 text-sm"
				role="alert"
			>
				{failure.message}
			</p>
		{/if}
		<noscript>
			<p
				class="border-border bg-muted/40 mb-4 rounded-md border p-3 text-sm"
				data-testid="no-js-notice"
			>
				The submission wizard requires JavaScript to switch between the four kinds. Enable
				JavaScript and refresh, or contact a maintainer to file the submission directly.
			</p>
		</noscript>
		<ol class="grid gap-3" data-testid="submission-kinds">
			{#each SUBMISSION_KINDS as k (k)}
				<li>
					<button
						type="button"
						class="border-border hover:bg-muted/40 focus-visible:ring-ring w-full rounded-md border p-4 text-left transition focus-visible:ring-2 focus-visible:outline-none"
						onclick={() => pickKind(k)}
						data-kind={k}
					>
						<span class="block text-base font-medium capitalize">{kindLabels[k].title}</span>
						<span class="text-muted-foreground mt-1 block text-sm">{kindLabels[k].blurb}</span>
					</button>
				</li>
			{/each}
		</ol>
	{:else if step === 2 && kind !== null}
		<form
			method="POST"
			action="?/create"
			use:enhance
			class="space-y-6"
			data-testid="submission-form"
		>
			<input type="hidden" name="kind" value={kind} />
			<div
				class="border-border bg-muted/30 flex items-center justify-between rounded-md border p-3 text-sm"
			>
				<span>
					Submitting a <strong class="capitalize">{kindLabels[kind].title}</strong>
				</span>
				<button
					type="button"
					class="text-muted-foreground hover:text-foreground underline-offset-2 hover:underline"
					onclick={backToStepOne}>change type</button
				>
			</div>

			{#if failure?.message}
				<p
					class="border-destructive/40 bg-destructive/10 text-destructive rounded-md border p-3 text-sm"
					role="alert"
				>
					{failure.message}
				</p>
			{/if}

			{#if kind === 'dataset'}
				<label class="block">
					<span class="text-sm font-medium">Dataset title</span>
					<input
						name="title"
						required
						maxlength="120"
						value={values.title ?? ''}
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Source URL</span>
					<input
						name="source_url"
						type="url"
						required
						maxlength="2048"
						value={values.source_url ?? ''}
						placeholder="https://data.gov.tw/dataset/…"
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">License</span>
					<input
						name="license"
						required
						maxlength="120"
						value={values.license ?? ''}
						placeholder="CC-BY-4.0 / 政府資料開放授權條款 / …"
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Domain slug</span>
					<input
						name="domain_slug"
						required
						maxlength="120"
						pattern="[A-Za-z0-9_\-]+"
						value={values.domain_slug ?? ''}
						placeholder="weather-climate"
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
					<span class="text-muted-foreground mt-1 block text-xs"
						>ASCII letters, digits, "-" and "_" only — moderator will reconcile to an existing
						domain.</span
					>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Description</span>
					<textarea
						name="description"
						required
						maxlength="2048"
						rows="4"
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
						>{values.description ?? ''}</textarea
					>
				</label>
			{:else if kind === 'tool'}
				<label class="block">
					<span class="text-sm font-medium">Tool name</span>
					<input
						name="name"
						required
						maxlength="120"
						value={values.name ?? ''}
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Repository URL</span>
					<input
						name="repo_url"
						type="url"
						required
						maxlength="2048"
						value={values.repo_url ?? ''}
						placeholder="https://github.com/you/your-tool"
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Implementation language</span>
					<input
						name="language"
						required
						maxlength="120"
						value={values.language ?? ''}
						placeholder="rust"
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Description</span>
					<textarea
						name="description"
						required
						maxlength="2048"
						rows="4"
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
						>{values.description ?? ''}</textarea
					>
				</label>
			{:else if kind === 'connector'}
				<label class="block">
					<span class="text-sm font-medium">Connector name</span>
					<input
						name="name"
						required
						maxlength="120"
						value={values.name ?? ''}
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Repository URL</span>
					<input
						name="repo_url"
						type="url"
						required
						maxlength="2048"
						value={values.repo_url ?? ''}
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">License</span>
					<input
						name="license"
						required
						maxlength="120"
						value={values.license ?? ''}
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Description</span>
					<textarea
						name="description"
						required
						maxlength="2048"
						rows="4"
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
						>{values.description ?? ''}</textarea
					>
				</label>
			{:else if kind === 'playground'}
				<label class="block">
					<span class="text-sm font-medium">Playground name</span>
					<input
						name="name"
						required
						maxlength="120"
						value={values.name ?? ''}
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Demo URL</span>
					<input
						name="demo_url"
						type="url"
						required
						maxlength="2048"
						value={values.demo_url ?? ''}
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Repository URL (optional)</span>
					<input
						name="repo_url"
						type="url"
						maxlength="2048"
						value={values.repo_url ?? ''}
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<label class="block">
					<span class="text-sm font-medium">Description</span>
					<textarea
						name="description"
						required
						maxlength="2048"
						rows="4"
						class="border-border focus-visible:ring-ring mt-1 block w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
						>{values.description ?? ''}</textarea
					>
				</label>
			{/if}

			<div class="flex items-center gap-3">
				<button
					type="submit"
					class="bg-primary text-primary-foreground hover:bg-primary/90 focus-visible:ring-ring rounded-md px-4 py-2 text-sm font-medium focus-visible:ring-2 focus-visible:outline-none"
					>Submit for moderation</button
				>
				<a
					href={resolve('/account/submissions')}
					class="text-muted-foreground hover:text-foreground text-sm underline-offset-2 hover:underline"
					>View my submissions</a
				>
			</div>
		</form>
	{:else if step === 3 && created}
		<div class="border-border bg-muted/30 space-y-3 rounded-md border p-6 text-sm" role="status">
			<p class="text-base font-medium">Thanks — your submission is in moderation.</p>
			<p class="text-muted-foreground">
				Submission id: <code class="text-foreground bg-background rounded px-1.5 py-0.5"
					>{created.id}</code
				>
			</p>
			<p class="text-muted-foreground">
				Track its status on
				<a class="underline underline-offset-2" href={resolve('/account/submissions')}
					>your submissions page</a
				>, or
				<a class="underline underline-offset-2" href={resolve('/submit')} data-sveltekit-reload
					>submit another</a
				>.
			</p>
		</div>
	{/if}
</section>
