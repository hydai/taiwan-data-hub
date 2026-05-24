<!--
	/connectors/[slug] — per-connector install guide.

	M6 #6.2 DoD: each of the 8 connectors gets an install guide
	with Claude Desktop / Cursor / Cline snippets. The three
	clients share the same `{"mcpServers": {<slug>: <template>}}`
	envelope, so the JSON snippet is identical across all three
	sections — only the surrounding instructions (config file
	path or extension UI flow) vary.

	Page layout:
	  - breadcrumb back to /connectors
	  - hero: name + status pill + description
	  - "Before you start" — token setup hint (when applicable)
	  - one section per client (Claude Desktop / Cursor / Cline),
	    each with config-file paths or UI steps + the snippet +
	    a reload hint
	  - "Verify" footer pointing back to the upstream homepage

	Why prerender: the YAML is build-time data; rendering at
	request time would burn CPU for no benefit and would also
	mean shipping the YAML parser into the SSR runtime more
	frequently. With prerender + +page.server.ts the YAML never
	reaches the client bundle.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import MetaTags from '$lib/seo/MetaTags.svelte';
	import { renderInstallSnippet } from '$lib/connectors/snippet';
	import type { ConnectorStatus } from '$lib/connectors/types';

	let { data } = $props();
	const connector = $derived(data.connector);
	const clients = $derived(data.clients);
	const snippet = $derived(renderInstallSnippet(connector));

	function statusClass(status: ConnectorStatus): string {
		switch (status) {
			case 'stable':
				return 'bg-emerald-50 text-emerald-700 ring-emerald-600/20';
			case 'beta':
				return 'bg-amber-50 text-amber-700 ring-amber-600/20';
			case 'experimental':
				return 'bg-rose-50 text-rose-700 ring-rose-600/20';
		}
	}

	/**
	 * Sorted list of env-var names the user must set. Stable
	 * alphabetical order so the "you'll need these env vars" list
	 * doesn't shuffle between deploys.
	 */
	const envKeys = $derived(
		Object.keys(connector.mcp_config_template.env ?? {}).sort((a, b) => a.localeCompare(b))
	);
</script>

<MetaTags
	title={`Install ${connector.name_i18n.en ?? connector.name_i18n['zh-TW']}`}
	description={`Install the ${connector.name_i18n.en ?? connector.name_i18n['zh-TW']} MCP server alongside Taiwan Data Hub — Claude Desktop, Cursor, and Cline snippets.`}
	schemaType="CollectionPage"
/>

<div class="mx-auto max-w-4xl px-4 py-10 sm:px-6 lg:px-8">
	<nav class="mb-6 text-sm text-neutral-500" aria-label="Breadcrumb">
		<ol class="flex flex-wrap items-center gap-1.5">
			<li>
				<a class="underline underline-offset-2 hover:text-primary-700" href={resolve('/')}>Home</a>
			</li>
			<li aria-hidden="true">/</li>
			<li>
				<a
					class="underline underline-offset-2 hover:text-primary-700"
					href={resolve('/connectors')}
				>
					Connectors
				</a>
			</li>
			<li aria-hidden="true">/</li>
			<li class="font-mono text-neutral-700">{connector.slug}</li>
		</ol>
	</nav>

	<header class="mb-8 border-b border-neutral-200 pb-6">
		<div class="flex flex-wrap items-baseline gap-3">
			<h1 class="text-3xl font-bold tracking-tight text-neutral-900">
				{connector.name_i18n['zh-TW']}
			</h1>
			<span
				class="inline-flex items-center rounded-md px-2 py-0.5 text-xs font-medium ring-1 ring-inset {statusClass(
					connector.status
				)}"
			>
				{connector.status}
			</span>
		</div>
		{#if connector.name_i18n.en && connector.name_i18n.en !== connector.name_i18n['zh-TW']}
			<p class="mt-1 text-sm text-neutral-500">{connector.name_i18n.en}</p>
		{/if}
		<p class="mt-4 max-w-3xl text-base leading-relaxed text-neutral-700">
			{connector.description_i18n['zh-TW']}
		</p>
		<p class="mt-2 max-w-3xl text-sm text-neutral-600">
			Upstream: <a
				class="text-primary-700 underline underline-offset-2 hover:text-primary-800"
				href={connector.homepage_url}
				target="_blank"
				rel="noopener noreferrer">{connector.homepage_url}</a
			>
		</p>
	</header>

	{#if connector.token_required}
		<section class="mb-10 rounded-md border border-amber-200 bg-amber-50 p-5">
			<h2 class="text-lg font-semibold text-amber-900">Before you start: get your credentials</h2>
			<p class="mt-2 text-sm leading-relaxed text-amber-900">
				{connector.install_instructions_i18n['zh-TW']}
			</p>
			{#if envKeys.length > 0}
				<p class="mt-3 text-xs font-semibold tracking-wide text-amber-900 uppercase">
					Environment variables to set
				</p>
				<ul role="list" class="mt-1 space-y-1 text-sm">
					{#each envKeys as key (key)}
						<li>
							<code class="rounded bg-amber-100 px-1.5 py-0.5 font-mono text-xs text-amber-900"
								>{key}</code
							>
						</li>
					{/each}
				</ul>
			{/if}
		</section>
	{:else}
		<section class="mb-10 rounded-md border border-neutral-200 bg-neutral-50 p-5">
			<h2 class="text-lg font-semibold text-neutral-900">No credentials required</h2>
			<p class="mt-2 text-sm leading-relaxed text-neutral-700">
				{connector.install_instructions_i18n['zh-TW']}
			</p>
		</section>
	{/if}

	<section class="space-y-10">
		<h2 class="text-2xl font-semibold text-neutral-900">Install in your MCP client</h2>
		<p class="-mt-6 text-sm text-neutral-600">
			Pick the client you use. The JSON block below is identical for all three — only the file path
			or UI flow differs. The snippet uses placeholder values like
			<code class="rounded bg-neutral-100 px-1 py-0.5 font-mono text-xs">&lt;TOKEN&gt;</code> that you
			must replace with your real secrets before saving.
		</p>

		{#each clients as client (client.slug)}
			<article
				id={client.slug}
				class="scroll-mt-8 rounded-lg border border-neutral-200 bg-white p-6"
			>
				<header class="mb-4 border-b border-neutral-100 pb-3">
					<h3 class="text-xl font-semibold text-neutral-900">{client.label}</h3>
					<p class="mt-1 text-sm text-neutral-600">{client.tagline}</p>
				</header>

				{#if client.configPaths}
					<p class="text-sm font-semibold text-neutral-800">Config file path</p>
					<ul role="list" class="mt-2 space-y-1.5 text-sm">
						{#each client.configPaths as p (p.platform)}
							<li class="flex flex-wrap items-baseline gap-2">
								<span class="font-medium text-neutral-700">{p.platform}:</span>
								<code
									class="rounded bg-neutral-100 px-1.5 py-0.5 font-mono text-xs text-neutral-800"
									>{p.path}</code
								>
							</li>
						{/each}
					</ul>
				{:else if client.uiSteps}
					<p class="text-sm font-semibold text-neutral-800">Install via the extension UI</p>
					<ol class="mt-2 list-decimal space-y-1 pl-5 text-sm text-neutral-700">
						{#each client.uiSteps as step (step)}
							<li>{step}</li>
						{/each}
					</ol>
				{/if}

				<p class="mt-4 text-sm font-semibold text-neutral-800">Snippet</p>
				<p class="mt-1 text-sm text-neutral-600">
					If the config file is empty or has no
					<code class="rounded bg-neutral-100 px-1 py-0.5 font-mono text-xs">mcpServers</code>
					block yet, paste this whole snippet as the file contents. If you already have other MCP servers
					configured, merge just the
					<code class="rounded bg-neutral-100 px-1 py-0.5 font-mono text-xs"
						>"{connector.slug}"</code
					>
					entry into your existing
					<code class="rounded bg-neutral-100 px-1 py-0.5 font-mono text-xs">mcpServers</code> object
					instead of overwriting the file.
				</p>
				<pre
					class="mt-2 overflow-x-auto rounded-md bg-neutral-900 px-4 py-3 text-xs leading-relaxed text-neutral-100"><code
						>{snippet}</code
					></pre>

				<p class="mt-3 text-sm text-neutral-600">{client.reloadHint}</p>
			</article>
		{/each}
	</section>

	<footer class="mt-12 border-t border-neutral-200 pt-6 text-sm text-neutral-600">
		<p>
			Stuck or noticing the upstream snippet has changed? Compare against the
			<a
				class="text-primary-700 underline underline-offset-2 hover:text-primary-800"
				href={connector.homepage_url}
				target="_blank"
				rel="noopener noreferrer">upstream README</a
			>
			and open a PR against
			<a
				class="text-primary-700 underline underline-offset-2 hover:text-primary-800"
				href="https://github.com/hydai/taiwan-data-hub/blob/main/config/connectors.yaml"
				target="_blank"
				rel="noopener noreferrer">config/connectors.yaml</a
			>
			— the install guide rebuilds itself from that file.
		</p>
	</footer>
</div>
