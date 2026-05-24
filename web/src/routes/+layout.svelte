<script lang="ts">
	import '../app.css';
	import { page } from '$app/state';
	import Footer from '$lib/components/layout/Footer.svelte';
	import Header, { type Locale } from '$lib/components/layout/Header.svelte';
	import MobileMenu from '$lib/components/layout/MobileMenu.svelte';
	import SkipLink from '$lib/components/layout/SkipLink.svelte';
	import { getLocale, setLocale as paraglideSetLocale } from '$lib/paraglide/runtime';

	let { data, children } = $props();

	let isMenuOpen = $state(false);
	const closeMenu = () => (isMenuOpen = false);
	const toggleMenu = () => (isMenuOpen = !isMenuOpen);

	// Locale is derived from Paraglide v2's runtime — the
	// `paraglideMiddleware` in `hooks.server.ts` resolves it
	// (URL prefix > cookie > Accept-Language > base-locale per
	// the chain in `vite.config.ts`) before this component
	// renders.
	//
	// `$derived.by` re-evaluates whenever its tracked deps
	// change. We track `page.url.pathname` because the URL
	// prefix is the only strategy that switches locale across
	// an SPA navigation; cookie + Accept-Language never change
	// mid-session, and base-locale is a build-time constant.
	// Without this dep the <select> would freeze on whatever
	// locale shipped with the initial SSR.
	const locale = $derived.by<Locale>(() => {
		// `void` keeps the read tracked by Svelte's reactivity
		// without ESLint flagging an unused expression.
		void page.url.pathname;
		return getLocale();
	});
	// Paraglide's `setLocale` updates the cookie + triggers
	// navigation so SSR + client agree on the active language.
	// The reactive `locale` above picks up the change on the
	// subsequent re-render — no local mirror needed.
	const setLocale = (next: Locale) => paraglideSetLocale(next);

	// Keep `<html lang>` in sync on client-side navigation.
	// `transformPageChunk` in `hooks.server.ts` only fires
	// during SSR, so an SPA hop between locales would otherwise
	// leave the previous locale's lang attribute on the
	// document. This effect runs on every locale change and
	// nudges the attribute so screen readers + browser auto-
	// translate UIs see the correct value.
	$effect(() => {
		if (typeof document !== 'undefined') {
			document.documentElement.lang = locale;
		}
	});

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
