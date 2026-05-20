<!--
	One row in the per-domain dataset list. Route-local. Same tier
	color scheme as the detail page (gold/silver/bronze → warning/
	neutral/danger tones) so users see consistent affordances.

	The whole row is the link target — clicking anywhere on it
	navigates to /datasets/[id].
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import { cn } from '$lib/utils';
	import type { Dataset } from '$lib/datasets/types';

	type Props = { dataset: Dataset };
	let { dataset }: Props = $props();

	const TIER_PILL: Record<Dataset['tier'], string> = {
		gold: 'bg-warning-50 text-warning-700 border-warning-500/30',
		silver: 'bg-neutral-100 text-neutral-700 border-neutral-300',
		bronze: 'bg-danger-50 text-danger-700 border-danger-500/30'
	};
</script>

<a
	href={resolve('/datasets/[id]', { id: dataset.slug })}
	class="group flex h-full flex-col gap-3 rounded-lg border border-neutral-200 bg-white p-5 transition-shadow hover:shadow-md focus:ring-2 focus:ring-primary-500 focus:outline-none"
>
	<div class="flex flex-wrap items-center gap-2">
		<span
			class={cn(
				'inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-semibold',
				TIER_PILL[dataset.tier]
			)}
		>
			{dataset.tier}
		</span>
		<span
			class="rounded-full bg-neutral-100 px-2 py-0.5 font-mono text-xs font-medium text-neutral-700"
		>
			{dataset.format}
		</span>
		<span class="text-xs text-neutral-500">Updates {dataset.updated}</span>
	</div>

	<div class="flex min-w-0 flex-col">
		<span class="truncate text-base font-semibold text-neutral-900">
			{dataset.name['zh-TW']}
		</span>
		{#if dataset.name.en}
			<span class="truncate text-xs text-neutral-500">{dataset.name.en}</span>
		{/if}
	</div>

	<p class="line-clamp-2 text-sm text-neutral-600">
		{dataset.description['zh-TW']}
	</p>

	<div class="mt-auto flex items-center justify-between border-t border-neutral-100 pt-3">
		<span class="truncate text-xs text-neutral-500">{dataset.license}</span>
		<span
			class="text-xs text-primary-700 opacity-0 transition-opacity group-hover:opacity-100 group-focus:opacity-100"
			aria-hidden="true"
		>
			Open →
		</span>
	</div>
</a>
