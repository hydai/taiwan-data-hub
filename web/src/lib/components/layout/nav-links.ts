import { m } from '$lib/paraglide/messages';

/**
 * Primary navigation links rendered by both the desktop {@link Header}
 * and the mobile {@link MobileMenu}. Centralised here so adding or
 * removing a top-level route only touches one file.
 *
 * `label` is a function — Paraglide v2 message helpers are
 * `() => string` so the call site can decide when the active locale
 * is read (typically per render). `as const` narrows `href` to its
 * literal type for SvelteKit's `RouteId` union.
 */
export const navLinks = [
	{ href: '/domains', label: m.nav_domains },
	{ href: '/datasets', label: m.nav_datasets },
	{ href: '/collections', label: m.nav_collections },
	{ href: '/connectors', label: m.nav_connectors },
	{ href: '/playgrounds', label: m.nav_playgrounds },
	{ href: '/licenses', label: m.nav_licenses }
] as const;

export type NavLink = (typeof navLinks)[number];
