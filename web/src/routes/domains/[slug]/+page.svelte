<!--
	/domains/[slug] — per-domain page.

	M2 #2.5 DoD: scope, typical questions, anchor datasets, then
	paginated dataset list with URL-driven tier/format/license
	filters that restore on reload. Empty state and loading skeleton.

	Filter pattern: every pill is a plain <a href={?…}> link. The
	URL is the single source of truth for filter state — bookmarkable,
	shareable, survives reload, and works with no JS. SvelteKit's
	client-side router upgrades the navigation transparently.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import { navigating } from '$app/state';
	import MetaTags from '$lib/seo/MetaTags.svelte';
	import { buildFilterUrl } from '$lib/datasets/filter';
	import { cn } from '$lib/utils';
	import FilterPill from './_FilterPill.svelte';
	import DatasetRow from './_DatasetRow.svelte';

	let { data } = $props();

	const KIND_LABEL: Record<typeof data.domain.kind, string> = {
		topical: 'Topical',
		meta: 'Meta',
		horizontal: 'Horizontal'
	};

	// The list dims while a filter / page link is in flight so the
	// user gets immediate visual feedback (DoD: "loading skeleton").
	// `$app/state`'s `navigating` is a reactive singleton with
	// nullable getters; `Boolean(navigating?.to)` is true only while
	// a route transition is in flight. The optional chain is
	// defensive — the singleton is always defined per the API, but
	// the `?.` survives any future API change that switches to
	// `Navigating | null`.
	const isLoading = $derived(Boolean(navigating?.to));

	const hasActiveFilters = $derived(
		data.filters.tier !== null || data.filters.format !== null || data.filters.license !== null
	);
</script>

<MetaTags
	title={data.domain.name['zh-TW']}
	description={data.domain.description?.['zh-TW'] ??
		`Datasets in the ${data.domain.slug} domain on Taiwan Data Hub.`}
	schemaType="CollectionPage"
/>

<div class="mx-auto max-w-7xl px-4 py-10 sm:px-6 lg:px-8">
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
			<li class="font-mono text-neutral-700">{data.domain.slug}</li>
		</ol>
	</nav>

	<header class="mb-10 border-b border-neutral-200 pb-6">
		<div class="mb-3 flex items-center gap-2">
			<span class="rounded-full bg-neutral-100 px-2.5 py-0.5 text-xs font-medium text-neutral-700">
				{KIND_LABEL[data.domain.kind]}
			</span>
		</div>
		<h1 class="text-4xl font-bold tracking-tight text-neutral-900">
			{data.domain.name['zh-TW']}
		</h1>
		{#if data.domain.name.en}
			<p class="mt-1 text-base text-neutral-500">{data.domain.name.en}</p>
		{/if}
		{#if data.domain.description?.['zh-TW']}
			<p class="mt-4 max-w-3xl text-lg text-neutral-600">
				{data.domain.description['zh-TW']}
			</p>
		{/if}
	</header>

	{#if data.domain.typical_questions && data.domain.typical_questions.length > 0}
		<section class="mb-10" aria-labelledby="typical-questions-heading">
			<h2 id="typical-questions-heading" class="mb-3 text-lg font-semibold text-neutral-900">
				Typical questions
			</h2>
			<ul role="list" class="space-y-2">
				{#each data.domain.typical_questions as q (q['zh-TW'])}
					<li
						class="rounded-md border border-neutral-200 bg-neutral-50 px-4 py-3 text-sm text-neutral-700"
					>
						<span class="font-medium text-neutral-900">{q['zh-TW']}</span>
						{#if q.en}
							<span class="ml-2 text-neutral-500">— {q.en}</span>
						{/if}
					</li>
				{/each}
			</ul>
		</section>
	{/if}

	{#if data.anchors.length > 0}
		<section class="mb-10" aria-labelledby="anchor-heading">
			<h2 id="anchor-heading" class="mb-3 text-lg font-semibold text-neutral-900">
				Anchor datasets
			</h2>
			<ul role="list" class="grid grid-cols-1 gap-4 md:grid-cols-3">
				{#each data.anchors as anchor (anchor.slug)}
					<li><DatasetRow dataset={anchor} /></li>
				{/each}
			</ul>
		</section>
	{/if}

	<section aria-labelledby="datasets-heading">
		<div
			class="mb-4 flex flex-wrap items-baseline justify-between gap-3 border-b border-neutral-200 pb-3"
		>
			<h2 id="datasets-heading" class="text-lg font-semibold text-neutral-900">All datasets</h2>
			<span class="text-xs text-neutral-500">
				{data.filteredCount}
				of
				{data.totalDatasets}
				{data.totalDatasets === 1 ? 'dataset' : 'datasets'}
			</span>
		</div>

		<!-- Filter bar. Each pill is a plain <a href={?…}>; the URL
		     is the filter state. Clicking an active pill clears it. -->
		<div class="mb-6 space-y-3" aria-label="Filters">
			{#if data.facets.tiers.length > 0}
				<div class="flex flex-wrap items-center gap-2">
					<span class="text-xs font-semibold tracking-wide text-neutral-500 uppercase">Tier</span>
					{#each data.facets.tiers as tier (tier)}
						{@const active = data.filters.tier === tier}
						<FilterPill
							href={buildFilterUrl(data.filters, { tier: active ? null : tier, page: 1 })}
							label={tier}
							{active}
						/>
					{/each}
				</div>
			{/if}
			{#if data.facets.formats.length > 0}
				<div class="flex flex-wrap items-center gap-2">
					<span class="text-xs font-semibold tracking-wide text-neutral-500 uppercase">
						Format
					</span>
					{#each data.facets.formats as format (format)}
						{@const active = data.filters.format === format}
						<FilterPill
							href={buildFilterUrl(data.filters, { format: active ? null : format, page: 1 })}
							label={format}
							{active}
						/>
					{/each}
				</div>
			{/if}
			{#if data.facets.licenses.length > 0}
				<div class="flex flex-wrap items-center gap-2">
					<span class="text-xs font-semibold tracking-wide text-neutral-500 uppercase">
						License
					</span>
					{#each data.facets.licenses as license (license)}
						{@const active = data.filters.license === license}
						<FilterPill
							href={buildFilterUrl(data.filters, { license: active ? null : license, page: 1 })}
							label={license}
							{active}
						/>
					{/each}
				</div>
			{/if}
			{#if hasActiveFilters}
				<div>
					<a
						href={resolve('/domains/[slug]', { slug: data.domain.slug })}
						class="text-xs text-primary-700 underline underline-offset-2 hover:text-primary-800"
					>
						Clear all filters
					</a>
				</div>
			{/if}
		</div>

		{#if data.datasets.length === 0}
			<div
				class="rounded-lg border border-dashed border-neutral-300 bg-neutral-50 px-6 py-12 text-center"
			>
				<p class="text-sm text-neutral-600">No datasets match the current filters.</p>
				{#if hasActiveFilters}
					<a
						href={resolve('/domains/[slug]', { slug: data.domain.slug })}
						class="mt-2 inline-block text-sm text-primary-700 underline underline-offset-2 hover:text-primary-800"
					>
						Clear all filters
					</a>
				{/if}
			</div>
		{:else}
			<ul
				role="list"
				class={cn(
					'grid grid-cols-1 gap-4 transition-opacity sm:grid-cols-2 lg:grid-cols-3',
					isLoading && 'pointer-events-none opacity-50'
				)}
				aria-busy={isLoading}
			>
				{#each data.datasets as dataset (dataset.slug)}
					<li><DatasetRow {dataset} /></li>
				{/each}
			</ul>
		{/if}

		{#if data.totalPages > 1}
			<!--
				Pagination hrefs are relative query-string-only URLs
				(`?tier=…&page=N`) composed by `buildFilterUrl`. resolve()
				is for full paths; query-only navigation against the
				current page doesn't need base-path resolution.
			-->
			<!-- eslint-disable svelte/no-navigation-without-resolve -->
			<nav class="mt-8 flex items-center justify-between" aria-label="Pagination">
				{#if data.page > 1}
					<a
						href={buildFilterUrl(data.filters, { page: data.page - 1 })}
						class="rounded-md border border-neutral-200 bg-white px-3 py-1.5 text-sm text-neutral-700 hover:bg-neutral-100 focus:ring-2 focus:ring-primary-500 focus:outline-none"
					>
						← Previous
					</a>
				{:else}
					<span></span>
				{/if}
				<span class="text-xs text-neutral-500">
					Page {data.page} of {data.totalPages}
				</span>
				{#if data.page < data.totalPages}
					<a
						href={buildFilterUrl(data.filters, { page: data.page + 1 })}
						class="rounded-md border border-neutral-200 bg-white px-3 py-1.5 text-sm text-neutral-700 hover:bg-neutral-100 focus:ring-2 focus:ring-primary-500 focus:outline-none"
					>
						Next →
					</a>
				{:else}
					<span></span>
				{/if}
			</nav>
			<!-- eslint-enable svelte/no-navigation-without-resolve -->
		{/if}
	</section>
</div>
