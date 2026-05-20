<!--
	Sticky top header: logo, primary nav, locale switcher, auth status,
	mobile burger button.

	Three breakpoints (sm/md/lg):
	  - sm  (< md)   : logo + burger only; nav lives in MobileMenu
	  - md  (≥ 768)  : full nav + locale + auth cluster visible
	  - lg  (≥ 1024) : same as md but with extra horizontal padding

	The open/closed state of the mobile menu lives in +layout.svelte and
	is passed in as props so the burger button (here) and the overlay
	(MobileMenu.svelte) stay in sync without context plumbing.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { navLinks } from '$lib/components/layout/nav-links';
	import { cn } from '$lib/utils';

	export type Locale = 'zh-TW' | 'en';

	type Props = {
		/** Whether the mobile-menu overlay is currently open. */
		isMenuOpen: boolean;
		/** Toggle the overlay open ↔ closed. */
		onToggleMenu: () => void;
		/** Operating mode from the gateway; "personal" hides auth UI. */
		mode: 'personal' | 'multi-user';
		/** Currently selected locale — owned by +layout.svelte so the
		 * mobile-menu language picker stays in sync with the desktop one. */
		locale: Locale;
		onLocaleChange: (next: Locale) => void;
	};

	let { isMenuOpen, onToggleMenu, mode, locale, onLocaleChange }: Props = $props();
</script>

<header
	class="sticky top-0 z-40 border-b border-neutral-200 bg-neutral-50/95 backdrop-blur supports-[backdrop-filter]:bg-neutral-50/80"
>
	<div class="mx-auto flex h-16 max-w-7xl items-center justify-between px-4 sm:px-6 lg:px-8">
		<a
			href={resolve('/')}
			class="text-lg font-bold tracking-tight text-neutral-900 focus:ring-2 focus:ring-primary-500 focus:outline-none"
			aria-label="Taiwan Data Hub — home"
		>
			<span class="text-primary-700">Taiwan</span> Data Hub
		</a>

		<nav aria-label="Main" class="hidden md:flex md:items-center md:gap-6">
			{#each navLinks as link (link.href)}
				{@const active = page.url.pathname.startsWith(link.href)}
				<a
					href={resolve(link.href)}
					class={cn(
						'text-sm font-medium transition-colors focus:ring-2 focus:ring-primary-500 focus:outline-none',
						active ? 'text-primary-700' : 'text-neutral-600 hover:text-neutral-900'
					)}
					aria-current={active ? 'page' : undefined}
				>
					{link.label}
				</a>
			{/each}
		</nav>

		<div class="hidden md:flex md:items-center md:gap-4">
			<label class="sr-only" for="locale">Language</label>
			<select
				id="locale"
				value={locale}
				onchange={(e) => onLocaleChange(e.currentTarget.value as Locale)}
				class="rounded-md border border-neutral-200 bg-neutral-50 px-2 py-1 text-sm text-neutral-700 focus:ring-2 focus:ring-primary-500 focus:outline-none"
			>
				<option value="zh-TW">繁中</option>
				<option value="en">EN</option>
			</select>

			<span
				class="rounded-full bg-neutral-100 px-2.5 py-1 text-xs font-medium text-neutral-600"
				title={mode === 'personal'
					? 'Set MODE=multi-user on the gateway to enable auth'
					: 'Auth UI lands in M4'}
			>
				{mode === 'personal' ? 'Personal mode' : 'Multi-user mode'}
			</span>
		</div>

		<button
			type="button"
			onclick={onToggleMenu}
			aria-label={isMenuOpen ? 'Close menu' : 'Open menu'}
			aria-expanded={isMenuOpen}
			aria-haspopup="dialog"
			class="inline-flex h-10 w-10 items-center justify-center rounded-md text-neutral-700 hover:bg-neutral-100 focus:ring-2 focus:ring-primary-500 focus:outline-none md:hidden"
		>
			{#if isMenuOpen}
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
			{:else}
				<svg
					viewBox="0 0 24 24"
					class="h-5 w-5"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					aria-hidden="true"
				>
					<path stroke-linecap="round" stroke-linejoin="round" d="M4 6h16M4 12h16M4 18h16" />
				</svg>
			{/if}
		</button>
	</div>
</header>
