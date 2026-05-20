<!--
	Stub. Per-domain dataset list with tier / format / license filters
	lands in M2 #2.5 (issue #23).
-->
<script lang="ts">
	import { page } from '$app/state';
	import MetaTags from '$lib/seo/MetaTags.svelte';
</script>

<MetaTags
	title={`Domain · ${page.params.slug}`}
	description={`Datasets in the ${page.params.slug} domain on Taiwan Data Hub. Tier / format / license filters land in #2.5.`}
	schemaType="CollectionPage"
/>

<div class="mx-auto max-w-7xl px-4 py-16 sm:px-6 lg:px-8">
	<h1 class="text-3xl font-bold tracking-tight text-neutral-900">
		Domain · <span class="font-mono text-primary-700">{page.params.slug}</span>
	</h1>
	<p class="mt-3 text-neutral-600">
		Dataset list with tier / format / license filters lands in
		<a class="text-primary-700 underline" href="https://github.com/hydai/taiwan-data-hub/issues/23"
			>#2.5</a
		>.
	</p>
</div>
