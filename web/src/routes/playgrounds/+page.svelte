<!--
	/playgrounds — index of interactive demo apps.

	Surfaces every playground discovered under `playgrounds/<slug>/`
	(except hidden ones like `_template`). M6 #6.3 ships the
	framework + reference template; the five real playgrounds land
	in #6.4–#6.8 and will appear here automatically as their
	directories are added.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import MetaTags from '$lib/seo/MetaTags.svelte';
	import type { PlaygroundStatus } from '$lib/playgrounds/types';

	let { data } = $props();

	function statusClass(status: PlaygroundStatus): string {
		switch (status) {
			case 'stable':
				return 'bg-emerald-50 text-emerald-700 ring-emerald-600/20';
			case 'beta':
				return 'bg-amber-50 text-amber-700 ring-amber-600/20';
			case 'experimental':
				return 'bg-rose-50 text-rose-700 ring-rose-600/20';
		}
	}
</script>

<MetaTags
	title="Playgrounds"
	description="Interactive demos showing how to compose Taiwan Data Hub MCP tools — company lookup, judicial stats, geography, housing prices, and procurement analytics."
	schemaType="CollectionPage"
/>

<div class="mx-auto max-w-7xl px-4 py-12 sm:px-6 lg:px-8">
	<header class="mb-10 max-w-3xl">
		<h1 class="text-4xl font-bold tracking-tight text-neutral-900">Playgrounds</h1>
		<p class="mt-3 text-lg text-neutral-600">
			Each playground is a small interactive demo that composes Taiwan Data Hub's MCP tools. They
			run inside a sandboxed iframe with a strict CSP and route every gateway call through the
			parent frame — see <code class="rounded bg-neutral-100 px-1 py-0.5 font-mono text-xs"
				>playgrounds/README.md</code
			>
			for the author contract.
		</p>
	</header>

	{#if data.playgrounds.length === 0}
		<section
			class="rounded-lg border border-dashed border-neutral-300 bg-neutral-50 p-10 text-center"
		>
			<h2 class="text-lg font-semibold text-neutral-900">No playgrounds yet</h2>
			<p class="mt-2 text-sm text-neutral-600">
				The framework is in place; the first five playgrounds land in M6 #6.4 – #6.8. Want to
				contribute one? Open <code class="rounded bg-white px-1 py-0.5 font-mono text-xs"
					>playgrounds/README.md</code
				> for the spec.
			</p>
		</section>
	{:else}
		<ul role="list" class="grid grid-cols-1 gap-6 md:grid-cols-2">
			{#each data.playgrounds as p (p.slug)}
				<li
					class="flex h-full flex-col rounded-lg border border-neutral-200 bg-white p-6 transition-shadow hover:shadow-md"
				>
					<header class="mb-4 border-b border-neutral-100 pb-3">
						<div class="flex flex-wrap items-baseline gap-2">
							<h2 class="text-xl font-semibold text-neutral-900">{p.title_i18n['zh-TW']}</h2>
							<span
								class="inline-flex items-center rounded-md px-2 py-0.5 text-xs font-medium ring-1 ring-inset {statusClass(
									p.status
								)}"
							>
								{p.status}
							</span>
						</div>
						{#if p.title_i18n.en && p.title_i18n.en !== p.title_i18n['zh-TW']}
							<p class="mt-1 text-sm text-neutral-500">{p.title_i18n.en}</p>
						{/if}
					</header>

					<p class="mb-4 text-sm leading-relaxed text-neutral-700">
						{p.description_i18n['zh-TW']}
					</p>

					{#if p.tags.length > 0}
						<ul role="list" class="mb-4 flex flex-wrap gap-1.5">
							{#each p.tags as tag (tag)}
								<li>
									<span
										class="rounded-md bg-neutral-100 px-2 py-0.5 font-mono text-xs text-neutral-700"
										>{tag}</span
									>
								</li>
							{/each}
						</ul>
					{/if}

					<a
						class="mt-auto inline-flex w-full items-center justify-center rounded-md bg-primary-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-primary-700 focus:ring-2 focus:ring-primary-500 focus:ring-offset-2 focus:outline-none"
						href={resolve('/playgrounds/[slug]', { slug: p.slug })}
					>
						Open playground
					</a>
				</li>
			{/each}
		</ul>
	{/if}
</div>
