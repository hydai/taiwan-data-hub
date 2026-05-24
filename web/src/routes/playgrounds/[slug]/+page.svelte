<!--
	/playgrounds/[slug] — framing page that embeds the playground
	iframe and wires the postMessage host.

	The iframe runs with `sandbox="allow-scripts"` (no
	allow-same-origin) so it has a unique opaque origin: no DOM
	access to this page, no cookies / localStorage, no top-nav.
	Combined with the strict CSP applied to the playground response
	itself (see `web/src/lib/playgrounds/csp.ts`) this is roughly
	the same security model JSFiddle / CodeSandbox use.
-->
<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { resolve } from '$app/paths';
	import MetaTags from '$lib/seo/MetaTags.svelte';
	import { attachPlaygroundHost } from '$lib/playgrounds/host';
	import type { PlaygroundStatus } from '$lib/playgrounds/types';

	let { data } = $props();
	const playground = $derived(data.playground);

	let iframe: HTMLIFrameElement | null = $state(null);
	let detach: (() => void) | null = null;

	onMount(() => {
		if (!iframe) return;
		detach = attachPlaygroundHost({ iframe });
	});

	onDestroy(() => {
		detach?.();
		detach = null;
	});

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

	// Address the iframe at `app/index.html` explicitly rather than
	// the bare `app/` directory: SvelteKit's default trailing-slash
	// policy strips the slash, which would make relative asset URLs
	// inside the playground (./app.js, ./style.css) resolve one
	// directory too high. Naming index.html keeps the resolution
	// base under `/playgrounds/<slug>/app/`.
	const appUrl = $derived(`/playgrounds/${playground.slug}/app/index.html`);
</script>

<MetaTags
	title={playground.title_i18n.en ?? playground.title_i18n['zh-TW']}
	description={playground.description_i18n.en ?? playground.description_i18n['zh-TW']}
	schemaType="CollectionPage"
/>

<div class="mx-auto max-w-7xl px-4 py-8 sm:px-6 lg:px-8">
	<nav class="mb-4 text-sm text-neutral-500" aria-label="Breadcrumb">
		<ol class="flex flex-wrap items-center gap-1.5">
			<li>
				<a class="underline underline-offset-2 hover:text-primary-700" href={resolve('/')}>Home</a>
			</li>
			<li aria-hidden="true">/</li>
			<li>
				<a
					class="underline underline-offset-2 hover:text-primary-700"
					href={resolve('/playgrounds')}
				>
					Playgrounds
				</a>
			</li>
			<li aria-hidden="true">/</li>
			<li class="font-mono text-neutral-700">{playground.slug}</li>
		</ol>
	</nav>

	<header class="mb-6 border-b border-neutral-200 pb-4">
		<div class="flex flex-wrap items-baseline gap-3">
			<h1 class="text-2xl font-semibold text-neutral-900">{playground.title_i18n['zh-TW']}</h1>
			<span
				class="inline-flex items-center rounded-md px-2 py-0.5 text-xs font-medium ring-1 ring-inset {statusClass(
					playground.status
				)}"
			>
				{playground.status}
			</span>
		</div>
		{#if playground.title_i18n.en && playground.title_i18n.en !== playground.title_i18n['zh-TW']}
			<p class="mt-1 text-sm text-neutral-500">{playground.title_i18n.en}</p>
		{/if}
		<p class="mt-3 max-w-3xl text-sm text-neutral-700">{playground.description_i18n['zh-TW']}</p>
	</header>

	<div class="overflow-hidden rounded-lg border border-neutral-200 shadow-sm">
		<iframe
			bind:this={iframe}
			title={`Playground: ${playground.title_i18n.en ?? playground.title_i18n['zh-TW']}`}
			src={appUrl}
			sandbox="allow-scripts"
			class="block h-[640px] w-full bg-white"
		></iframe>
	</div>

	<aside class="mt-4 text-xs text-neutral-500">
		Sandbox: <code class="rounded bg-neutral-100 px-1 py-0.5 font-mono">allow-scripts</code>. All
		gateway calls proxy through this page; the iframe cannot reach the network directly.
	</aside>
</div>
