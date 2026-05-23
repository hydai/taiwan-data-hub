<!--
	/datasets/[id] — full dataset detail page.

	M2 #2.6 DoD: detail page with resources + Copy MCP config button.

	Layout:
	  - breadcrumb back to /domains/<domain> and /
	  - hero block: tier badge, name (zh-TW + en), description
	  - "MCP wiring" panel with the Copy button (route-local
	    _McpConfigButton.svelte)
	  - metadata grid: format / license / updated cadence / source
	  - resources list (downloads + API endpoints)
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import HeartButton from '$lib/bookmarks/HeartButton.svelte';
	import CommentThread from '$lib/comments/CommentThread.svelte';
	import MetaTags from '$lib/seo/MetaTags.svelte';
	import { cn } from '$lib/utils';
	import McpConfigButton from './_McpConfigButton.svelte';

	let { data } = $props();
	const dataset = $derived(data.dataset);

	// Tier indicator colors mirror /domains kind dots, but flip the
	// semantics: tier is data quality/popularity (gold > silver >
	// bronze), not a categorical kind.
	const TIER_STYLES: Record<typeof dataset.tier, { label: string; classes: string }> = {
		gold: { label: 'Gold', classes: 'bg-warning-50 text-warning-700 border-warning-500/30' },
		silver: { label: 'Silver', classes: 'bg-neutral-100 text-neutral-700 border-neutral-300' },
		bronze: { label: 'Bronze', classes: 'bg-danger-50 text-danger-700 border-danger-500/30' }
	};
</script>

<MetaTags
	title={dataset.name['zh-TW']}
	description={dataset.description['zh-TW']}
	schemaType="Dataset"
/>

<div class="mx-auto max-w-5xl px-4 py-10 sm:px-6 lg:px-8">
	<nav class="mb-6 text-sm text-neutral-500" aria-label="Breadcrumb">
		<ol class="flex flex-wrap items-center gap-1.5">
			<li>
				<a class="underline underline-offset-2 hover:text-primary-700" href={resolve('/')}>Home</a>
			</li>
			<li aria-hidden="true">/</li>
			<li>
				<a class="underline underline-offset-2 hover:text-primary-700" href={resolve('/domains')}
					>Domains</a
				>
			</li>
			<li aria-hidden="true">/</li>
			<li>
				<a
					class="underline underline-offset-2 hover:text-primary-700"
					href={resolve('/domains/[slug]', { slug: dataset.domain_slug })}
				>
					{dataset.domain_slug}
				</a>
			</li>
			<li aria-hidden="true">/</li>
			<li class="font-mono text-neutral-700">{dataset.slug}</li>
		</ol>
	</nav>

	<header class="mb-8 border-b border-neutral-200 pb-6">
		<div class="mb-3 flex flex-wrap items-center gap-2">
			<span
				class={cn(
					'inline-flex items-center rounded-full border px-2.5 py-0.5 text-xs font-semibold',
					TIER_STYLES[dataset.tier].classes
				)}
			>
				{TIER_STYLES[dataset.tier].label}
			</span>
			<span class="rounded-full bg-neutral-100 px-2.5 py-0.5 text-xs font-medium text-neutral-700">
				{dataset.format}
			</span>
			<span class="text-xs text-neutral-500">
				Updates {dataset.updated}
			</span>
		</div>

		<div class="flex items-start gap-3">
			<h1 class="flex-1 text-3xl font-bold tracking-tight text-neutral-900">
				{dataset.name['zh-TW']}
			</h1>
			{#if data.communityEnabled}
				<HeartButton
					targetKind="dataset"
					targetId={data.commentTargetId}
					currentUserId={data.currentUserId}
					bookmarked={data.bookmarked}
					size="md"
				/>
			{/if}
		</div>
		{#if dataset.name.en}
			<p class="mt-1 text-base text-neutral-500">{dataset.name.en}</p>
		{/if}

		<p class="mt-4 max-w-3xl text-base leading-relaxed text-neutral-700">
			{dataset.description['zh-TW']}
		</p>
		{#if dataset.description.en}
			<p class="mt-2 max-w-3xl text-sm text-neutral-500 italic">
				{dataset.description.en}
			</p>
		{/if}
	</header>

	<section class="mb-8" aria-label="MCP wiring">
		<McpConfigButton datasetSlug={dataset.slug} />
	</section>

	<section class="mb-8 grid grid-cols-1 gap-4 sm:grid-cols-2" aria-label="Dataset metadata">
		<dl class="rounded-lg border border-neutral-200 bg-white p-5">
			<dt class="text-xs font-semibold tracking-wide text-neutral-500 uppercase">Publisher</dt>
			<dd class="mt-1 text-sm text-neutral-900">{dataset.source.publisher}</dd>
			<dd class="mt-1">
				<a
					class="text-sm break-all text-primary-700 underline underline-offset-2 hover:text-primary-800"
					href={dataset.source.url}
					target="_blank"
					rel="noopener noreferrer"
				>
					{dataset.source.url}
				</a>
			</dd>
		</dl>

		<dl class="rounded-lg border border-neutral-200 bg-white p-5">
			<dt class="text-xs font-semibold tracking-wide text-neutral-500 uppercase">License</dt>
			<dd class="mt-1 text-sm text-neutral-900">{dataset.license}</dd>
			<dt class="mt-4 text-xs font-semibold tracking-wide text-neutral-500 uppercase">
				Update cadence
			</dt>
			<dd class="mt-1 text-sm text-neutral-900">{dataset.updated}</dd>
		</dl>
	</section>

	<section aria-labelledby="resources-heading">
		<h2 id="resources-heading" class="mb-3 text-lg font-semibold text-neutral-900">Resources</h2>
		<ul
			role="list"
			class="divide-y divide-neutral-200 rounded-lg border border-neutral-200 bg-white"
		>
			{#each dataset.resources as resource (resource.url)}
				<li class="flex items-center gap-3 px-5 py-3">
					<span
						class={cn(
							'inline-flex items-center rounded px-2 py-0.5 text-xs font-medium',
							resource.kind === 'api'
								? 'bg-info-50 text-info-700'
								: 'bg-success-50 text-success-700'
						)}
					>
						{resource.kind}
					</span>
					<a
						class="flex-1 text-sm text-primary-700 underline underline-offset-2 hover:text-primary-800"
						href={resource.url}
						target="_blank"
						rel="noopener noreferrer"
					>
						{resource.label}
					</a>
					<span class="hidden truncate font-mono text-xs text-neutral-500 sm:inline">
						{resource.url}
					</span>
				</li>
			{/each}
		</ul>
	</section>

	{#if data.communityEnabled}
		<CommentThread
			targetKind="dataset"
			targetId={data.commentTargetId}
			currentUserId={data.currentUserId}
		/>
	{/if}
</div>
