<!--
	Mobile navigation overlay — slide-in panel from the right that
	covers the viewport on screens below the `md` breakpoint. Opened by
	the burger button in Header.svelte; both sides read the same
	`isOpen` state from +layout.svelte.

	Accessibility wiring:
	  - role="dialog" + aria-modal so screen readers announce a modal
	    context (Lighthouse a11y signal)
	  - ESC key closes (`svelte:window onkeydown`)
	  - Backdrop click closes
	  - Body scroll is locked while open so the page underneath cannot
	    scroll on iOS Safari
	  - First focusable element receives focus on open; focus is trapped
	    inside the panel via Tab/Shift-Tab; previously focused element
	    is restored when the panel closes

	Focus trap is hand-rolled rather than reaching for bits-ui's Dialog
	primitive: the surface is tiny and the trap is ~25 lines of code,
	while bits-ui would drag a ~30-file dependency tree into the bundle
	for a single in-app use site.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { navLinks } from '$lib/components/layout/nav-links';
	import type { Locale } from '$lib/components/layout/Header.svelte';
	import { cn } from '$lib/utils';
	import { deLocalizeUrl } from '$lib/paraglide/runtime';
	import { fade, fly } from 'svelte/transition';

	type Props = {
		isOpen: boolean;
		onClose: () => void;
		locale: Locale;
		onLocaleChange: (next: Locale) => void;
	};

	let { isOpen, onClose, locale, onLocaleChange }: Props = $props();

	let panel: HTMLDivElement | undefined = $state();

	function focusableElements(root: HTMLElement): HTMLElement[] {
		return Array.from(
			root.querySelectorAll<HTMLElement>(
				'a[href], button:not([disabled]), select, [tabindex]:not([tabindex="-1"])'
			)
		).filter((el) => !el.hasAttribute('inert'));
	}

	function handleKeydown(event: KeyboardEvent) {
		if (!isOpen) return;
		if (event.key === 'Escape') {
			event.preventDefault();
			onClose();
			return;
		}
		if (event.key !== 'Tab' || !panel) return;
		const focusables = focusableElements(panel);
		if (focusables.length === 0) return;
		const first = focusables[0];
		const last = focusables[focusables.length - 1];
		const active = document.activeElement;
		if (event.shiftKey && active === first) {
			event.preventDefault();
			last.focus();
		} else if (!event.shiftKey && active === last) {
			event.preventDefault();
			first.focus();
		}
	}

	// Lock body scroll, save/restore focus across open transitions.
	$effect(() => {
		if (!isOpen || !panel) return;
		const previouslyFocused = document.activeElement as HTMLElement | null;
		const originalOverflow = document.body.style.overflow;
		document.body.style.overflow = 'hidden';
		const focusables = focusableElements(panel);
		focusables[0]?.focus();
		return () => {
			document.body.style.overflow = originalOverflow;
			previouslyFocused?.focus();
		};
	});
</script>

<svelte:window onkeydown={handleKeydown} />

{#if isOpen}
	<div
		id="mobile-menu"
		role="dialog"
		aria-modal="true"
		aria-label="Mobile navigation"
		class="fixed inset-0 z-50 md:hidden"
	>
		<!-- Backdrop is mouse-only dismissal — keyboard users have ESC
		     and the close button inside the panel, so the backdrop is
		     hidden from the a11y tree and only carries a click handler.
		     aria-hidden="true" already exempts this from Svelte's a11y
		     warnings about onclick on a static element. -->
		<div
			aria-hidden="true"
			onclick={onClose}
			transition:fade={{ duration: 150 }}
			class="absolute inset-0 bg-neutral-950/40"
		></div>

		<div
			bind:this={panel}
			transition:fly={{ x: 320, duration: 200 }}
			class="absolute inset-y-0 right-0 flex w-72 max-w-[80%] flex-col bg-neutral-50 shadow-xl"
		>
			<div class="flex items-center justify-between border-b border-neutral-200 px-5 py-4">
				<span class="text-sm font-medium text-neutral-700">Menu</span>
				<button
					type="button"
					onclick={onClose}
					aria-label="Close menu"
					class="inline-flex h-10 w-10 items-center justify-center rounded-md text-neutral-700 hover:bg-neutral-100 focus:ring-2 focus:ring-primary-500 focus:outline-none"
				>
					<svg
						viewBox="0 0 24 24"
						class="h-5 w-5"
						fill="none"
						stroke="currentColor"
						stroke-width="2"
						aria-hidden="true"
					>
						<path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
					</svg>
				</button>
			</div>

			<nav aria-label="Mobile" class="flex-1 space-y-1 px-3 py-4">
				{#each navLinks as link (link.href)}
					<!-- Same `deLocalizeUrl` rationale as Header.svelte. -->
					{@const active = deLocalizeUrl(page.url).pathname.startsWith(link.href)}
					<a
						href={resolve(link.href)}
						onclick={onClose}
						class={cn(
							'block rounded-md px-3 py-2 text-base font-medium focus:ring-2 focus:ring-primary-500 focus:outline-none',
							active ? 'bg-primary-50 text-primary-700' : 'text-neutral-700 hover:bg-neutral-100'
						)}
						aria-current={active ? 'page' : undefined}
					>
						{link.label}
					</a>
				{/each}
			</nav>

			<div class="space-y-3 border-t border-neutral-200 p-5">
				<div class="flex items-center justify-between">
					<label class="text-sm font-medium text-neutral-700" for="locale-mobile">Language</label>
					<select
						id="locale-mobile"
						value={locale}
						onchange={(e) => onLocaleChange(e.currentTarget.value as Locale)}
						class="rounded-md border border-neutral-200 bg-neutral-50 px-2 py-1 text-sm text-neutral-700 focus:ring-2 focus:ring-primary-500 focus:outline-none"
					>
						<option value="zh-TW">繁中</option>
						<option value="en">EN</option>
						<option value="ja">日本語</option>
						<option value="ko">한국어</option>
						<option value="fr">Français</option>
					</select>
				</div>
				<p class="text-xs text-neutral-500">Auth UI lands in M4.</p>
			</div>
		</div>
	</div>
{/if}
