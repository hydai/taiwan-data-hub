<script lang="ts">
	import '../app.css';
	import Footer from '$lib/components/layout/Footer.svelte';
	import Header, { type Locale } from '$lib/components/layout/Header.svelte';
	import MobileMenu from '$lib/components/layout/MobileMenu.svelte';
	import SkipLink from '$lib/components/layout/SkipLink.svelte';
	import { getLocale, setLocale as paraglideSetLocale } from '$lib/paraglide/runtime';

	let { data, children } = $props();

	let isMenuOpen = $state(false);
	const closeMenu = () => (isMenuOpen = false);
	const toggleMenu = () => (isMenuOpen = !isMenuOpen);

	// Locale is read straight from Paraglide v2's runtime — the
	// `paraglideMiddleware` in `hooks.server.ts` resolves it from the
	// URL prefix > cookie > Accept-Language > base-locale chain
	// (configured in `vite.config.ts`) before this component renders.
	// We mirror it into a `$state` so the desktop / mobile <select>
	// elements can both bind to the same value.
	let locale = $state<Locale>(getLocale());
	// Paraglide's `setLocale` updates the cookie + reloads navigation
	// so SSR + client agree on the active language. The local mirror
	// is kept in sync optimistically so the UI doesn't flash the old
	// value during the navigation.
	const setLocale = (next: Locale) => {
		locale = next;
		paraglideSetLocale(next);
	};

	// If the user opens the burger menu on mobile and then resizes /
	// rotates to ≥md, MobileMenu hides itself via `md:hidden` but its
	// `$effect` (body-scroll lock + ESC/Tab trap) stays armed because
	// isMenuOpen is still true. Close the menu when crossing into the
	// desktop breakpoint so scroll lock is released and the burger
	// returns to a clean state on rotate-back-to-mobile.
	//
	// The effect reads no $state (closeMenu only writes), so Svelte
	// tracks zero deps and the listener mounts/unmounts once. 768px is
	// the Tailwind 4 `md` default; keep these in lockstep.
	$effect(() => {
		const mql = window.matchMedia('(min-width: 768px)');
		const handler = (e: MediaQueryListEvent) => {
			if (e.matches) closeMenu();
		};
		mql.addEventListener('change', handler);
		return () => mql.removeEventListener('change', handler);
	});
</script>

<svelte:head><link rel="icon" href="/favicon.svg" /></svelte:head>

<div class="flex min-h-screen flex-col">
	<SkipLink />
	<Header
		{isMenuOpen}
		onToggleMenu={toggleMenu}
		mode={data.mode}
		user={data.user}
		{locale}
		onLocaleChange={setLocale}
	/>
	<MobileMenu isOpen={isMenuOpen} onClose={closeMenu} {locale} onLocaleChange={setLocale} />
	<main id="main" class="flex-1">
		{@render children()}
	</main>
	<Footer />
</div>
