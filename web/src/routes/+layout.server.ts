import { env } from '$env/dynamic/private';
import type { LayoutServerLoad } from './$types';

/**
 * Read the gateway operating mode (CLAUDE.md "Operating modes") and
 * expose it to the layout shell so the header can hide / show the
 * auth UI accordingly. Defaults to `personal` when unset.
 *
 * `$env/dynamic/private` is read at request time, so the same
 * compiled bundle can serve both modes depending on how the node
 * adapter is deployed — no rebuild required when toggling MODE.
 */
export const load: LayoutServerLoad = () => {
	const mode: 'personal' | 'multi-user' = env.MODE === 'multi-user' ? 'multi-user' : 'personal';
	return { mode };
};
