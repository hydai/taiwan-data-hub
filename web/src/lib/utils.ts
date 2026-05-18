import { type ClassValue, clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

/**
 * Merge Tailwind class strings safely.
 *
 * `clsx` handles conditional / array / object inputs; `tailwind-merge`
 * resolves conflicts (e.g. `px-2 px-4` → `px-4`). Canonical `cn` helper
 * expected by every shadcn-svelte component.
 */
export function cn(...inputs: ClassValue[]) {
	return twMerge(clsx(inputs));
}

/**
 * Add a `ref` binding to a component's props type so consumers can
 * `bind:this={ref}` against the underlying HTML element.
 *
 * Used pervasively by shadcn-svelte components — see e.g. button.svelte.
 */
export type WithElementRef<T, U extends HTMLElement = HTMLElement> = T & {
	ref?: U | null;
};

/**
 * Drop the bits-ui `child` snippet prop, used when wrapping a bits-ui
 * primitive in a shadcn-svelte component that only wants `children`.
 */
export type WithoutChild<T> = T extends { child?: unknown } ? Omit<T, 'child'> : T;

/**
 * Drop the `children` prop (rare; used when a component always provides
 * its own content).
 */
export type WithoutChildren<T> = T extends { children?: unknown } ? Omit<T, 'children'> : T;

/**
 * Convenience composition of {@link WithoutChild} and {@link WithoutChildren}.
 */
export type WithoutChildrenOrChild<T> = WithoutChildren<WithoutChild<T>>;
