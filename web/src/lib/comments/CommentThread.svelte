<!--
	Reusable comment thread (#5a.3).

	Renders a flat list of comments grouped into parent → reply
	pairs (depth ≤ 1). Supports posting a new root comment, a
	reply on any root, in-place edit within the 5-minute window,
	and soft-delete via the gateway.

	Authentication state comes via the `currentUserId` prop:
	when `null`, the comment-entry form is rendered disabled
	with a "Sign in to comment" placeholder, and the per-row
	edit / delete / reply affordances are hidden. The
	component fetches the thread itself on mount and after
	each mutation so a logged-out reader still sees the
	latest state.

	All gateway calls are same-origin (`/api/v1/comments…`) so
	the host-only session cookie is sent automatically and the
	internal gateway URL never reaches the browser. The reverse
	proxy (Caddy in prod, vite proxy in dev) routes `/api/v1/*`
	to the gateway.
-->
<script lang="ts">
	import { onMount } from 'svelte';
	import type { CommentTargetKind, RenderedComment } from '$lib/comments/types';
	import {
		commentByIdUrl,
		commentsListUrl,
		commentsUrl,
		parseRenderedComment,
		parseRenderedCommentArray
	} from '$lib/comments/gateway';
	import { MAX_COMMENT_BODY_LEN } from '$lib/comments/types';
	import ReportButton from '$lib/reports/ReportButton.svelte';

	let {
		targetKind,
		targetId,
		currentUserId
	}: {
		targetKind: CommentTargetKind;
		targetId: string;
		/** `null` for logged-out readers. */
		currentUserId: string | null;
	} = $props();

	type ThreadState =
		| { state: 'loading' }
		| { state: 'ok'; comments: RenderedComment[] }
		// Gateway returns 404: the comments subrouter is not
		// mounted (personal-mode deployment). The component
		// hides the section rather than surfacing it as an
		// error — the deployment intentionally has no
		// comments.
		| { state: 'disabled' }
		| { state: 'error'; message: string };

	let thread = $state<ThreadState>({ state: 'loading' });

	// Form state for posting a new root or reply. `replyParent`
	// is the id of the comment we're replying to (null = post a
	// root).
	let newBody = $state('');
	let replyParent = $state<string | null>(null);
	let submitError = $state<string | null>(null);
	let submitting = $state(false);

	// Edit state — track which comment is currently being
	// edited and its draft body.
	let editingId = $state<string | null>(null);
	let editDraft = $state('');
	let editError = $state<string | null>(null);
	let editSaving = $state(false);

	const fiveMinutes = 5 * 60 * 1000;
	const isWithinEditWindow = (c: RenderedComment): boolean =>
		// Inclusive boundary to match the backend's
		// `now - created_at <= window` SQL predicate. Without
		// `>=`, the Edit UI would vanish at exactly the
		// 5-minute mark even though the server would still
		// accept the request.
		!c.is_deleted && Date.parse(c.created_at) >= Date.now() - fiveMinutes;
	const canMutate = (c: RenderedComment): boolean =>
		currentUserId !== null && c.user_id === currentUserId && !c.is_deleted && !c.is_hidden;

	async function loadThread(): Promise<void> {
		thread = { state: 'loading' };
		try {
			const res = await fetch(commentsListUrl(targetKind, targetId), {
				method: 'GET',
				headers: { accept: 'application/json' },
				credentials: 'include'
			});
			if (res.status === 404) {
				// Personal-mode / mis-configured deploy: the
				// gateway didn't mount the comments
				// subrouter. Hide the section instead of
				// showing a misleading "failed to load" error.
				thread = { state: 'disabled' };
				return;
			}
			if (!res.ok) {
				thread = {
					state: 'error',
					message: `Failed to load comments (${res.status}).`
				};
				return;
			}
			const parsed = parseRenderedCommentArray(await res.json().catch(() => null));
			if (parsed === null) {
				thread = { state: 'error', message: 'Gateway returned an unexpected response.' };
				return;
			}
			thread = { state: 'ok', comments: parsed };
		} catch (e) {
			console.error('[comments] load failed:', e);
			thread = { state: 'error', message: 'Could not reach the gateway.' };
		}
	}

	onMount(loadThread);

	async function submitNew(): Promise<void> {
		if (currentUserId === null) {
			submitError = 'Please sign in to comment.';
			return;
		}
		const trimmed = newBody.trim();
		if (trimmed.length === 0) {
			submitError = 'Comment cannot be empty.';
			return;
		}
		if ([...trimmed].length > MAX_COMMENT_BODY_LEN) {
			submitError = `Comment exceeds the ${MAX_COMMENT_BODY_LEN}-character limit.`;
			return;
		}
		submitting = true;
		submitError = null;
		try {
			const res = await fetch(commentsUrl(), {
				method: 'POST',
				headers: {
					accept: 'application/json',
					'content-type': 'application/json'
				},
				credentials: 'include',
				body: JSON.stringify({
					target_kind: targetKind,
					target_id: targetId,
					parent_id: replyParent,
					body_md: trimmed
				})
			});
			if (!res.ok) {
				const detail = await res.json().catch(() => null);
				submitError = (detail as { message?: string })?.message ?? 'Failed to post comment.';
				return;
			}
			newBody = '';
			replyParent = null;
			await loadThread();
		} catch (e) {
			console.error('[comments] submit failed:', e);
			submitError = 'Network error — please try again.';
		} finally {
			submitting = false;
		}
	}

	function startEditing(c: RenderedComment): void {
		// Refuse to open a second edit while a save is still
		// in flight — otherwise the in-flight PATCH would
		// resolve into `cancelEditing()` and wipe the user's
		// new draft. The button itself is gated below.
		if (editSaving) return;
		editingId = c.id;
		editDraft = c.body_md ?? '';
		editError = null;
	}

	function cancelEditing(): void {
		editingId = null;
		editDraft = '';
		editError = null;
	}

	async function submitEdit(): Promise<void> {
		if (editingId === null || editSaving) return;
		// Reset the error each attempt so a successful retry
		// after a transient failure replaces the prior banner
		// (and so the in-flight state is honest).
		editError = null;
		const trimmed = editDraft.trim();
		if (trimmed.length === 0) {
			editError = 'Comment cannot be empty.';
			return;
		}
		// Unicode scalar count matches the server's
		// `chars().count()` cap — keeps the client cap
		// consistent with `submitNew`.
		if ([...trimmed].length > MAX_COMMENT_BODY_LEN) {
			editError = `Comment exceeds the ${MAX_COMMENT_BODY_LEN}-character limit.`;
			return;
		}
		editSaving = true;
		try {
			const res = await fetch(commentByIdUrl(editingId), {
				method: 'PATCH',
				headers: {
					accept: 'application/json',
					'content-type': 'application/json'
				},
				credentials: 'include',
				body: JSON.stringify({ body_md: trimmed })
			});
			if (!res.ok) {
				const detail = await res.json().catch(() => null);
				editError = (detail as { message?: string })?.message ?? 'Failed to save edit.';
				return;
			}
			const updated = parseRenderedComment(await res.json().catch(() => null));
			if (updated === null) {
				editError = 'Gateway returned an unexpected response.';
				return;
			}
			cancelEditing();
			await loadThread();
		} catch (e) {
			console.error('[comments] edit failed:', e);
			editError = 'Network error — please try again.';
		} finally {
			editSaving = false;
		}
	}

	async function deleteComment(c: RenderedComment): Promise<void> {
		if (!canMutate(c)) return;
		// Pure best-effort confirm; the server reconfirms ownership.
		const ok = window.confirm('Delete this comment?');
		if (!ok) return;
		try {
			const res = await fetch(commentByIdUrl(c.id), {
				method: 'DELETE',
				headers: { accept: 'application/json' },
				credentials: 'include'
			});
			if (!res.ok) {
				console.error('[comments] delete failed with status', res.status);
				return;
			}
			await loadThread();
		} catch (e) {
			console.error('[comments] delete failed:', e);
		}
	}

	// Build the parent→reply structure once per load. Comments
	// arrive sorted by `created_at ASC` so the chronological
	// order is preserved within each group.
	const groups = $derived(thread.state === 'ok' ? buildGroups(thread.comments) : []);

	type CommentGroup = {
		root: RenderedComment;
		replies: RenderedComment[];
	};

	function buildGroups(flat: RenderedComment[]): CommentGroup[] {
		// Plain object instead of Map to keep the Svelte
		// `prefer-svelte-reactivity` lint happy — this is a
		// transient pure-function structure with no reactivity
		// needs, but the rule still fires on `new Map(...)`.
		const roots: Record<string, CommentGroup> = {};
		for (const c of flat) {
			if (c.depth === 0) {
				roots[c.id] = { root: c, replies: [] };
			}
		}
		for (const c of flat) {
			if (c.depth === 1 && c.parent_id !== undefined) {
				const g = roots[c.parent_id];
				if (g !== undefined) g.replies.push(c);
			}
		}
		return Object.values(roots);
	}

	function formatDate(iso: string): string {
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
	}
</script>

<!-- Hide the whole section in personal-mode deploys
     where the comments subrouter isn't mounted. -->
{#if thread.state !== 'disabled'}
	<section class="mt-8 border-t border-neutral-200 pt-6" data-testid="comments-thread">
		<h2 class="mb-4 text-lg font-semibold tracking-tight">Comments</h2>

		{#if thread.state === 'loading'}
			<p class="text-sm text-neutral-500">Loading comments…</p>
		{:else if thread.state === 'error'}
			<p class="text-danger-600 text-sm" role="alert">{thread.message}</p>
		{:else if groups.length === 0}
			<p class="text-sm text-neutral-500">No comments yet — be the first to share something.</p>
		{:else}
			<ol class="space-y-4">
				{#each groups as group (group.root.id)}
					<li class="rounded-md border border-neutral-200 bg-white p-4">
						{#if editingId === group.root.id}
							<form
								class="space-y-2"
								onsubmit={(e) => {
									e.preventDefault();
									submitEdit();
								}}
							>
								<textarea
									bind:value={editDraft}
									rows="3"
									aria-label="Edit comment"
									class="w-full rounded-md border border-neutral-300 p-2 text-sm focus-visible:ring-2 focus-visible:ring-primary-500 focus-visible:outline-none"
								></textarea>
								{#if editError}
									<p class="text-danger-600 text-xs">{editError}</p>
								{/if}
								<div class="flex items-center gap-2">
									<button
										type="submit"
										disabled={editSaving}
										class="rounded-md bg-primary-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-primary-700 disabled:cursor-not-allowed disabled:opacity-50"
										>{editSaving ? 'Saving…' : 'Save'}</button
									>
									<button
										type="button"
										onclick={cancelEditing}
										class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs hover:bg-neutral-50"
										>Cancel</button
									>
								</div>
							</form>
						{:else}
							<div class="prose prose-sm max-w-none">
								<!-- The HTML is server-rendered via comrak +
							     ammonia; the sanitizer is the load-bearing
							     XSS guard.  -->
								<!-- eslint-disable-next-line svelte/no-at-html-tags -->
								{@html group.root.body_html}
							</div>
							<p class="mt-2 flex items-center gap-2 text-xs text-neutral-500">
								<span>{formatDate(group.root.created_at)}</span>
								{#if group.root.edited_at}
									<span aria-label="edited" title="Edited">(edited)</span>
								{/if}
								{#if canMutate(group.root) && isWithinEditWindow(group.root)}
									<button
										type="button"
										onclick={() => startEditing(group.root)}
										disabled={editSaving}
										class="underline underline-offset-2 hover:text-neutral-700 disabled:cursor-not-allowed disabled:opacity-50"
										>Edit</button
									>
								{/if}
								{#if canMutate(group.root)}
									<button
										type="button"
										onclick={() => deleteComment(group.root)}
										class="underline underline-offset-2 hover:text-neutral-700">Delete</button
									>
								{/if}
								{#if currentUserId !== null && !group.root.is_deleted && !group.root.is_hidden}
									<button
										type="button"
										onclick={() => {
											replyParent = group.root.id;
											newBody = '';
											submitError = null;
										}}
										class="underline underline-offset-2 hover:text-neutral-700">Reply</button
									>
								{/if}
								{#if currentUserId !== null && currentUserId !== group.root.user_id && !group.root.is_deleted && !group.root.is_hidden}
									<ReportButton
										targetKind="comment"
										targetId={group.root.id}
										onReported={(r) => {
											if (r.freshly_hidden) {
												group.root.is_hidden = true;
												group.root.body_html = '<p>[hidden by community reports]</p>';
											}
										}}
									/>
								{/if}
							</p>
						{/if}

						{#if group.replies.length > 0}
							<ol class="mt-3 space-y-3 border-l border-neutral-200 pl-4">
								{#each group.replies as reply (reply.id)}
									<li>
										{#if editingId === reply.id}
											<form
												class="space-y-2"
												onsubmit={(e) => {
													e.preventDefault();
													submitEdit();
												}}
											>
												<textarea
													bind:value={editDraft}
													rows="2"
													aria-label="Edit reply"
													class="w-full rounded-md border border-neutral-300 p-2 text-sm focus-visible:ring-2 focus-visible:ring-primary-500 focus-visible:outline-none"
												></textarea>
												{#if editError}
													<p class="text-danger-600 text-xs">{editError}</p>
												{/if}
												<div class="flex items-center gap-2">
													<button
														type="submit"
														disabled={editSaving}
														class="rounded-md bg-primary-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-primary-700 disabled:cursor-not-allowed disabled:opacity-50"
														>{editSaving ? 'Saving…' : 'Save'}</button
													>
													<button
														type="button"
														onclick={cancelEditing}
														class="rounded-md border border-neutral-300 px-3 py-1.5 text-xs hover:bg-neutral-50"
														>Cancel</button
													>
												</div>
											</form>
										{:else}
											<div class="prose prose-sm max-w-none">
												<!-- eslint-disable-next-line svelte/no-at-html-tags -->
												{@html reply.body_html}
											</div>
											<p class="mt-1 flex items-center gap-2 text-xs text-neutral-500">
												<span>{formatDate(reply.created_at)}</span>
												{#if reply.edited_at}
													<span aria-label="edited" title="Edited">(edited)</span>
												{/if}
												{#if canMutate(reply) && isWithinEditWindow(reply)}
													<button
														type="button"
														onclick={() => startEditing(reply)}
														disabled={editSaving}
														class="underline underline-offset-2 hover:text-neutral-700 disabled:cursor-not-allowed disabled:opacity-50"
														>Edit</button
													>
												{/if}
												{#if canMutate(reply)}
													<button
														type="button"
														onclick={() => deleteComment(reply)}
														class="underline underline-offset-2 hover:text-neutral-700"
														>Delete</button
													>
												{/if}
												{#if currentUserId !== null && currentUserId !== reply.user_id && !reply.is_deleted && !reply.is_hidden}
													<ReportButton
														targetKind="comment"
														targetId={reply.id}
														onReported={(r) => {
															if (r.freshly_hidden) {
																reply.is_hidden = true;
																reply.body_html = '<p>[hidden by community reports]</p>';
															}
														}}
													/>
												{/if}
											</p>
										{/if}
									</li>
								{/each}
							</ol>
						{/if}
					</li>
				{/each}
			</ol>
		{/if}

		<form
			class="mt-6 space-y-2"
			onsubmit={(e) => {
				e.preventDefault();
				submitNew();
			}}
		>
			{#if replyParent !== null}
				<p class="text-xs text-neutral-500">
					Replying to a comment.
					<button
						type="button"
						onclick={() => {
							replyParent = null;
						}}
						class="underline underline-offset-2 hover:text-neutral-700">Cancel</button
					>
				</p>
			{/if}
			<label class="block">
				<span class="sr-only">New comment</span>
				<textarea
					bind:value={newBody}
					rows="3"
					placeholder={currentUserId === null
						? 'Sign in to comment'
						: 'Write a comment (Markdown supported)…'}
					disabled={currentUserId === null}
					class="w-full rounded-md border border-neutral-300 p-2 text-sm focus-visible:ring-2 focus-visible:ring-primary-500 focus-visible:outline-none disabled:bg-neutral-100"
				></textarea>
			</label>
			{#if submitError}
				<p class="text-danger-600 text-xs" role="alert">{submitError}</p>
			{/if}
			<button
				type="submit"
				disabled={currentUserId === null || submitting}
				class="rounded-md bg-primary-600 px-4 py-2 text-sm font-medium text-white hover:bg-primary-700 disabled:cursor-not-allowed disabled:opacity-50"
				>{submitting ? 'Posting…' : replyParent === null ? 'Post comment' : 'Post reply'}</button
			>
		</form>
	</section>
{/if}
