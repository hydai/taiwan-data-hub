<!--
	/domains marketplace index — 20 cards grouped by kind.

	M2 #2.4 DoD: 20 cards (icon + name + count + scope), SSR with
	5 min stale-while-revalidate cache header, Topical / Meta /
	Horizontal section dividers.

	Data lives in `config/domains.yaml`, loaded via +page.server.ts
	(see `$lib/domains/load.ts`). Dataset counts are zero until the
	storage queries land in M3 — the schema is in place so the UI
	doesn't need to change when real counts arrive.
-->
<script lang="ts">
	import DomainCard from './_DomainCard.svelte';

	let { data } = $props();
</script>

<svelte:head>
	<title>Domains · Taiwan Data Hub</title>
	<meta
		name="description"
		content="Browse 20 domains of Taiwan public data — topical, meta, and horizontal slices for AI agents and analysts."
	/>
</svelte:head>

<div class="mx-auto max-w-7xl px-4 py-12 sm:px-6 lg:px-8">
	<header class="mb-10 max-w-3xl">
		<h1 class="text-4xl font-bold tracking-tight text-neutral-900">Domains</h1>
		<p class="mt-3 text-lg text-neutral-600">
			Taiwan public data sliced into 20 browsable domains. Pick a domain to see its datasets, tier,
			and MCP wiring instructions.
		</p>
	</header>

	{#each data.groups as group (group.kind)}
		<section class="mb-12" aria-labelledby={`group-${group.kind}`}>
			<div class="mb-5 flex items-baseline gap-3 border-b border-neutral-200 pb-3">
				<h2 id={`group-${group.kind}`} class="text-xl font-semibold text-neutral-900">
					{group.heading}
				</h2>
				<span class="text-sm text-neutral-500">{group.subheading}</span>
				<span class="ml-auto text-xs text-neutral-400">
					{group.domains.length}
					{group.domains.length === 1 ? 'domain' : 'domains'}
				</span>
			</div>

			<!--
				role="list" preserves the list semantics that Tailwind's
				preflight `list-style: none` strips on Safari. The <li>
				stays a real layout box so AT correctly announces
				"list, N items" — `display: contents` would drop that
				announcement on WebKit.
			-->
			<ul role="list" class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
				{#each group.domains as domain (domain.slug)}
					<li><DomainCard {domain} /></li>
				{/each}
			</ul>
		</section>
	{/each}
</div>
