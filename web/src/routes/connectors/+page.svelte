<!--
	/connectors — showcase of external MCP servers that compose
	well with Taiwan Data Hub for AI-agent workflows.

	M6 #6.1 DoD: 8 connector cards, each showing name + avatar +
	description + token-requirement badge + install button. Source
	= config/connectors.yaml.

	The "install button" expands an inline <details> with the JSON
	snippet a user pastes into their MCP client config. #6.2 will
	build per-client install-guide pages (Claude / Cursor / Cline);
	the inline snippet here is the canonical Claude Desktop shape
	and is enough to get going without leaving the index.

	Logos are intentionally letter-initial avatars rather than
	brand marks — shipping arbitrary third-party trademarks is a
	licensing risk we don't need to take for the showcase, and the
	avatar style stays consistent across the eight cards.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import MetaTags from '$lib/seo/MetaTags.svelte';
	import { renderInstallSnippet } from '$lib/connectors/snippet';
	import type { Connector, ConnectorStatus } from '$lib/connectors/types';

	let { data } = $props();

	/**
	 * Visual styling for the status pill. Picked to read at a glance
	 * without relying on colour alone (the label text is the source
	 * of truth for screen readers).
	 */
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
	 * First letter of the English name (or zh-TW fallback) for the
	 * letter-initial avatar. Upper-cased so the visual reads as a
	 * monogram even when the source string starts lowercase (e.g.
	 * "n8n").
	 */
	function avatarLetter(c: Connector): string {
		const source = c.name_i18n.en ?? c.name_i18n['zh-TW'];
		return source.charAt(0).toUpperCase();
	}
</script>

<MetaTags
	title="Connectors"
	description="External MCP servers that compose with Taiwan Data Hub for AI-agent workflows — browser automation, knowledge bases, observability, and more."
	schemaType="CollectionPage"
/>

<div class="mx-auto max-w-7xl px-4 py-12 sm:px-6 lg:px-8">
	<header class="mb-10 max-w-3xl">
		<h1 class="text-4xl font-bold tracking-tight text-neutral-900">Connectors</h1>
		<p class="mt-3 text-lg text-neutral-600">
			Eight external MCP servers that pair naturally with Taiwan Data Hub. Each card carries an
			install-ready config snippet plus a clear token-requirement signal — install the ones you need
			alongside Taiwan Data Hub in your MCP client.
		</p>
	</header>

	<ul role="list" class="grid grid-cols-1 gap-6 md:grid-cols-2">
		{#each data.connectors as connector (connector.slug)}
			<li
				class="flex h-full flex-col rounded-lg border border-neutral-200 bg-white p-6 transition-shadow hover:shadow-md"
			>
				<header class="mb-4 flex items-start gap-4 border-b border-neutral-100 pb-4">
					<span
						aria-hidden="true"
						class="flex h-12 w-12 flex-none items-center justify-center rounded-md bg-primary-100 text-lg font-semibold text-primary-700"
					>
						{avatarLetter(connector)}
					</span>
					<div class="min-w-0 flex-1">
						<div class="flex flex-wrap items-baseline gap-2">
							<h2 class="text-xl font-semibold text-neutral-900">
								{connector.name_i18n['zh-TW']}
							</h2>
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
					</div>
				</header>

				<p class="mb-4 text-sm leading-relaxed text-neutral-700">
					{connector.description_i18n['zh-TW']}
				</p>

				<div class="mb-4 flex flex-wrap items-center gap-2">
					{#if connector.token_required}
						<span
							class="inline-flex items-center gap-1 rounded-md bg-amber-50 px-2 py-1 text-xs font-medium text-amber-800 ring-1 ring-amber-600/20 ring-inset"
						>
							<svg aria-hidden="true" class="h-3 w-3" viewBox="0 0 20 20" fill="currentColor">
								<path
									fill-rule="evenodd"
									d="M10 1a4.5 4.5 0 0 0-4.5 4.5V9H5a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-6a2 2 0 0 0-2-2h-.5V5.5A4.5 4.5 0 0 0 10 1Zm3 8V5.5a3 3 0 1 0-6 0V9h6Z"
									clip-rule="evenodd"
								/>
							</svg>
							Token required
						</span>
					{:else}
						<span
							class="inline-flex items-center gap-1 rounded-md bg-neutral-100 px-2 py-1 text-xs font-medium text-neutral-700 ring-1 ring-neutral-500/20 ring-inset"
						>
							No token
						</span>
					{/if}
					<a
						class="text-xs text-neutral-600 underline underline-offset-2 hover:text-primary-700"
						href={connector.homepage_url}
						target="_blank"
						rel="noopener noreferrer"
					>
						Homepage
					</a>
				</div>

				<p class="mb-4 text-sm leading-relaxed text-neutral-600">
					{connector.install_instructions_i18n['zh-TW']}
				</p>

				<div class="mt-auto space-y-3">
					<details class="rounded-md border border-neutral-200 bg-neutral-50">
						<summary
							class="cursor-pointer rounded-md px-3 py-2 text-sm font-medium text-primary-700 hover:bg-neutral-100 focus:ring-2 focus:ring-primary-500 focus:outline-none"
						>
							Install snippet
						</summary>
						<pre
							class="overflow-x-auto rounded-b-md bg-neutral-900 px-3 py-3 text-xs leading-relaxed text-neutral-100"><code
								>{renderInstallSnippet(connector)}</code
							></pre>
					</details>

					<a
						class="inline-flex w-full items-center justify-center rounded-md bg-primary-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-primary-700 focus:ring-2 focus:ring-primary-500 focus:ring-offset-2 focus:outline-none"
						href={resolve('/connectors/[slug]', { slug: connector.slug })}
					>
						Full install guide
					</a>
				</div>
			</li>
		{/each}
	</ul>
</div>
