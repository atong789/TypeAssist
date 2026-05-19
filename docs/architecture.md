# Architecture

TypeAssist is a 5-layer app. Layer 1 is OS-specific and swappable. Layers 2–5 are portable. Layer 3 is the load-bearing contract between L2 and L4.

```
+---------------------------+
| L5  Tauri shell + Svelte  |   apps/tauri/
+---------------------------+
            ▲   │
            │   ▼
+---------------------------+
| L4  Correction engine     |   crates/correction-engine/   (stub)
+---------------------------+
            ▲
            │  reads VolatilityMap
            │
+---------------------------+
| L3  Volatility map        |   crates/volatility-map/      (contract)
+---------------------------+
            ▲
            │  L2 projects aggregates into L3
            │
+---------------------------+
| L2  Behavioural model     |   crates/behavioural-model/
+---------------------------+
            ▲
            │  line-delimited JSON over stdio
            │
+---------------------------+
| L1  Input adapter (Swift) |   adapters/macos/
+---------------------------+
```

## Why these boundaries

- **L1 is its own process.** A CGEventTap handler runs in a hot path; isolating it from L2–L5 lets the Rust core be tested with synthetic event streams, and makes Windows/Android adapters a drop-in swap.
- **L3 is its own crate, not a module of L2.** Both L2 (writer) and L4 (reader) depend on L3 as a peer. That keeps the contract explicit and lets us version it independently.
- **L4 reads only from L3.** It never inspects the raw behavioural-model internals. If L4 wants a new signal, it goes through L3's schema.
- **Storage is its own crate.** L2's aggregates and L4's outcome history both need durable writes; centralising migrations avoids two crates fighting over schema.

## Cross-cutting invariants

- All processing is local. No network code anywhere in the tree.
- L2–L4 must not call macOS-specific APIs directly. Anything OS-specific lives in L1.
- L5 surfaces the `permission_required` event from L1 gracefully — it must not crash the app or silently fail.
- The UI must never use a slider control (see product principle 5).
- The four-state outcome model (`CleanHit`, `SelfCorrected`, `UncorrectedMiss`, `TwoKeysTogether`) is load-bearing — do not collapse.

## Build pipeline

```
adapters/macos          →  swift build  →  .build/release/typeassist-input-macos
                                                   │
                                                   ▼  (copy + sign)
apps/tauri/src-tauri/sidecars/typeassist-input-macos-aarch64-apple-darwin
                                                   │
                                                   ▼
              cargo build (workspace) + vite build + tauri bundle
```

Orchestrated by `just`. See `CLAUDE.md` for the command list.
