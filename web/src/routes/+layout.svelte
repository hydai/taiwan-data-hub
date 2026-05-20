<script lang="ts">
	import '../app.css';
	import Footer from '$lib/components/layout/Footer.svelte';
	import Header, { type Locale } from '$lib/components/layout/Header.svelte';
	import MobileMenu from '$lib/components/layout/MobileMenu.svelte';
	import SkipLink from '$lib/components/layout/SkipLink.svelte';

	let { data, children } = $props();

	let isMenuOpen = $state(false);
	const closeMenu = () => (isMenuOpen = false);
	const toggleMenu = () => (isMenuOpen = !isMenuOpen);

	// Locale lives at layout scope so the desktop <select> in Header and
	// the mobile <select> in MobileMenu both bind to the same value. UI
	// is still a placeholder — Paraglide v2's `setLocale()` replaces the
	// raw mutation in #7.x.
	let locale = $state<Locale>('zh-TW');
	const setLocale = (next: Locale) => (locale = next);
</script>

<svelte:head><link rel="icon" href="/favicon.svg" /></svelte:head>

<div class="flex min-h-screen flex-col">
	<SkipLink />
	<Header
		{isMenuOpen}
		onToggleMenu={toggleMenu}
		mode={data.mode}
		{locale}
		onLocaleChange={setLocale}
	/>
	<MobileMenu isOpen={isMenuOpen} onClose={closeMenu} {locale} onLocaleChange={setLocale} />
	<main id="main" class="flex-1">
		{@render children()}
	</main>
	<Footer />
</div>
