<script lang="ts">
	import { enhance } from '$app/forms';
	import type { ActionData, PageData } from './$types';
	import type { ApiKeySummary } from '$lib/account/types';
	import { RATE_LIMIT_TIERS } from '$lib/account/types';

	const { data, form }: { data: PageData; form: ActionData | null } = $props();

	// The action return surfaces the one-time cleartext from
	// `create` or `rotate`. We hold a local copy so the modal
	// stays visible after navigation completes — once the user
	// dismisses, the value is dropped and never re-displayed.
	let dismissedCleartextId = $state<string | null>(null);
	const issued = $derived(
		form?.created && form.created.id !== dismissedCleartextId ? form.created : null
	);
	const wasRotation = $derived(Boolean(form?.was_rotation));

	const formatDate = (iso: string | null): string => {
		if (iso === null) return '—';
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		// `zh-TW` locale rendering keeps the calendar consistent
		// with the rest of the marketplace UI; toggling to user
		// preferences lands when Paraglide v2's `getLocale()` is
		// wired through.
		//
		// `timeZone: 'Asia/Taipei'` is set explicitly because the
		// page is SSR'd on Node (whose default zone is the host)
		// and then hydrated on the client (whose zone is the
		// user's). Without a pinned zone the two formats can
		// disagree, causing a hydration mismatch / brief flicker
		// the moment Svelte takes over the DOM. Asia/Taipei is
		// the right anchor since this product targets a Taiwan
		// audience; switching to user-local rendering would
		// require a client-only formatter that mounts after
		// hydration.
		return d.toLocaleString('zh-TW', {
			year: 'numeric',
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit',
			timeZone: 'Asia/Taipei'
		});
	};

	const isActive = (key: ApiKeySummary): boolean => key.revoked_at === null;
</script>

<svelte:head>
	<title>API keys — Account · Taiwan Data Hub</title>
	<meta
		name="description"
		content="Manage Taiwan Data Hub API keys: create, revoke, and rotate keys for programmatic access."
	/>
</svelte:head>

<section class="mx-auto max-w-4xl px-4 py-8 sm:px-6 lg:px-8">
	<header class="mb-8">
		<h1 class="text-2xl font-semibold tracking-tight">API keys</h1>
		<p class="text-muted-foreground mt-2 text-sm">
			Programmatic access to the Taiwan Data Hub gateway. Keys are shown only once on creation —
			store them in a password manager. Revoke immediately if a key is compromised.
		</p>
	</header>

	{#if data.state === 'unauthenticated'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Please sign in</p>
			<p class="text-muted-foreground mt-2">
				You need an active session to manage API keys. Sign in from the home page and return.
			</p>
		</div>
	{:else if data.state === 'unavailable'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Service temporarily unavailable</p>
			<p class="text-muted-foreground mt-2">{data.message}</p>
		</div>
	{:else if data.state === 'unexpected'}
		<div class="border-destructive bg-destructive/5 rounded-md border p-6 text-sm">
			<p class="font-medium">Unexpected error</p>
			<p class="text-muted-foreground mt-2">{data.message}</p>
		</div>
	{:else if data.state === 'ok'}
		{#if form?.revoke?.error}
			<aside
				role="alert"
				class="border-destructive bg-destructive/5 mb-6 rounded-md border p-4 text-sm"
			>
				<p class="font-medium">Revoke failed</p>
				<p class="text-muted-foreground mt-1">{form.revoke.error}</p>
			</aside>
		{/if}
		{#if form?.rotate?.error}
			<aside
				role="alert"
				class="border-destructive bg-destructive/5 mb-6 rounded-md border p-4 text-sm"
			>
				<p class="font-medium">Rotate failed</p>
				<p class="text-muted-foreground mt-1">{form.rotate.error}</p>
			</aside>
		{/if}
		{#if issued}
			<aside role="alert" class="border-primary/30 bg-primary/5 mb-6 rounded-md border p-4 text-sm">
				<p class="font-medium">
					{wasRotation ? 'Key rotated — copy the new value below' : 'Key created — copy it now'}
				</p>
				<p class="text-muted-foreground mt-1">
					This is the only time the full key will be displayed. Store it in a password manager
					before dismissing this notice.
				</p>
				<code
					class="bg-background/80 mt-3 block w-full overflow-x-auto rounded px-3 py-2 font-mono text-xs"
					data-testid="issued-cleartext">{issued.cleartext}</code
				>
				<button
					type="button"
					class="border-primary/40 text-primary hover:bg-primary/10 mt-3 inline-flex items-center rounded border px-3 py-1 text-xs font-medium"
					onclick={() => (dismissedCleartextId = issued.id)}
				>
					I have copied it — dismiss
				</button>
			</aside>
		{/if}

		<form
			method="POST"
			action="?/create"
			use:enhance
			class="border-border mb-8 rounded-md border p-4 text-sm"
		>
			<fieldset class="space-y-4">
				<legend class="text-base font-medium">Create new key</legend>
				<div>
					<label class="mb-1 block text-xs font-medium" for="new-key-name">Name</label>
					<input
						id="new-key-name"
						name="name"
						type="text"
						required
						placeholder="laptop"
						class="border-input bg-background focus:ring-ring w-full rounded border px-3 py-2 text-sm focus:ring-2 focus:outline-none"
					/>
				</div>
				<div>
					<label class="mb-1 block text-xs font-medium" for="new-key-tier">Rate-limit tier</label>
					<select
						id="new-key-tier"
						name="rate_limit_tier"
						class="border-input bg-background focus:ring-ring w-full rounded border px-3 py-2 text-sm focus:ring-2 focus:outline-none"
					>
						{#each RATE_LIMIT_TIERS as tier (tier)}
							<option value={tier}>{tier}</option>
						{/each}
					</select>
				</div>
				<div>
					<label class="mb-1 block text-xs font-medium" for="new-key-scopes">
						Scopes <span class="text-muted-foreground">(comma-separated)</span>
					</label>
					<input
						id="new-key-scopes"
						name="scopes"
						type="text"
						placeholder="read,write"
						class="border-input bg-background focus:ring-ring w-full rounded border px-3 py-2 text-sm focus:ring-2 focus:outline-none"
					/>
				</div>
				<button
					type="submit"
					class="bg-primary text-primary-foreground hover:bg-primary/90 inline-flex items-center rounded px-4 py-2 text-sm font-medium"
				>
					Create key
				</button>
				{#if form?.create?.error}
					<p role="alert" class="text-destructive text-xs">{form.create.error}</p>
				{/if}
			</fieldset>
		</form>

		<div class="border-border rounded-md border">
			<table class="w-full text-left text-sm">
				<thead class="bg-muted/40 text-muted-foreground text-xs uppercase">
					<tr>
						<th class="px-4 py-2">Name</th>
						<th class="px-4 py-2">Prefix</th>
						<th class="px-4 py-2">Tier</th>
						<th class="px-4 py-2">Scopes</th>
						<th class="px-4 py-2">Created</th>
						<th class="px-4 py-2">Last used</th>
						<th class="px-4 py-2">State</th>
						<th class="sr-only px-4 py-2">Actions</th>
					</tr>
				</thead>
				<tbody>
					{#if data.keys.length === 0}
						<tr>
							<td colspan="8" class="text-muted-foreground px-4 py-6 text-center">
								No API keys yet — create one above.
							</td>
						</tr>
					{:else}
						{#each data.keys as key (key.id)}
							<tr class="border-border border-t">
								<td class="px-4 py-2 font-medium">{key.name}</td>
								<td class="px-4 py-2 font-mono text-xs">{key.key_prefix}…</td>
								<td class="px-4 py-2">{key.rate_limit_tier}</td>
								<td class="px-4 py-2 text-xs">
									{#if key.scopes.length === 0}
										<span class="text-muted-foreground">—</span>
									{:else}
										<!-- Pre-join with `,` rather than CSS spacing so the
										     comma stays inside the truncate boundary on narrow
										     viewports; the cell wraps onto a second line
										     before it hides any individual scope. -->
										{key.scopes.join(', ')}
									{/if}
								</td>
								<td class="px-4 py-2">{formatDate(key.created_at)}</td>
								<td class="px-4 py-2">{formatDate(key.last_used_at)}</td>
								<td class="px-4 py-2">
									{#if isActive(key)}
										<span class="text-primary">active</span>
									{:else}
										<span class="text-muted-foreground">revoked {formatDate(key.revoked_at)}</span>
									{/if}
								</td>
								<td class="px-4 py-2 text-right">
									{#if isActive(key)}
										<form method="POST" action="?/rotate" use:enhance class="inline">
											<input type="hidden" name="id" value={key.id} />
											<button
												type="submit"
												class="text-primary hover:underline"
												aria-label={`Rotate ${key.name}`}>Rotate</button
											>
										</form>
										<form method="POST" action="?/revoke" use:enhance class="ml-3 inline">
											<input type="hidden" name="id" value={key.id} />
											<button
												type="submit"
												class="text-destructive hover:underline"
												aria-label={`Revoke ${key.name}`}>Revoke</button
											>
										</form>
									{:else}
										<span class="text-muted-foreground text-xs">—</span>
									{/if}
								</td>
							</tr>
						{/each}
					{/if}
				</tbody>
			</table>
		</div>
	{/if}
</section>
