import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig, loadEnv } from 'vite';

export default defineConfig(({ mode }) => {
	// Dev proxy: route `/api/v1/*` and `/v1/api-keys/*` to the
	// gateway so browser-side fetches (the comment thread,
	// future client-driven views) work on a single origin
	// without needing Caddy locally. Mirrors what the reverse
	// proxy does in production. `GATEWAY_HTTP_URL` falls back
	// to the gateway's default listener.
	const env = loadEnv(mode, process.cwd(), '');
	const gatewayTarget = env.GATEWAY_HTTP_URL || 'http://127.0.0.1:8080';
	const proxy = {
		'/api/v1': { target: gatewayTarget, changeOrigin: false },
		'/v1/api-keys': { target: gatewayTarget, changeOrigin: false }
	};
	return {
		plugins: [tailwindcss(), sveltekit()],
		server: {
			port: 3000,
			strictPort: true,
			proxy
		},
		preview: {
			port: 3000,
			strictPort: true,
			proxy
		}
	};
});
