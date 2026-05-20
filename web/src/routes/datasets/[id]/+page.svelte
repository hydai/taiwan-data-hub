<!--
	Stub. Full dataset detail (resources + Copy MCP config button)
	lands in M2 #2.6 (issue #24). Kept as a stub now so:
	  - links from /datasets and /domains/[slug] resolve cleanly
	  - SvelteKit's RouteId knows /datasets/[id] exists for resolve()
	  - the Lighthouse CI gate (#2.10) has a real dynamic route to
	    score, not just the static /datasets index
-->
<script lang="ts">
	import { page } from '$app/state';
	import MetaTags from '$lib/seo/MetaTags.svelte';
</script>

<MetaTags
	title={`Dataset · ${page.params.id}`}
	description={`Dataset detail for ${page.params.id} on Taiwan Data Hub. Resources + MCP wiring instructions land in #2.6.`}
	schemaType="Dataset"
/>

<div class="mx-auto max-w-7xl px-4 py-16 sm:px-6 lg:px-8">
	<h1 class="text-3xl font-bold tracking-tight text-neutral-900">
		Dataset · <span class="font-mono text-primary-700">{page.params.id}</span>
	</h1>
	<p class="mt-3 text-neutral-600">
		Resources and the "Copy MCP config" button land in
		<a class="text-primary-700 underline" href="https://github.com/hydai/taiwan-data-hub/issues/24"
			>#2.6</a
		>.
	</p>
</div>
