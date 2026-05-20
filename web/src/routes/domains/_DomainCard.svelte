<!--
	Single card on the /domains index. Underscore prefix is a
	convention for route-local helpers — SvelteKit doesn't route
	files that don't start with `+`, but the underscore makes the
	intent explicit when scanning the routes/ tree.

	Card anatomy:
	  - kind indicator dot (color from #2.1 design tokens)
	  - zh-TW name (primary heading) + en sub-name (small, neutral)
	  - zh-TW description clamped to two lines
	  - dataset count footer
	  - whole card is a link to /domains/[slug] (filled in by #2.5)

	The link's accessible name comes from its visible text content
	(name + description + count) — no aria-label override, which
	would otherwise replace the descendant text for screen readers.

	Hover / focus reveal a thin primary-500 ring + lift via shadow.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import type { DomainCardData } from '$lib/domains/types';

	type Props = { domain: DomainCardData };
	let { domain }: Props = $props();

	// Kind → token mapping. Keeps the visual indicator semantic without
	// pulling in icon glyphs we don't have a design for yet.
	const KIND_DOT: Record<DomainCardData['kind'], string> = {
		topical: 'bg-primary-500',
		meta: 'bg-warning-500',
		horizontal: 'bg-info-500'
	};
</script>

<a
	href={resolve('/domains/[slug]', { slug: domain.slug })}
	class="group flex h-full flex-col gap-3 rounded-lg border border-neutral-200 bg-white p-5 transition-shadow hover:shadow-md focus:ring-2 focus:ring-primary-500 focus:outline-none"
>
	<div class="flex items-center gap-3">
		<span class={`h-2.5 w-2.5 shrink-0 rounded-full ${KIND_DOT[domain.kind]}`} aria-hidden="true"
		></span>
		<div class="flex min-w-0 flex-col">
			<span class="truncate text-base font-semibold text-neutral-900">
				{domain.name['zh-TW']}
			</span>
			{#if domain.name.en}
				<span class="truncate text-xs text-neutral-500">{domain.name.en}</span>
			{/if}
		</div>
	</div>

	{#if domain.description?.['zh-TW']}
		<p class="line-clamp-2 text-sm text-neutral-600">
			{domain.description['zh-TW']}
		</p>
	{/if}

	<div class="mt-auto flex items-center justify-between border-t border-neutral-100 pt-3">
		<span class="text-xs text-neutral-500">
			{domain.count}
			<span class="text-neutral-400">{domain.count === 1 ? 'dataset' : 'datasets'}</span>
		</span>
		<span
			class="text-xs text-primary-700 opacity-0 transition-opacity group-hover:opacity-100 group-focus:opacity-100"
			aria-hidden="true"
		>
			Browse →
		</span>
	</div>
</a>
