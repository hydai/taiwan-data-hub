# Design tokens

> Canonical reference for the Taiwan Data Hub design system. Tokens are
> declared in [`web/src/app.css`](../web/src/app.css) and consumed
> everywhere via Tailwind 4 utilities (`bg-primary-500`, `text-neutral-700`,
> `font-mono`, etc.) or directly as CSS variables (`var(--color-primary-500)`).

Updated as part of M2 [#2.1](https://github.com/hydai/taiwan-data-hub/issues/19).

---

## 1. Tailwind 4, CSS-first config

There is **no `tailwind.config.js`** in this repo. Tailwind 4 derives its
utilities from CSS custom properties declared inside an `@theme` block in
the global stylesheet. The configuration file *is* the CSS.

```css
@import 'tailwindcss';

@theme {
  --color-primary-500: oklch(0.62 0.18 250);
  --font-sans: 'Inter', 'Noto Sans TC', system-ui, sans-serif;
}
```

Tailwind walks the custom properties in `@theme` and automatically wires up
matching utilities — `bg-primary-500`, `text-primary-500`, `border-primary-500`,
`ring-primary-500`, `from-primary-500`, etc.

This means the **token name is the utility name**. Adding
`--color-mango-500: oklch(0.75 0.15 80)` to `@theme` gives you `bg-mango-500`
for free.

### Tailwind 3 → 4 differences worth remembering

| Tailwind 3 | Tailwind 4 |
|---|---|
| `tailwind.config.js` | `@theme { … }` in your CSS |
| `theme.extend.colors.primary[500]` | `--color-primary-500: …;` |
| Default `shadow` utility | Now `shadow-sm`; the old `shadow-sm` is `shadow-xs` |
| Plugins via `tailwind.config.js` | Plugins via `@plugin` directive in CSS |
| `darkMode: 'class'` | `@variant dark (&:where(.dark, .dark *))` |

The shadow rename is especially easy to miss — any component you port from a
Tailwind-3 codebase will look subtly off until you bump the shadow utility.

---

## 2. Color tokens

All colors use the **OKLCH** color space — `oklch(L C H)` — because steps
along a single hue are perceptually uniform. The same L-delta produces the
same apparent contrast change at any hue, which makes the 50→950 ramps
predictable to use.

For each color family the **hue stays fixed**; only L (lightness) and C
(chroma) vary across stops.

### 2.1 Primary — `--color-primary-{50..950}`

Taiwan-data blue, hue **250°**. The 500 step is the brand anchor. Use:

- `primary-50/100` for subtle tinted backgrounds (e.g. info cards)
- `primary-500` for default brand surfaces (CTAs, focus rings)
- `primary-600/700` for hover states and contrast-sensitive text
- `primary-800/900/950` for dark-mode foregrounds (future)

### 2.2 Neutral — `--color-neutral-{50..950}`

Hue 250° with very low chroma (≤ 0.013). This **overrides Tailwind 4's
default `neutral` palette** — shadcn-svelte's `components.json` sets
`baseColor: "neutral"`, so generated components automatically pick up these
ramps without further configuration.

The slight cool tint (vs. truly achromatic gray) keeps neutral surfaces from
looking dirty next to saturated primary blue. This is a deliberate choice —
do not "fix" the chroma to zero.

Common uses:

- `neutral-50` — page background
- `neutral-100/200` — card backgrounds, dividers
- `neutral-400/500` — secondary / placeholder text
- `neutral-700/800` — body copy
- `neutral-900` — high-emphasis text, headings

### 2.3 Semantic colors

Three stops per family — **50**, **500**, **700**. Extend with 100/600/900
only when a concrete UI need appears.

| Family | Hue | Use |
|---|---:|---|
| `success` | 150° (green) | Confirmations, healthy status, "200 OK" |
| `warning` | 70°  (amber) | Soft alerts, deprecations, throttle nudges |
| `danger`  | 25°  (red) | Destructive actions, errors, "5xx" |
| `info`    | 215° (cyan) | Informational banners; **note** this is intentionally distinct from `primary` (250°) so users can tell brand callouts apart from informational ones at a glance |

### 2.4 OKLCH cheatsheet

When extending a ramp, follow this rough L curve:

| Stop | L (lightness) | Chroma rule |
|---|---:|---|
| 50  | 0.96–0.98 | very low |
| 100 | 0.93–0.96 | low |
| 200 | 0.86–0.90 | rising |
| 300 | 0.78–0.83 | rising |
| 400 | 0.68–0.74 | near-peak |
| 500 | 0.60–0.65 | peak chroma (the brand anchor) |
| 600 | 0.52–0.58 | starting to fall |
| 700 | 0.44–0.50 | falling |
| 800 | 0.35–0.42 | falling |
| 900 | 0.25–0.32 | low |
| 950 | 0.13–0.20 | very low |

Chroma typically peaks around the 500 stop and tapers off at both ends.
Pure black/white (L=0 / L=1) should be approached with `chroma → 0`.

---

## 3. Typography

### 3.1 `--font-sans`

```css
'Inter', 'Noto Sans TC', system-ui, -apple-system, 'Segoe UI', Roboto,
'Helvetica Neue', Arial, 'PingFang TC', 'Microsoft JhengHei', sans-serif;
```

**Order is intentional, not arbitrary.**

The browser picks the first font containing a given glyph. Latin characters
exist in both Inter and Noto Sans TC; CJK characters exist only in Noto Sans
TC, PingFang TC, and the generic `sans-serif` fallback. By listing Inter
first the page renders Latin in Inter's UI-tuned shapes while CJK glyphs
"fall through" to Noto Sans TC seamlessly.

If we ever switch the order — putting Noto Sans TC first — Latin would
render in Noto Sans TC's less-polished Latin glyphs. **Don't do that.**

### 3.2 `--font-mono`

```css
'JetBrains Mono', ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas,
'Liberation Mono', monospace;
```

Used for code blocks, JSON payloads in the Playground (#6.x), MCP tool
descriptors in the inspector view, and any tabular numeric data where
column alignment matters.

### 3.3 Font feature settings

`app.css` enables Inter's stylistic alternates globally via
`font-feature-settings: 'cv02', 'cv03', 'cv04', 'cv11'`. These alternate
glyphs give cleaner-looking digits (one-storey 1, slashed 0, etc.) which
matters in a data-heavy UI. The features are silently ignored by Noto Sans
TC, PingFang TC, and the other fallbacks, so leaving them on globally is
safe.

---

## 4. Spacing and radius

Tailwind 4 ships a sensible 0.25 rem-step scale (`p-1` = 0.25 rem, `p-4` = 1
rem, `p-8` = 2 rem, …). We **do not override it**. Custom spacing tokens
should only be added when a recurring measurement appears in three or more
components and doesn't snap to the default scale.

A single named radius token is defined:

```css
--radius: 0.5rem;
```

This matches the shadcn-svelte idiom — cards, inputs, buttons, and dropdown
menus all reference `var(--radius)` so a single change can re-skin the
entire UI.

---

## 5. Dark mode (planned — not in #2.1)

Dark mode is intentionally out of scope for #2.1. When added, the migration
plan is:

1. Add a `@media (prefers-color-scheme: dark)` block that re-declares the
   same `@theme` tokens with inverted lightness curves (50↔950, 100↔900, …).
2. Layer an opt-in `[data-theme="dark"]` selector on `<html>` so user
   preference can override the system setting.
3. Audit semantic colors — `danger-500` at L=0.62 reads fine on a light
   background but needs to drop to ~L=0.55 on dark surfaces to preserve
   contrast. Don't blind-flip the curves.
4. Verify Lighthouse a11y score stays ≥ 95 under both color schemes.

Track in a future M2 sub-issue.

---

## 6. Contribution rules

- **No magic colors.** Every color used in a Svelte component must reference
  a token through a Tailwind utility (`bg-primary-500`) or a CSS variable
  (`var(--color-primary-500)`). No raw `#3b82f6`, no raw `oklch(…)` in
  `*.svelte` files.
- **Don't shadow tokens locally.** Defining `--color-primary-500` inside a
  component scope makes Tailwind utilities stop matching what your eyes
  see. Override at the root only.
- **One source of truth.** All tokens live in `web/src/app.css`. The
  shadcn-svelte config (`components.json`) points at this file, so any
  component you scaffold inherits the same palette automatically.
- **OKLCH only.** Mixing OKLCH and HSL/RGB in the same ramp makes the
  perceptual steps uneven. New colors join the system in OKLCH; if you
  inherit a hex spec from a brand brief, convert it once and never look at
  the hex again.

---

## 7. Quick reference (most-used)

```html
<!-- Page chrome -->
<body class="bg-neutral-50 text-neutral-900 font-sans">

<!-- Heading -->
<h1 class="text-4xl font-bold tracking-tight text-neutral-900">

<!-- Body copy -->
<p class="text-base text-neutral-700">

<!-- Brand link / CTA -->
<a class="text-primary-700 hover:text-primary-800 underline">
<button class="bg-primary-500 hover:bg-primary-600 text-white">

<!-- Card -->
<div class="rounded-[var(--radius)] border border-neutral-200 bg-white p-5">

<!-- Status badges -->
<span class="bg-success-50 text-success-700">Healthy</span>
<span class="bg-warning-50 text-warning-700">Deprecated</span>
<span class="bg-danger-50  text-danger-700">Failed</span>
<span class="bg-info-50    text-info-700">Note</span>

<!-- Code -->
<code class="font-mono text-sm text-neutral-800">
```
