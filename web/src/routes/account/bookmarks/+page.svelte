<script lang="ts">
	import { enhance } from '$app/forms';
	import { resolve } from '$app/paths';
	import type { ActionData, PageData } from './$types';
	import {
		BOOKMARK_TARGET_KINDS,
		COLLECTION_NAME_MAX_LEN,
		type BookmarkTargetKind
	} from '$lib/bookmarks/types';

	const { data, form }: { data: PageData; form: ActionData | null } = $props();

	const kindLabel: Record<BookmarkTargetKind, string> = {
		dataset: 'Datasets',
		tool: 'Tools',
		connector: 'Connectors',
		playground: 'Playgrounds'
	};

	const formatDate = (iso: string): string => {
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		return d.toLocaleString('zh-TW', {
			year: 'numeric',
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit',
			timeZone: 'Asia/Taipei'
		});
	};
</script>

<svelte:head>
	<title>My bookmarks — Account · Taiwan Data Hub</title>
	<meta
		name="description"
		content="Bookmarked datasets, tools, connectors, and playgrounds plus your private collections."
	/>
</svelte:head>

<section class="mx-auto max-w-4xl px-4 py-8 sm:px-6 lg:px-8">
	<header class="mb-8">
		<h1 class="text-2xl font-semibold tracking-tight">Bookmarks</h1>
		<p class="text-muted-foreground mt-2 text-sm">
			Saved community items and your private collections. Collections are personal — no sharing yet.
		</p>
	</header>

	{#if data.state === 'unauthenticated'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Please sign in</p>
			<p class="text-muted-foreground mt-2">A session is required to see your bookmarks.</p>
		</div>
	{:else if data.state === 'unavailable' || data.state === 'unexpected'}
		<div class="border-border bg-muted/40 rounded-md border p-6 text-sm">
			<p class="font-medium">Bookmarks unavailable</p>
			<p class="text-muted-foreground mt-2">{data.message}</p>
		</div>
	{:else if data.state === 'ok'}
		{#if form?.message}
			<p
				class="border-destructive/40 bg-destructive/10 text-destructive mb-4 rounded-md border p-3 text-sm"
				role="alert"
			>
				{form.message}
			</p>
		{/if}
		{#if form?.created}
			<p class="border-border bg-muted/40 mb-4 rounded-md border p-3 text-sm" role="status">
				Collection created.
			</p>
		{/if}

		<section class="mb-10">
			<header class="mb-3 flex flex-wrap items-center gap-2">
				<h2 class="text-lg font-semibold">Saved items</h2>
				<nav class="flex flex-wrap items-center gap-2 text-sm" aria-label="Kind filter">
					<a
						class="border-border rounded-full border px-2 py-1 {data.kindFilter === null
							? 'bg-muted/50'
							: ''}"
						href={resolve('/account/bookmarks')}
						aria-current={data.kindFilter === null ? 'page' : undefined}>All</a
					>
					{#each BOOKMARK_TARGET_KINDS as k (k)}
						<a
							class="border-border rounded-full border px-2 py-1 {data.kindFilter === k
								? 'bg-muted/50'
								: ''}"
							href={resolve(`/account/bookmarks?kind=${k}`)}
							aria-current={data.kindFilter === k ? 'page' : undefined}>{kindLabel[k]}</a
						>
					{/each}
				</nav>
			</header>
			{#if data.bookmarks.length === 0}
				<p class="text-muted-foreground text-sm">
					No bookmarks{data.kindFilter ? ` of kind ${data.kindFilter}` : ''} yet. Tap the heart on any
					card to save it.
				</p>
			{:else}
				<ul class="space-y-2" data-testid="bookmarks">
					{#each data.bookmarks as b (b.id)}
						<li
							class="border-border bg-card flex items-center justify-between rounded-md border p-3 text-sm"
						>
							<div>
								<p class="text-muted-foreground text-xs capitalize">{b.target_kind}</p>
								<p class="font-mono text-xs">{b.target_id}</p>
							</div>
							<p class="text-muted-foreground text-xs">Saved {formatDate(b.created_at)}</p>
						</li>
					{/each}
				</ul>
			{/if}
		</section>

		<section>
			<header class="mb-3 flex items-center justify-between">
				<h2 class="text-lg font-semibold">Collections</h2>
			</header>
			<form
				method="POST"
				action="?/create_collection"
				use:enhance
				class="border-border bg-muted/30 mb-4 flex flex-col gap-2 rounded-md border p-3 text-sm sm:flex-row sm:items-center"
			>
				<label class="flex-1">
					<span class="sr-only">New collection name</span>
					<input
						name="name"
						required
						maxlength={COLLECTION_NAME_MAX_LEN}
						placeholder="New collection name"
						class="border-border focus-visible:ring-ring w-full rounded-md border px-3 py-2 text-sm focus-visible:ring-2 focus-visible:outline-none"
					/>
				</label>
				<button
					type="submit"
					class="bg-primary text-primary-foreground hover:bg-primary/90 rounded-md px-3 py-2 text-sm font-medium"
					>Create</button
				>
			</form>
			{#if data.collections.length === 0}
				<p class="text-muted-foreground text-sm">
					No collections yet — create one to start organising saves.
				</p>
			{:else}
				<ul class="space-y-2">
					{#each data.collections as c (c.id)}
						<li
							class="border-border bg-card flex items-center justify-between rounded-md border p-3 text-sm"
						>
							<div>
								<p class="font-medium">{c.name}</p>
								{#if c.description}
									<p class="text-muted-foreground mt-1 text-xs">{c.description}</p>
								{/if}
								<p class="text-muted-foreground mt-1 text-xs">
									Created {formatDate(c.created_at)}
								</p>
							</div>
							<form method="POST" action="?/delete_collection" use:enhance>
								<input type="hidden" name="id" value={c.id} />
								<button
									type="submit"
									aria-label={`Delete collection ${c.name}`}
									class="border-border hover:bg-muted/40 focus-visible:ring-ring rounded-md border px-3 py-2 text-xs font-medium focus-visible:ring-2 focus-visible:outline-none"
									>Delete</button
								>
							</form>
						</li>
					{/each}
				</ul>
			{/if}
		</section>
	{/if}
</section>
