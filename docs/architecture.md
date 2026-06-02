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

- **Nothing leaves the device (Principle #9).** All user data — keystrokes, motor map, snapshots, slip patterns, fatigue signals, mode preferences — stays local. No network code anywhere in the tree; no cloud sync, analytics, telemetry, crash reports, or cross-user shared learning, ever. Recovery data demands an **absolute, not best-effort** bar; any off-device feature is rejected by default.
- L2–L4 must not call macOS-specific APIs directly. Anything OS-specific lives in L1.
- L5 surfaces the `permission_required` event from L1 gracefully — it must not crash the app or silently fail.
- The UI must never use a slider control (see product principle 5).
- The four-state outcome model (`CleanHit`, `SelfCorrected`, `UncorrectedMiss`, `TwoKeysTogether`) is load-bearing — do not collapse.
- **Capture integrity is observable, not assumed (Principle #8).** Every pipeline stage (L1 ingest → engine accept → sealing → 5a verdict → 5c observe → persistence) exposes a counter reconciled against the previous stage; adjacent-stage conversion ratios are auditable in real time (`Funnel` / `FUNNEL_DUMP` in `engine.rs`). Silent drops are unacceptable — for a recovery-tracking app, dropping the user's data is lying about their recovery. New stages that can drop data must ship with a counter. See CLAUDE.md for the full statement. **Milestone M2.5** (between understanding M2 and the learning loop M4): no 5c-proper / Practice / kill-switch work until the funnel is healthy on a calibration run.
- **Every meaningful state is preserved (Principle #6).** The engine writes a dated snapshot (`~/.typeassist/snapshots/YYYY-MM-DD.json`) **once per day on the first event of a new calendar day**, *and* **on engine startup before any writes to the live map**. Snapshots are **never overwritten or deleted by the engine — they accumulate**. So every day the app runs leaves a preserved per-key checkpoint, even across a crash. This replaced a weekly cadence that had already lost a day (no `2026-05-31` snapshot); losing a day's state is the same failure class as a silent capture drop. Implemented in `engine.rs` (startup snapshot beside the motor-map load; the watchdog writes the new day's file, write-only-if-absent, when the calendar date rolls over); these snapshots are the history the Practice trend and (future) Progress view read back.
- **TypeAssist grows with you, not over you (Principle #10).** Not pre-trained — it learns the specific user. It observes more than it acts early on, and helps more as it builds confidence about that person's slips (less help day one, more correct help day ninety). The kill-switch is **per-user, not a global threshold**; Practice mode is the accelerator for teaching it; it must never feel like a "worse autocorrect."

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
