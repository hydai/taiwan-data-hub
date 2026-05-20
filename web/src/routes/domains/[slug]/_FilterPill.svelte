<!--
	One filter pill on /domains/[slug]. Clicking navigates via plain
	<a href={...}> so the URL stays the single source of truth.
	When `active` is true, the pill highlights and clicking again
	would clear that dimension (the parent passes the cleared URL).
-->
<script lang="ts">
	import { cn } from '$lib/utils';

	type Props = {
		href: string;
		label: string;
		active: boolean;
	};

	let { href, label, active }: Props = $props();
</script>

<!--
	The href is a relative query-string-only URL (e.g. `?tier=gold`)
	composed by `buildFilterUrl`. SvelteKit's resolve() is for full
	paths; appending a query string to the current page doesn't
	involve base-path resolution. Suppressing svelte/no-navigation-
	without-resolve here is the correct call.
-->
<!-- eslint-disable svelte/no-navigation-without-resolve -->
<a
	{href}
	aria-current={active ? 'true' : undefined}
	class={cn(
		'inline-flex items-center rounded-full border px-3 py-1 text-xs font-medium transition-colors focus:ring-2 focus:ring-primary-500 focus:outline-none',
		active
			? 'border-primary-500 bg-primary-50 text-primary-700'
			: 'border-neutral-200 bg-white text-neutral-600 hover:border-neutral-300 hover:text-neutral-900'
	)}
>
	{label}
</a>
<!-- eslint-enable svelte/no-navigation-without-resolve -->
