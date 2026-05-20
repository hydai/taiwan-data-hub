<!--
	/collections — curated dataset packs, YAML-driven.

	M2 #2.7 DoD: lists curated packs (education / healthcare / tourism
	+ extensible). Source = config/collections.yaml. Each card shows
	the curator's editorial note plus the 6 anchor dataset slugs.

	Anchor slugs are placeholder strings until the dataset records land
	in M3 — the UI renders them as monospace links to /datasets/[id]
	so the structure is in place. Each slug is therefore a real,
	resolvable link (to the #2.6-bound dataset stub) even before its
	canonical record exists.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import MetaTags from '$lib/seo/MetaTags.svelte';

	let { data } = $props();
</script>

<MetaTags
	title="Collections"
	description="Curated packs of Taiwan public-data datasets — education, healthcare, tourism, and more. YAML-driven editorial groupings for AI agents and analysts."
	schemaType="CollectionPage"
/>

<div class="mx-auto max-w-7xl px-4 py-12 sm:px-6 lg:px-8">
	<header class="mb-10 max-w-3xl">
		<h1 class="text-4xl font-bold tracking-tight text-neutral-900">Collections</h1>
		<p class="mt-3 text-lg text-neutral-600">
			Editor-curated dataset packs anchored to a topic. Each collection ships with a curator note
			and six anchor datasets so you can wire an MCP-agent flow without first cataloguing dozens of
			sources.
		</p>
	</header>

	<ul role="list" class="grid grid-cols-1 gap-6 lg:grid-cols-2">
		{#each data.collections as collection (collection.slug)}
			<li
				class="flex h-full flex-col rounded-lg border border-neutral-200 bg-white p-6 transition-shadow hover:shadow-md"
			>
				<header class="mb-4 border-b border-neutral-100 pb-4">
					<h2 class="text-xl font-semibold text-neutral-900">
						{collection.name['zh-TW']}
					</h2>
					{#if collection.name.en}
						<p class="mt-1 text-sm text-neutral-500">{collection.name.en}</p>
					{/if}
				</header>

				<p class="mb-5 text-sm leading-relaxed text-neutral-700">
					{collection.curator_note['zh-TW']}
				</p>

				<div class="mt-auto">
					<h3 class="mb-2 text-xs font-semibold tracking-wide text-neutral-500 uppercase">
						Anchor datasets
					</h3>
					<ul role="list" class="space-y-1.5">
						{#each collection.anchor_datasets as slug (slug)}
							<li>
								<a
									href={resolve('/datasets/[id]', { id: slug })}
									class="block rounded-md px-2 py-1 font-mono text-sm text-primary-700 hover:bg-primary-50 hover:underline focus:ring-2 focus:ring-primary-500 focus:outline-none"
								>
									{slug}
								</a>
							</li>
						{/each}
					</ul>
				</div>
			</li>
		{/each}
	</ul>
</div>
