<!--
	5-star rating component (#5a.5).

	Three states:
	- Anonymous viewer: stars are read-only, average shown.
	- Signed-in + eligible: hover-to-preview, click-to-set,
	  re-click selected star to withdraw.
	- Signed-in + too-young account: stars read-only with a
	  "wait until your account is 24h old" hint.

	Mirrors the HeartButton patterns from #5a.4 — re-seeds
	on (kind, id) change so SvelteKit's same-instance reuse
	across /datasets/A → /datasets/B doesn't bleed state,
	and drops stale responses if the user navigates mid-
	flight.
-->
<script lang="ts">
	import { SCORE_MAX, SCORE_MIN, type RatingTargetKind } from '$lib/ratings/types';
	import { ratingByTargetUrl, ratingsUrl } from '$lib/ratings/gateway';
	import type { RatingView, UpsertRatingResponse } from '$lib/ratings/types';

	let {
		targetKind,
		targetId,
		currentUserId,
		initialView
	}: {
		targetKind: RatingTargetKind;
		targetId: string;
		currentUserId: string | null;
		initialView: RatingView;
	} = $props();

	// IIFE-wrapped seeds for the same reason HeartButton
	// uses them — the `$effect` below resyncs on route
	// navigation.
	let avgScore = $state((() => initialView.avg_score)());
	let count = $state((() => initialView.count)());
	let viewerScore = $state((() => initialView.viewer_score)());
	let hoverScore = $state<number | null>(null);
	let inFlight = $state(false);
	let error = $state<string | null>(null);

	let lastTargetKey = '';
	$effect(() => {
		const key = `${targetKind}|${targetId}`;
		if (key === lastTargetKey) return;
		lastTargetKey = key;
		avgScore = initialView.avg_score;
		count = initialView.count;
		viewerScore = initialView.viewer_score;
		hoverScore = null;
		error = null;
		inFlight = false;
	});

	const interactive = $derived(currentUserId !== null);
	const displayScore = $derived(hoverScore ?? viewerScore ?? Math.round(avgScore ?? 0));

	function clamp(n: number): number {
		return Math.max(SCORE_MIN, Math.min(SCORE_MAX, Math.round(n)));
	}

	async function pickScore(n: number): Promise<void> {
		if (!interactive || inFlight) return;
		const startKey = `${targetKind}|${targetId}`;
		const newScore = clamp(n);
		// Re-clicking the currently-rated star withdraws.
		const isWithdraw = viewerScore === newScore;
		const previousViewer = viewerScore;
		const previousAvg = avgScore;
		const previousCount = count;
		viewerScore = isWithdraw ? null : newScore;
		// Optimistic aggregate update — recomputed on
		// reconcile when the server returns the canonical
		// view in a follow-up GET. Keep it simple: don't
		// guess avg, just nudge count.
		count = isWithdraw
			? Math.max(0, previousCount - (previousViewer !== null ? 1 : 0))
			: previousCount + (previousViewer === null ? 1 : 0);
		inFlight = true;
		error = null;
		try {
			const res = isWithdraw
				? await fetch(ratingByTargetUrl(targetKind, targetId), {
						method: 'DELETE',
						credentials: 'include',
						headers: { accept: 'application/json' }
					})
				: await fetch(ratingsUrl(), {
						method: 'POST',
						credentials: 'include',
						headers: {
							accept: 'application/json',
							'content-type': 'application/json'
						},
						body: JSON.stringify({
							target_kind: targetKind,
							target_id: targetId,
							score: newScore
						})
					});
			// Stale-target guard.
			if (`${targetKind}|${targetId}` !== startKey) return;
			if (!res.ok) {
				viewerScore = previousViewer;
				avgScore = previousAvg;
				count = previousCount;
				const body = (await res.json().catch(() => null)) as {
					error?: string;
					message?: string;
				} | null;
				if (res.status === 403 && body?.error === 'account_too_new') {
					error = 'Ratings are unlocked 24h after sign-up.';
				} else if (res.status === 401) {
					error = 'Please sign in again.';
				} else {
					error = body?.message ?? `Failed to save rating (${res.status}).`;
				}
				return;
			}
			if (!isWithdraw) {
				const body = (await res.json().catch(() => null)) as UpsertRatingResponse | null;
				if (body && typeof body.score === 'number') {
					viewerScore = body.score;
				}
			}
			// Refresh the aggregate from the canonical view
			// so the optimistic count + avg settle to the
			// server's truth.
			await refreshView(startKey);
		} catch (e) {
			if (`${targetKind}|${targetId}` !== startKey) return;
			viewerScore = previousViewer;
			avgScore = previousAvg;
			count = previousCount;
			console.error('[stars] toggle failed:', e);
			error = 'Network error — please try again.';
		} finally {
			if (`${targetKind}|${targetId}` === startKey) {
				inFlight = false;
			}
		}
	}

	async function refreshView(startKey: string): Promise<void> {
		try {
			const res = await fetch(ratingByTargetUrl(targetKind, targetId), {
				method: 'GET',
				credentials: 'include',
				headers: { accept: 'application/json' }
			});
			if (`${targetKind}|${targetId}` !== startKey) return;
			if (!res.ok) return;
			const body = (await res.json().catch(() => null)) as RatingView | null;
			if (body === null) return;
			avgScore = body.avg_score;
			count = body.count;
			viewerScore = body.viewer_score;
		} catch (e) {
			console.error('[stars] refresh failed:', e);
		}
	}
</script>

<div class="flex items-center gap-2">
	<div
		role={interactive ? 'radiogroup' : undefined}
		aria-label="Rate this dataset"
		class="inline-flex items-center"
	>
		{#each [1, 2, 3, 4, 5] as star (star)}
			{@const filled = star <= displayScore}
			<button
				type="button"
				role={interactive ? 'radio' : undefined}
				aria-checked={interactive ? viewerScore === star : undefined}
				aria-label={`${star} ${star === 1 ? 'star' : 'stars'}`}
				disabled={!interactive || inFlight}
				onclick={() => pickScore(star)}
				onmouseenter={() => interactive && (hoverScore = star)}
				onmouseleave={() => (hoverScore = null)}
				onfocus={() => interactive && (hoverScore = star)}
				onblur={() => (hoverScore = null)}
				class={`p-0.5 transition focus-visible:ring-2 focus-visible:ring-amber-400 focus-visible:outline-none ${
					interactive ? 'cursor-pointer hover:scale-110' : 'cursor-default disabled:opacity-100'
				}`}
			>
				<svg
					class="h-5 w-5"
					viewBox="0 0 24 24"
					fill={filled ? 'currentColor' : 'none'}
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
					aria-hidden="true"
					class:text-amber-400={filled}
					class:text-neutral-300={!filled}
				>
					<path d="M12 2 14.39 8.36H21l-5.3 4.04 2 6.6L12 15.27l-5.7 3.73 2-6.6L3 8.36h6.61z" />
				</svg>
			</button>
		{/each}
	</div>
	<span class="text-sm text-neutral-600">
		{#if count > 0 && avgScore !== null}
			{avgScore.toFixed(2)} · {count}
			{count === 1 ? 'rating' : 'ratings'}
		{:else}
			No ratings yet
		{/if}
	</span>
	<!--
		Screen-reader announcement on rating failure / state
		change. Mirrors the HeartButton pattern: visually
		hidden, `role="alert"` + `aria-live="polite"` so a
		failure lands without interrupting a screen reader's
		current speech.
	-->
	<span class="sr-only" role="alert" aria-live="polite">
		{#if error}{error}{/if}
	</span>
</div>
