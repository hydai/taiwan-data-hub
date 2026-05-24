<script lang="ts">
	import MetaTags from '$lib/seo/MetaTags.svelte';
	import { m } from '$lib/paraglide/messages';

	// Svelte 5 Runes — pre-alpha placeholder home.
	// Replaced by the real marketplace shell in M2 #2.2/#2.4.
	//
	// Milestone `id` + `name` are project-internal labels (build-time
	// brand strings) — kept untranslated. `status` is a controlled
	// vocabulary so it goes through the message catalog.
	const STATUS_IN_PROGRESS = 'in_progress';
	const STATUS_PLANNED = 'planned';
	const milestones = [
		{ id: 'M0', name: 'Foundations', status: STATUS_IN_PROGRESS },
		{ id: 'M1', name: 'MCP MVP', status: STATUS_PLANNED },
		{ id: 'M2', name: 'Marketplace UI', status: STATUS_PLANNED },
		{ id: 'M3', name: 'Rich MCP + Utility Wave 1', status: STATUS_PLANNED },
		{ id: 'M4', name: 'Auth + Personal/Multi-user', status: STATUS_PLANNED },
		{ id: 'M5a', name: 'Community Features', status: STATUS_PLANNED },
		{ id: 'M5b', name: 'Multi-source ETL', status: STATUS_PLANNED },
		{ id: 'M6', name: 'Connectors + Playground + Util W2', status: STATUS_PLANNED },
		{ id: 'M7', name: 'Discovery + REST + i18n', status: STATUS_PLANNED }
	] as const;

	function statusLabel(status: (typeof milestones)[number]['status']) {
		return status === STATUS_IN_PROGRESS ? m.home_status_in_progress() : m.home_status_planned();
	}
</script>

<MetaTags
	title="Taiwan Data Hub"
	description="Open-source, self-hostable MCP service hub for Taiwan public data. Aggregates 20 domains for AI agents like Claude Desktop, Cursor, and Cline."
	schemaType="WebSite"
/>

<div class="mx-auto max-w-3xl px-6 py-16">
	<header class="space-y-2">
		<p class="text-sm font-medium tracking-wide text-primary-700 uppercase">
			{m.home_pre_alpha_badge()}
		</p>
		<h1 class="text-4xl font-bold tracking-tight">{m.app_name()}</h1>
		<p class="text-lg text-neutral-600">{m.app_tagline_short()}</p>
	</header>

	<section class="mt-10 rounded-lg border border-neutral-200 bg-primary-50 p-5">
		<h2 class="text-base font-semibold">{m.home_scaffold_heading()}</h2>
		<p class="mt-1 text-sm text-neutral-700">
			{m.home_scaffold_prefix()}
			<a class="underline" href="https://github.com/hydai/taiwan-data-hub/milestone/3">M2</a
			>{m.home_scaffold_middle()}
			<a class="underline" href="https://github.com/hydai/taiwan-data-hub/blob/main/docs/DESIGN.md"
				>docs/DESIGN.md</a
			>
			{m.home_scaffold_suffix()}
		</p>
	</section>

	<section class="mt-10">
		<h2 class="text-xl font-semibold">{m.home_roadmap_heading()}</h2>
		<ul class="mt-4 divide-y divide-neutral-200 rounded-md border border-neutral-200">
			{#each milestones as milestone (milestone.id)}
				<li class="flex items-center justify-between px-4 py-3">
					<span class="font-mono text-sm font-semibold text-primary-700">{milestone.id}</span>
					<span class="flex-1 px-4 text-sm">{milestone.name}</span>
					<span class="text-xs text-neutral-500">{statusLabel(milestone.status)}</span>
				</li>
			{/each}
		</ul>
	</section>
</div>
