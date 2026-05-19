# TypeAssist

Native macOS app that helps users with motor difficulty type more accurately by learning each user's personal motor patterns and correcting typos via a spatial volatility map (not a generic dictionary).

## Privacy invariant — non-negotiable

All processing is local. No internet, no accounts, no cloud, no telemetry. This is structural, not optional. Do not add an HTTP client, analytics, or remote logging without an explicit product decision to reverse this stance.

## Architecture — 5 layers

L1 is OS-specific and swappable. L2–L5 are portable. L3 is the load-bearing contract between L2 and L4.

| Layer | Role | Tech | Location |
|---|---|---|---|
| 1 | Input capture (keystrokes, timing, modifiers, correction injection) | Swift · CGEventTap + AX API | `adapters/macos/` |
| 2 | Behavioural model — per-key timing, asymmetry, fatigue, ghost keys, temporal profiles | Rust | `crates/behavioural-model/` |
| 3 | Spatial volatility map — per-key confidence + swap pairs, versioned JSON schema | Rust | `crates/volatility-map/` |
| 4 | Correction engine — lexicon-aware spatial fixes, three tiers, three space-error types, four outcome states | Rust | `crates/correction-engine/` |
| 5 | UI — Tauri 2.x shell + Svelte/TS webview | Tauri + Svelte | `apps/tauri/` |

Persistence: `crates/storage/` (SQLite via sqlx).

## Contracts

- **L1 ↔ L2**: line-delimited JSON over stdio. Wire types in `crates/behavioural-model/src/events.rs`. Schema doc: `docs/contracts/input-events.md`.
- **L2 → L3 → L4**: serde structs in `crates/volatility-map/src/schema.rs`. Versioned. JSON Schema is emitted to `crates/volatility-map/schema/volatility-map.v1.json` by `just schema`.

## Product principles (tiebreakers when in doubt)

1. **Dignity over diagnosis** — never describe the user by their condition.
2. **Capability not compensation** — "TypeAssist learned a pattern", not "TypeAssist fixed your mistake".
3. **The good day and the bad day both belong** — the app adapts, the user doesn't.
4. **Quiet by default** — no notifications, no streaks, no guilt mechanics.
5. **No sliders, anywhere.** Discrete card selectors only. Mouse path forgiving, keyboard path one-handed-friendly.

These override implementation convenience. If a feature seems to want a slider, find another shape.

## Correction-engine state model

- **Confidence tiers**: `Gentle`, `Balanced`, `Bold`.
- **Space-error types**: `MissingSpace`, `ExtraSpace`, `ModifierDrift`.
- **Outcome states** (per word): `CleanHit`, `SelfCorrected`, `UncorrectedMiss`, `TwoKeysTogether`. All four are intentional — do not collapse to three. Self-corrections via backspace are signal, not failure.

## Target platform

macOS 13 (Ventura) or later. Windows and Android adapters are future work; the L1 boundary is shaped to make them pluggable. L2–L4 must never call OS-specific APIs directly.

## Build & dev

`just` is the entry point. From the repo root:

| Command | What it does |
|---|---|
| `just build-mac` | Build Swift sidecar + Rust workspace + Tauri app |
| `just build-sidecar` | Build + ad-hoc codesign the Swift sidecar |
| `just dev` | Run Tauri in dev (spawns the Swift sidecar) |
| `just schema` | Regenerate `crates/volatility-map/schema/volatility-map.v1.json` |
| `just test` | `cargo test --workspace` |
| `just lint` | `cargo clippy` + `cargo fmt --check` |
| `just sign-dev` | Ad-hoc codesign the Swift binary so Accessibility permission persists across rebuilds |

Before the first `just dev`, run `npm install` inside `apps/tauri/`.

## Accessibility permission (macOS)

The Swift sidecar needs Accessibility permission (System Settings → Privacy & Security → Accessibility). On launch, if the permission is missing, it emits `{"type":"permission_required"}` on stdout rather than crashing. The UI surfaces this gracefully. See `adapters/macos/README.md`.

## Things to never do

- Do not introduce a slider control anywhere in the UI.
- Do not add network I/O, analytics, account systems, or remote sync.
- Do not describe the user by their condition in copy or code identifiers.
- Do not couple L2–L4 to macOS-specific APIs. L1 is the only place OS calls live.
- Do not bypass the L3 schema by reading the behavioural model directly from L4.
- Do not skip codesigning in dev — Accessibility permission resets if the binary identity changes.

## Repo layout

```
typeassist/
├── CLAUDE.md
├── Cargo.toml             # Rust workspace
├── justfile
├── rust-toolchain.toml
├── crates/
│   ├── volatility-map/    # L3 — schema contract
│   ├── behavioural-model/ # L2
│   ├── correction-engine/ # L4 (stub)
│   └── storage/           # SQLite persistence
├── adapters/
│   └── macos/             # L1 — Swift sidecar
├── apps/
│   └── tauri/             # L5 — Tauri + Svelte (mostly stub)
└── docs/
    ├── architecture.md
    └── contracts/
        ├── input-events.md
        └── volatility-map.md
```
