/**
 * Primary navigation links rendered by both the desktop {@link Header}
 * and the mobile {@link MobileMenu}. Centralised here so adding or
 * removing a top-level route only touches one file.
 *
 * `as const` narrows each `href` to its literal type so it satisfies
 * SvelteKit's `RouteId` union for `resolve(...)` without a cast.
 */
export const navLinks = [
	{ href: '/domains', label: 'Domains' },
	{ href: '/datasets', label: 'Datasets' },
	{ href: '/collections', label: 'Collections' },
	{ href: '/connectors', label: 'Connectors' },
	{ href: '/licenses', label: 'Licenses' }
] as const;

export type NavLink = (typeof navLinks)[number];
