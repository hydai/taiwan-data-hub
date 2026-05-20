<script lang="ts">
	import '../app.css';
	import Footer from '$lib/components/layout/Footer.svelte';
	import Header from '$lib/components/layout/Header.svelte';
	import MobileMenu from '$lib/components/layout/MobileMenu.svelte';
	import SkipLink from '$lib/components/layout/SkipLink.svelte';

	let { data, children } = $props();

	let isMenuOpen = $state(false);
	const closeMenu = () => (isMenuOpen = false);
	const toggleMenu = () => (isMenuOpen = !isMenuOpen);
</script>

<svelte:head><link rel="icon" href="/favicon.svg" /></svelte:head>

<div class="flex min-h-screen flex-col">
	<SkipLink />
	<Header {isMenuOpen} onToggleMenu={toggleMenu} mode={data.mode} />
	<MobileMenu isOpen={isMenuOpen} onClose={closeMenu} />
	<main id="main" class="flex-1">
		{@render children()}
	</main>
	<Footer />
</div>
