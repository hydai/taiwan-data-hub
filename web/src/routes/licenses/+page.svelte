<!--
	/licenses — enumeration of every license in use across the dataset
	corpus, with counts and (when published) a link to the license
	document. Companion to the per-dataset license link added on
	/datasets/[id] in #5b.6.

	Layout:
	  - breadcrumb back to /
	  - summary: total licenses + total datasets covered
	  - per-license card: name, optional URL, dataset count, list
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import MetaTags from '$lib/seo/MetaTags.svelte';

	let { data } = $props();
	const licenses = $derived(data.licenses);
	const totalDatasets = $derived(licenses.reduce((sum, l) => sum + l.datasets.length, 0));
</script>

<MetaTags
	title="Licenses"
	description="Every license currently in use across the Taiwan Data Hub dataset corpus."
/>

<div class="mx-auto max-w-5xl px-4 py-10 sm:px-6 lg:px-8">
	<nav class="mb-6 text-sm text-neutral-500" aria-label="Breadcrumb">
		<ol class="flex flex-wrap items-center gap-1.5">
			<li>
				<a class="underline underline-offset-2 hover:text-primary-700" href={resolve('/')}>Home</a>
			</li>
			<li aria-hidden="true">/</li>
			<li class="font-mono text-neutral-700">licenses</li>
		</ol>
	</nav>

	<header class="mb-8 border-b border-neutral-200 pb-6">
		<h1 class="text-3xl font-semibold text-neutral-900">Licenses</h1>
		<p class="mt-2 max-w-3xl text-sm text-neutral-600">
			{licenses.length}
			{licenses.length === 1 ? 'license' : 'licenses'} cover the
			{totalDatasets}
			{totalDatasets === 1 ? 'dataset' : 'datasets'} surfaced by Taiwan Data Hub. Each card below links
			to the canonical license document when the upstream publishes one.
		</p>
	</header>

	<section aria-labelledby="licenses-heading">
		<h2 id="licenses-heading" class="sr-only">License groups</h2>
		<ul role="list" class="space-y-4">
			{#each licenses as license (license.name)}
				<li
					class="rounded-lg border border-neutral-200 bg-white p-5 transition-shadow hover:shadow-sm"
				>
					<div class="flex flex-wrap items-baseline justify-between gap-2">
						<h3 class="text-lg font-semibold text-neutral-900">{license.name}</h3>
						<span class="text-sm text-neutral-500">
							{license.datasets.length}
							{license.datasets.length === 1 ? 'dataset' : 'datasets'}
						</span>
					</div>
					{#if license.url}
						<p class="mt-1">
							<a
								class="text-sm break-all text-primary-700 underline underline-offset-2 hover:text-primary-800"
								href={license.url}
								target="_blank"
								rel="noopener noreferrer"
							>
								{license.url}
							</a>
						</p>
					{:else}
						<p class="mt-1 text-sm text-neutral-500 italic">
							No canonical license document URL is published for this license.
						</p>
					{/if}
					<details class="mt-3">
						<summary
							class="cursor-pointer text-sm text-neutral-700 underline underline-offset-2 hover:text-primary-700"
						>
							Show {license.datasets.length}
							{license.datasets.length === 1 ? 'dataset' : 'datasets'} under {license.name}
						</summary>
						<ul role="list" class="mt-2 space-y-1 pl-4 text-sm">
							{#each license.datasets as ds (ds.slug)}
								<li>
									<a
										class="text-primary-700 underline underline-offset-2 hover:text-primary-800"
										href={resolve('/datasets/[id]', { id: ds.slug })}
									>
										{ds.name}
									</a>
									<span class="ml-1 font-mono text-xs text-neutral-500">({ds.slug})</span>
								</li>
							{/each}
						</ul>
					</details>
				</li>
			{/each}
		</ul>
	</section>
</div>
