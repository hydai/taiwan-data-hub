# `web/` — Taiwan Data Hub frontend

SvelteKit 2 + Svelte 5 (Runes) + Tailwind 4 (CSS-first) + shadcn-svelte 1 with bits-ui 2. See [`docs/DESIGN.md`](../docs/DESIGN.md) for the full system design.

This package is part of the [Taiwan Data Hub](../README.md) pnpm
workspace. Run commands from the **repo root** unless noted; the root
`package.json` forwards them with `pnpm --filter web ...`.

## Develop

```bash
pnpm install              # at repo root (installs all workspaces)
pnpm dev                  # → http://localhost:3000  (strictPort)
```

The dev server binds to port **3000** (not SvelteKit's default 5173) so
it matches the Docker Compose mapping that lands in `#0.3`.

## Quality gates (must pass before merge)

```bash
pnpm check                # svelte-check (typecheck Svelte + TS)
pnpm lint                 # prettier --check && eslint
pnpm format               # auto-fix formatting
pnpm build                # adapter-node production build → web/build/
```

## Layout

```
web/
├── src/
│   ├── app.d.ts                # SvelteKit ambient types
│   ├── app.html                # HTML shell
│   ├── lib/
│   │   ├── components/ui/      # shadcn-svelte components (added per-issue)
│   │   ├── index.ts            # public lib exports
│   │   └── utils.ts            # cn(), WithElementRef, WithoutChild*
│   └── routes/
│       ├── +layout.svelte      # global layout
│       ├── +page.svelte        # placeholder home (replaced in M2)
│       └── layout.css          # Tailwind @import + @theme tokens
├── static/                     # favicon, robots.txt, etc.
├── components.json             # shadcn-svelte config
├── svelte.config.js            # adapter-node, Runes mode
├── vite.config.ts              # Tailwind v4 Vite plugin + port 3000
├── tsconfig.json
└── package.json
```

## Adding a shadcn-svelte component

```bash
cd web
pnpm dlx shadcn-svelte@latest add <component>   # e.g. button, dialog
```

Generated files land under `src/lib/components/ui/<component>/`. They
import the `cn` / `WithElementRef` helpers from `$lib/utils.ts` (see
`components.json` aliases).

## Notes for contributors

- Use Svelte 5 **Runes** (`$state` / `$derived` / `$effect` / `$props`).
  `$:` reactive statements and `export let` props are not used.
- Tailwind 4 uses **CSS-first config** via `@theme` in `src/routes/layout.css`.
  Do not add a `tailwind.config.js`.
- We ship via `@sveltejs/adapter-node`, not `adapter-auto`. The build
  honors standard `PORT` / `HOST` / `ORIGIN` env vars.
