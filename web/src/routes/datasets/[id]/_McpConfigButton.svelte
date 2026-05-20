<!--
	"Copy MCP config" button — renders the JSON snippet a user would
	paste into their Claude Desktop `claude_desktop_config.json` (or
	equivalent) to expose this single dataset through Taiwan Data Hub's
	stdio shim.

	Underscore prefix is the route-local convention; only used on this
	page.

	The shape mirrors what `crates/mcp-stdio` will expose once the
	stdio shim ships (currently planned for M3); the env-var name
	(TDH_DATASET) is the contract this button is committing to, so
	the future shim must respect it.
-->
<script lang="ts">
	type Props = { datasetSlug: string };
	let { datasetSlug }: Props = $props();

	const config = $derived({
		mcpServers: {
			'taiwan-data-hub': {
				command: 'npx',
				args: ['-y', '@taiwan-data-hub/mcp-stdio'],
				env: {
					TDH_DATASET: datasetSlug
				}
			}
		}
	});

	const configText = $derived(JSON.stringify(config, null, 2));

	let copied = $state(false);
	let copyTimer: ReturnType<typeof setTimeout> | null = null;

	async function copyToClipboard() {
		try {
			await navigator.clipboard.writeText(configText);
			copied = true;
			if (copyTimer) clearTimeout(copyTimer);
			copyTimer = setTimeout(() => {
				copied = false;
			}, 2000);
		} catch {
			// Clipboard API may be blocked (insecure context, perms).
			// Fall back to surfacing the text so the user can copy
			// manually; the <pre> block below is always visible.
			copied = false;
		}
	}

	// Clear the pending "Copied" timer on unmount so navigation away
	// within the 2-second window doesn't leave a phantom closure
	// alive holding refs into the unmounted component.
	$effect(() => {
		return () => {
			if (copyTimer) {
				clearTimeout(copyTimer);
				copyTimer = null;
			}
		};
	});
</script>

<div class="rounded-lg border border-neutral-200 bg-neutral-50 p-5">
	<div class="flex items-center justify-between gap-3">
		<h2 class="text-base font-semibold text-neutral-900">MCP wiring</h2>
		<button
			type="button"
			onclick={copyToClipboard}
			class="inline-flex items-center gap-1.5 rounded-md bg-primary-700 px-3 py-1.5 text-sm font-medium text-white shadow-sm hover:bg-primary-800 focus:ring-2 focus:ring-primary-500 focus:outline-none"
		>
			{#if copied}
				<svg
					viewBox="0 0 24 24"
					class="h-4 w-4"
					fill="none"
					stroke="currentColor"
					stroke-width="2.5"
					aria-hidden="true"
				>
					<path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
				</svg>
				Copied
			{:else}
				<svg
					viewBox="0 0 24 24"
					class="h-4 w-4"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					aria-hidden="true"
				>
					<path
						stroke-linecap="round"
						stroke-linejoin="round"
						d="M8 5H6a2 2 0 00-2 2v12a2 2 0 002 2h10a2 2 0 002-2v-2M8 5a2 2 0 002 2h4a2 2 0 002-2M8 5a2 2 0 012-2h4a2 2 0 012 2m0 0h2a2 2 0 012 2v3"
					/>
				</svg>
				Copy MCP config
			{/if}
		</button>
	</div>

	<!--
		Dedicated polite live region for the copy confirmation.
		Putting aria-live on the <button> itself is non-standard and
		some screen readers don't announce content swaps on interactive
		elements reliably. A separate role="status" span is the
		canonical pattern.
	-->
	<span role="status" aria-live="polite" class="sr-only">
		{copied ? 'MCP config copied to clipboard' : ''}
	</span>

	<p class="mt-2 text-xs text-neutral-500">
		Paste into <code class="rounded bg-neutral-100 px-1 font-mono text-neutral-800"
			>claude_desktop_config.json</code
		>
		(or your MCP client's equivalent) to expose this dataset to AI agents.
	</p>

	<pre
		class="mt-3 overflow-x-auto rounded-md border border-neutral-200 bg-white p-3 font-mono text-xs leading-relaxed text-neutral-800"><code
			>{configText}</code
		></pre>
</div>
