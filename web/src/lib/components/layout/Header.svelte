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
	import type { MeUser } from '$lib/gateway/config';

	export type Locale = 'zh-TW' | 'en';

	type Props = {
		/** Whether the mobile-menu overlay is currently open. */
		isMenuOpen: boolean;
		/** Toggle the overlay open ↔ closed. */
		onToggleMenu: () => void;
		/** Operating mode from the gateway; "personal" hides auth UI. */
		mode: 'personal' | 'multi-user';
		/** Active session's user identity, or `null` for anonymous /
		 * personal-mode requests. When non-null and `mode === 'multi-user'`
		 * the header renders the signed-in cluster (user id chip +
		 * sign-out); when null and `mode === 'multi-user'` it renders
		 * the sign-in / sign-up CTAs. Personal mode ignores this prop. */
		user: MeUser | null;
		/** Currently selected locale — owned by +layout.svelte so the
		 * mobile-menu language picker stays in sync with the desktop one. */
		locale: Locale;
		onLocaleChange: (next: Locale) => void;
	};

	let { isMenuOpen, onToggleMenu, mode, user, locale, onLocaleChange }: Props = $props();

	/** Short, low-information surface for the user id chip — enough
	 * to disambiguate accounts on a shared device without leaking
	 * the full UUID into the DOM. The first 8 hex chars × 2^32 is
	 * plenty for visual disambiguation. */
	const shortUserId = $derived(user ? user.user_id.slice(0, 8) : '');
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

			{#if mode === 'personal'}
				<!-- Personal mode: no auth surface at all. The
				     badge labels the deploy posture for ops /
				     contributors who arrived here from a multi-
				     user link and would otherwise be confused
				     by the missing sign-in button. -->
				<span
					class="rounded-full bg-neutral-100 px-2.5 py-1 text-xs font-medium text-neutral-600"
					title="Set MODE=multi-user on the gateway to enable auth"
				>
					Personal mode
				</span>
			{:else if user}
				<!-- Multi-user, signed in: user-id chip + Account
				     link (to manage API keys) + sign-out form
				     (POSTs to a logout endpoint). The routes are
				     placeholders today — the gateway-side login /
				     logout endpoints ship in a follow-up — but
				     the header surface lands here per the #4.8
				     DoD so the multi-user deploy posture looks
				     complete from the UI. -->
				<a
					href={resolve('/account')}
					class="text-sm font-medium text-neutral-700 hover:text-neutral-900 focus:ring-2 focus:ring-primary-500 focus:outline-none"
				>
					Account
				</a>
				<span
					class="rounded-full bg-primary-50 px-2.5 py-1 font-mono text-xs font-medium text-primary-700"
					title={`Signed in as user ${user.user_id}`}
					data-testid="header-user-chip"
				>
					{shortUserId}…
				</span>
				<form method="POST" action={resolve('/signout')} class="inline">
					<button
						type="submit"
						class="text-sm font-medium text-neutral-600 hover:text-neutral-900 focus:ring-2 focus:ring-primary-500 focus:outline-none"
					>
						Sign out
					</button>
				</form>
			{:else}
				<!-- Multi-user, anonymous: sign-in / sign-up CTAs.
				     Same caveat as above — the routes are
				     placeholder pages until the gateway login
				     flow lands. -->
				<a
					href={resolve('/signin')}
					class="text-sm font-medium text-neutral-700 hover:text-neutral-900 focus:ring-2 focus:ring-primary-500 focus:outline-none"
				>
					Sign in
				</a>
				<a
					href={resolve('/signup')}
					class="inline-flex items-center rounded-md bg-primary-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-700 focus:ring-2 focus:ring-primary-500 focus:outline-none"
				>
					Sign up
				</a>
			{/if}
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
