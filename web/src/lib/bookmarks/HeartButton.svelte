<!--
	Reusable heart/favorite toggle button (#5a.4).

	Renders a heart icon that flips between hollow / filled
	state on click. The component:

	- Accepts `currentUserId === null` (rendered disabled
	  with a "Sign in to bookmark" title).
	- Optimistically flips the local state for snappy UI and
	  reconciles on the gateway's response.
	- All fetches are same-origin (`/api/v1/bookmarks`) so the
	  session cookie travels via the reverse proxy.
-->
<script lang="ts">
	import { bookmarksUrl } from '$lib/bookmarks/gateway';
	import type { BookmarkTargetKind, ToggleResponse } from '$lib/bookmarks/types';

	let {
		targetKind,
		targetId,
		currentUserId,
		bookmarked: initialBookmarked,
		size = 'sm'
	}: {
		targetKind: BookmarkTargetKind;
		targetId: string;
		currentUserId: string | null;
		bookmarked: boolean;
		size?: 'sm' | 'md';
	} = $props();

	// Local optimistic state — flips immediately on click and
	// reconciles after the network round trip lands. Seed
	// from the SSR prop via an IIFE so the Svelte
	// `state_referenced_locally` lint doesn't fire on
	// `$state(initialBookmarked)`; the `$effect` below
	// handles the "props changed because SvelteKit reused
	// this instance across /datasets/A → /datasets/B"
	// case.
	let bookmarked = $state((() => initialBookmarked)());
	let inFlight = $state(false);
	let error = $state<string | null>(null);

	// Track the (kind, id) pair this state was seeded for.
	// SvelteKit reuses the same HeartButton instance when
	// the user navigates between dataset pages, so the
	// component sees new props instead of remounting.
	// Without this, the heart would stay stuck on the
	// previous dataset's bookmark state.
	//
	// The key starts empty so the first `$effect` run on
	// mount unconditionally re-seeds — keeps the prop reads
	// inside the closure (the Svelte lint forbids reading
	// `$props()` values at top level for exactly this
	// "snapshot once" reason).
	let lastTargetKey = '';
	$effect(() => {
		const key = `${targetKind}|${targetId}`;
		if (key === lastTargetKey) return;
		lastTargetKey = key;
		bookmarked = initialBookmarked;
		error = null;
		// Any in-flight toggle was for the previous target.
		// Clear `inFlight` here so the new target's button
		// is interactive immediately, and rely on the
		// stale-target guard in the toggle's `finally` to
		// keep the old response from clobbering the new
		// state when it lands.
		inFlight = false;
	});

	async function toggle(): Promise<void> {
		if (currentUserId === null || inFlight) return;
		// Snapshot the target at the start so a response
		// that lands after the user has navigated to a
		// different dataset doesn't bleed its outcome into
		// the new target's state. Same key shape as the
		// effect above.
		const startKey = `${targetKind}|${targetId}`;
		const previous = bookmarked;
		bookmarked = !bookmarked;
		inFlight = true;
		error = null;
		try {
			const res = await fetch(bookmarksUrl(), {
				method: 'POST',
				headers: {
					accept: 'application/json',
					'content-type': 'application/json'
				},
				credentials: 'include',
				body: JSON.stringify({
					target_kind: targetKind,
					target_id: targetId
				})
			});
			// If the user navigated away before the response
			// landed, drop it on the floor. The effect above
			// already re-seeded state for the new target;
			// reverting/reconciling here would corrupt it.
			if (`${targetKind}|${targetId}` !== startKey) return;
			if (!res.ok) {
				bookmarked = previous;
				error = `Failed to update bookmark (${res.status}).`;
				return;
			}
			const body = (await res.json().catch(() => null)) as ToggleResponse | null;
			if (body === null || (body.outcome !== 'bookmarked' && body.outcome !== 'removed')) {
				bookmarked = previous;
				error = 'Gateway returned an unexpected response.';
				return;
			}
			// Reconcile with the server's authoritative answer.
			bookmarked = body.outcome === 'bookmarked';
		} catch (e) {
			// Same stale-response guard for the network-fail
			// branch — don't revert if the user has already
			// moved on.
			if (`${targetKind}|${targetId}` !== startKey) return;
			bookmarked = previous;
			console.error('[heart] toggle failed:', e);
			error = 'Network error — please try again.';
		} finally {
			// Only clear `inFlight` if we're still on the
			// target this toggle was started for. Without
			// this guard, an old request's finally could
			// clear `inFlight` mid-way through a new
			// toggle on a different target, falsely
			// re-enabling the button.
			if (`${targetKind}|${targetId}` === startKey) {
				inFlight = false;
			}
		}
	}

	const sizeClasses = $derived(size === 'md' ? 'h-6 w-6' : 'h-5 w-5');
	const labelText = $derived(
		currentUserId === null ? 'Sign in to bookmark' : bookmarked ? 'Remove bookmark' : 'Add bookmark'
	);
</script>

<button
	type="button"
	onclick={toggle}
	disabled={currentUserId === null || inFlight}
	aria-pressed={bookmarked}
	aria-label={labelText}
	title={error ?? labelText}
	class={`inline-flex items-center justify-center rounded-md p-1 transition focus-visible:ring-2 focus-visible:ring-primary-500 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50 ${
		bookmarked ? 'text-rose-500 hover:text-rose-600' : 'text-neutral-400 hover:text-rose-500'
	}`}
>
	<svg
		class={sizeClasses}
		viewBox="0 0 24 24"
		fill={bookmarked ? 'currentColor' : 'none'}
		stroke="currentColor"
		stroke-width="2"
		stroke-linecap="round"
		stroke-linejoin="round"
		aria-hidden="true"
	>
		<!-- Heart path; fill toggles between hollow / filled. -->
		<path
			d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"
		/>
	</svg>
</button>
