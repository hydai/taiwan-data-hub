import adapter from '@sveltejs/adapter-node';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	compilerOptions: {
		// Force Runes mode everywhere except node_modules (libs may still be on Svelte 4).
		// Can be removed once Svelte 6 makes Runes the universal default.
		runes: ({ filename }) => (filename.split(/[/\\]/).includes('node_modules') ? undefined : true)
	},
	kit: {
		// adapter-node so we can ship a single Node binary inside Docker
		// (self-hosted is our deploy target — see docs/DESIGN.md §4.7).
		// Default env var names (PORT, HOST, ORIGIN, …) are kept so standard
		// container orchestrators (Docker, k8s, Fly, Render) work without
		// custom config. We can introduce an envPrefix later if/when we have
		// an actual conflict with the wider TDH_* env namespace.
		adapter: adapter({
			out: 'build',
			precompress: true
		})
	}
};

export default config;
