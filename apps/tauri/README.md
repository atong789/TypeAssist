# Tauri app (Layer 5)

The Tauri shell + Svelte webview. Mostly stubbed in this scaffold pass.

## First-time setup

```bash
cd apps/tauri
npm install
```

## Dev

From the repo root: `just dev`. This builds + signs the Swift sidecar, then runs `npm run tauri dev`.

## Bundle

`just build-mac` produces a `.app` bundle. You'll need icons in `src-tauri/icons/` first; see `src-tauri/icons/README.md`.

## Frontend stack

- Svelte 4 + TypeScript
- Vite (fixed dev port 1420 so Tauri can find it)
- One root component (`App.svelte`) with a flat in-component router across five routes: Home, Today, Warm-up, Practice, Settings. No SvelteKit, no router lib — the surface is small enough that one switch suffices.

## UI constraints — enforced by stylesheet

- `input[type="range"]` is hidden globally in `src/app.css`. Use discrete card selectors instead. See Principle #5 in `CLAUDE.md`.
