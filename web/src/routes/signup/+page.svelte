<!--
	Placeholder sign-up page (#4.8). Same rationale as
	`/signin`: the header CTA needs a destination route so
	`resolve('/signup')` typechecks AND visitors don't hit
	SvelteKit's default 404. The interactive form (with email
	verification + password strength meter + ToS opt-in) ships
	alongside the gateway signup endpoint in the follow-up.
-->
<script lang="ts">
	import { resolve } from '$app/paths';
</script>

<svelte:head>
	<title>Sign up — Taiwan Data Hub</title>
	<meta
		name="description"
		content="Sign up for Taiwan Data Hub. The interactive form ships alongside the gateway signup endpoint."
	/>
</svelte:head>

<section class="mx-auto max-w-md px-4 py-16 sm:px-6 lg:px-8">
	<h1 class="text-2xl font-semibold tracking-tight">Sign up</h1>
	<p class="text-muted-foreground mt-4 text-sm">
		The interactive sign-up form is being wired up alongside the gateway's signup endpoint. The
		marketplace itself works without an account — only contributions, favourites, and API key
		issuance require sign-up.
	</p>
	<p class="text-muted-foreground mt-4 text-sm">
		<a href={resolve('/')} class="underline">Back to the marketplace</a>
	</p>
</section>
