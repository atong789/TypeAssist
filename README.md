# TypeAssist

A native macOS app that helps users with motor difficulty type more accurately by learning each user's personal motor patterns and correcting typos based on a spatial volatility map, not a generic dictionary.

## Status

Early scaffolding. See [CLAUDE.md](./CLAUDE.md) for architecture and conventions.

## Quick start

Prerequisites: Rust (stable), Xcode command-line tools, Node 20+, [`just`](https://github.com/casey/just).

```bash
just build-sidecar      # build the Swift Layer-1 adapter
cd apps/tauri && npm install
cd ../..
just dev                # run the Tauri app
```

On first launch macOS will prompt for Accessibility permission. Grant it in System Settings → Privacy & Security → Accessibility. See [adapters/macos/README.md](./adapters/macos/README.md) for details.

## What it is — and is not

TypeAssist is local-first by structure. No internet, no accounts, no cloud, no telemetry. See [CLAUDE.md](./CLAUDE.md) for the full architecture and product principles.
