<script lang="ts">
	// Svelte 5 Runes — pre-alpha placeholder home.
	// Replaced by the real marketplace shell in M2 #2.2/#2.4.
	const milestones = [
		{ id: 'M0', name: 'Foundations', status: 'in progress' },
		{ id: 'M1', name: 'MCP MVP', status: 'planned' },
		{ id: 'M2', name: 'Marketplace UI', status: 'planned' },
		{ id: 'M3', name: 'Rich MCP + Utility Wave 1', status: 'planned' },
		{ id: 'M4', name: 'Auth + Personal/Multi-user', status: 'planned' },
		{ id: 'M5a', name: 'Community Features', status: 'planned' },
		{ id: 'M5b', name: 'Multi-source ETL', status: 'planned' },
		{ id: 'M6', name: 'Connectors + Playground + Util W2', status: 'planned' },
		{ id: 'M7', name: 'Discovery + REST + i18n', status: 'planned' }
	];
</script>

<main class="mx-auto max-w-3xl px-6 py-16">
	<header class="space-y-2">
		<p class="text-sm font-medium tracking-wide text-brand-700 uppercase">
			Pre-alpha · design phase
		</p>
		<h1 class="text-4xl font-bold tracking-tight">Taiwan Data Hub</h1>
		<p class="text-lg text-gray-600">
			Open-source, self-hostable MCP service hub for Taiwan public data.
		</p>
	</header>

	<section class="mt-10 rounded-lg border border-gray-200 bg-brand-50 p-5">
		<h2 class="text-base font-semibold">This is a scaffold.</h2>
		<p class="mt-1 text-sm text-gray-700">
			Real marketplace UI lands in
			<a class="underline" href="https://github.com/hydai/taiwan-data-hub/milestone/3">M2</a>. Until
			then, see
			<a class="underline" href="https://github.com/hydai/taiwan-data-hub/blob/main/docs/DESIGN.md"
				>docs/DESIGN.md</a
			>
			for the full system design.
		</p>
	</section>

	<section class="mt-10">
		<h2 class="text-xl font-semibold">Roadmap</h2>
		<ul class="mt-4 divide-y divide-gray-200 rounded-md border border-gray-200">
			{#each milestones as m (m.id)}
				<li class="flex items-center justify-between px-4 py-3">
					<span class="font-mono text-sm font-semibold text-brand-700">{m.id}</span>
					<span class="flex-1 px-4 text-sm">{m.name}</span>
					<span class="text-xs text-gray-500">{m.status}</span>
				</li>
			{/each}
		</ul>
	</section>

	<footer class="mt-12 text-xs text-gray-400">
		Apache-2.0 ·
		<a class="underline" href="https://github.com/hydai/taiwan-data-hub">GitHub</a>
	</footer>
</main>
