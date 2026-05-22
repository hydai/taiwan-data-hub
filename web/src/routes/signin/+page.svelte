<!--
	Placeholder sign-in page (#4.8). The header CTA needs a
	destination route so `resolve('/signin')` typechecks AND so
	visitors don't land on SvelteKit's default 404 page. The
	functional sign-in form (email + password, OAuth, magic
	link) ships in the follow-up that wires the login / logout
	gateway endpoints; this stub just acknowledges the route
	exists.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
</script>

<svelte:head>
	<title>Sign in — Taiwan Data Hub</title>
	<meta
		name="description"
		content="Sign in to Taiwan Data Hub. The interactive form ships alongside the gateway login endpoints."
	/>
</svelte:head>

<section class="mx-auto max-w-md px-4 py-16 sm:px-6 lg:px-8">
	<h1 class="text-2xl font-semibold tracking-tight">Sign in</h1>
	<p class="text-muted-foreground mt-4 text-sm">
		The interactive sign-in form (email + password, OAuth, magic link) is being wired up alongside
		the gateway's login endpoints. Until that ships, programmatic access works via API keys — see
		the
		<a href={resolve('/account')} class="underline">Account page</a>
		to manage them.
	</p>
	<p class="text-muted-foreground mt-4 text-sm">
		<a href={resolve('/')} class="underline">Back to the marketplace</a>
	</p>
</section>
