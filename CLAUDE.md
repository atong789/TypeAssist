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

## Recovery physiology

After stroke, fingers recover at physiologically different rates — not by the user's choice. **Thumb and index regain independent control fastest**; the outer three (middle, ring, little) are more tendon-interconnected and recover slowest, **ring and little especially**. This is anatomy, not effort. The engine must be finger-aware about it:

1. **Frame as physiology, not failure.** Higher slip rates and slower improvement on slow-recovery fingers are NORMAL physiology — never the user's failure. UI copy, progress framings, and insights treat a slow ring finger the way physical therapy treats a slow leg: expected, not a deficit.
2. **Weight correction priors by finger.** Downstream of L2, the correction engine should bias confidence by which finger is involved. A slip on a slow-recovery finger is more likely a motor error to smooth; an unusual key under thumb or index is more likely intentional and should be left alone. This applies to the L4 confidence tiers (`Gentle`/`Balanced`/`Bold`) and to swap-pair scoring in L3.
3. **Calibrate progress per finger.** Grade each finger against *its own* expected recovery curve, so a slow finger is never made to feel like it's lagging the others. Progress on Progress's "where your hands are gaining ground" is per-finger, not whole-hand.

Sourced from the builder's lived stroke-recovery experience — load-bearing for both engine weighting and UI tone.

## Accessibility standards — non-negotiable

TypeAssist's users have motor difficulties (stroke survivors, arthritis). Large, mouse-forgiving targets and full keyboard operability are the *core of the product*, not enhancements. Every screen must meet all of these:

- **Target size.** Interactive controls are at least **36px tall** (aim for 44px) with comfortable padding. Prefer large hit areas — e.g. make a whole card clickable, with an inner control as the visible affordance.
- **Full keyboard navigation.** Every interactive element is reachable with **Tab** and activatable with **Enter and Space**. Use a logical DOM/tab order (primary navigation, then main content); never use positive `tabindex` — the visual order is the tab order.
- **Primary navigation = a single tab stop.** The sidebar is a **WAI-ARIA vertical tablist with roving tabindex** (matches native macOS sidebars): one Tab stop lands on the *selected* item, **Up/Down** (wrapping) + **Home/End** move focus between items, and Tab/Shift+Tab move out of/into the content. Use **manual activation** — arrows move focus only; **Enter/Space (or click) commits** the screen change — so a stray arrow never navigates (fewer accidental navigations for motor-impaired users). Mark items `role="tab"` with `aria-selected`, and the content region `role="tabpanel"` + `aria-labelledby` the active tab. Don't make the panel wrapper itself a tab stop; Tab should land on the content's first *interactive* element.
- **Visible focus rings — always.** Every focusable control shows a clear, high-contrast ring (the `--focus-ring` token) whenever focused. Style **`:focus`** (not only `:focus-visible`): a keyboard-first app must never hide focus, and `:focus-visible` silently drops when focus is moved *programmatically* (e.g. by the focus trap or roving-tabindex arrows) — which produced a real "vanishing ring" bug. There is also a global `:focus` ring in `app.css` as a backstop.
- **Focus must never escape the app (focus trap).** In a WebView, Tab past the last control hands focus to the host window — a ringless, non-DOM location — before wrapping. A root-level `keydown` handler wraps focus: Tab on the last tabbable → first, Shift+Tab on the first → last (computing tabbables live, respecting roving `tabindex="-1"`). This keeps the ring continuous on every screen, including ones with no content yet. Lives in `apps/tauri/src/App.svelte`.
- **Navigation lands focus on a sensible visible target.** The focus ring must never be invisible after a navigation. Opening a sub-view focuses its **primary anchor** (e.g. Progress focuses its back arrow on arrival, via `.focus()` in `onMount`). Returning from a sub-view **restores focus to the control that opened it** (e.g. returning to Today focuses the "See your progress" link). Implementation: child views dispatch `navigate-back` instead of `navigate` when returning; the parent passes a one-shot `focusTarget` prop, cleared via `tick()` after the child mounts. Because rings are styled on `:focus` (not only `:focus-visible`), programmatic focus shows the ring immediately — no Tab needed.
- **One screen-title style, used everywhere.** Every screen wraps its name in `<header class="screen-header"><h1>…</h1></header>` and gets the **same** title treatment — shared style in `apps/tauri/src/app.css`. Sub-views with a back path (Progress, the Practice round) add a standalone focusable chevron button (`.screen-back`) before the `<h1>`; that chevron is the **only** visual difference between a sub-view header and a top-level header (the title itself is identical). Where a title has a "· context" suffix (e.g. *Today · Friday, May 23*; *Practice · Steady*), wrap the suffix in `<span class="screen-context">` so it renders in the secondary color while the heading otherwise looks identical. Never hand-size or hand-style a title.
- **Fit the launch window — nothing important below the fold.** Every screen must lay out so all of its read-only content sits within the launch window without scrolling. Read-only content is *not* keyboard-reachable, so anything below the fold can only be reached with a mouse — which a keyboard-first app must not require. Pair a tight, compact layout with a Tauri default + `minHeight` that holds the densest screen, and cap any expandable list (e.g. Progress's "See all fingers") to what fits. Window dimensions live in `apps/tauri/src-tauri/tauri.conf.json`.
- **WCAG AA contrast.** Body and secondary ("quiet") text must clear **4.5:1** against the background in *both* light and dark mode (3:1 for large text and non-text UI). Use the `--text-secondary` token, not translucent gray — gray mixed with `transparent` fails contrast unpredictably over varied backgrounds.
- **Semantic elements.** Use real `<button>`/`<a>` with appropriate ARIA (e.g. `aria-current="page"` on the active nav item), never click-handler `<div>`s, so screen readers announce roles correctly.
- **No sliders, anywhere.** Discrete card selectors only (see Product principles). Enforced by CSS in `apps/tauri/src/app.css`.

Shared accessible tokens live in `apps/tauri/src/app.css` (`--text-secondary`, `--hairline`, `--focus-ring`) plus a global `:focus` ring. Reuse them on every new screen so these rules hold automatically.

## Insight system

TypeAssist surfaces insight across three surfaces — **Today**, **Progress**, **Practice** — each with a distinct job and voice. Never blur them.

**Two kinds of typing data:**
- **Ambient** — normal all-day typing everywhere on the Mac, captured passively. The core product.
- **Deliberate practice** — opt-in structured sessions (Practice mode, Warm-up).

**Content-blind, always.** The app never sees *what* was typed — only motor/timing shape (key, dwell, drift, episode rhythm). Insight speaks to **episode shape and patterns, never content**. Do **not** track which app was focused (surveillance feel).

### Today — the daily mirror (observational, read-only)

- A short, gentle **narrative readback** of the day. A friend giving a readback, never a clinician.
- **"What I noticed"**: a few specific, recognizable patterns (e.g. "right thumb on the spacebar"). **Descriptive, never prescriptive** — never "you should practice."
- **No clock-time / time-of-day timeline.** (Retired: time-of-day bands are context-free — a user can't connect "hesitant at 2pm" to anything. The meaningful variable is the **typing episode/effort** — cold starts, sustained-effort fatigue, recovery — not the clock.)
- **Read-only.** The only interactive element is a quiet link into Progress. Today is a sidebar destination, so **no back-arrow**.
- **Honest empty states**: a morning with no data says "the day's just beginning" — never predictions or yesterday's baggage.
- **Today observes; Home invites.** Any call to action (e.g. a warm-up suggestion on a stiff cold-start morning) lives on **Home's warm-up card**, never on Today.

### Progress — where the meaning lives (reached from Today; feeds the therapist export)

- **Clean rate, not error count**: "landed clean 88% this week, up from 82% last month." Always a **rate** (per 100 words), never a raw count — a heavy day inflates raw counts and misleads. Capability framing ("clean"), never deficit ("errors").
- **Steadiness trend across weeks** — meaningful because it spans weeks, not one day. The trend line **must show honest variation** (good and bad weeks both belong); never a fake monotonic rise.
- **Volume as practice**: "~14,000 words this week, all of it practice." Reflective and gentle — **never a goal/target/streak**. A quiet day must never read as failure.
- **Slips smoothed for you**: the count of silent corrections, framed as **help given** ("smoothed for you"), never errors made. This makes invisible progress visible — the whole point, since silent help means the user can't otherwise feel the improvement.
- **"Where your hands are gaining ground"**: per-finger/per-key patterns + commonalities (recurring swap-pairs from the volatility map), framed as capability ("steadier," "gaining ground"). Show **only fingers with something to say** (gains, plus the occasional "still finding it"), capped ~3–4 by default, with a quiet **"see all"** for the full hand. Calm by default, complete on demand.
- **No progress without meaningful samples**: a finger needs enough real data before it earns a trend; below that show "still getting to know this one," never a number invented from a few keystrokes. (Exact threshold is a build-time tuning detail.)
- **Range selector**: Week / Month / All time. Retain longitudinal data locally (cheap, private; also powers the therapist export).
- **Footer**: restate the on-device privacy promise. The opt-in therapist-share link is a **v2 footer element** — never pushed.
- **Never on this surface**: WPM, streaks, daily scores, comparison to other users, goals/targets, prescriptions.

### Practice — opt-in targeted training

- Where structured measurement legitimately belongs (the user opted into an exercise). Typing-Club style: accuracy and improvement on focused letter-combinations, in TypeAssist's voice. Levels: **Gentle / Steady / Spirited** (kinds of day, not difficulty grades).
- **Content is personalised to the user's own tricky keys and finger-transitions** (drawn from the volatility map) — not a generic keyboard-row curriculum. Real lowercase words, short and focused, denser by level. At cold start, a sensible common set until the map has learned the user.

### Cross-cutting — progress is offered, never imposed

The same number that motivates on a good day can sting on a bad one. Progress is rich and available in the views the user **goes to** (Progress, Practice), framed as capability growing — never a daily verdict, notification, or streak that greets them. **The good day and the bad day both belong.**

## Typing surfaces

TypeAssist sees user typing on three kinds of surface: the **ambient** OS-wide capture (no UI), **Warm-up** (opt-in, unmeasured), and **Practice** (opt-in, measured — future). These share one rule and diverge on another.

- **Backspace always works (universal).** On every typing surface, backspace moves the caret back one character so the user can retype. Never block backspace, never discourage it — self-correction is signal, not failure (see Correction-engine state model → `SelfCorrected`).
- **Warm-up — unmeasured: smooth and advance.** A wrong key never blocks and never displays as an error: the caret advances one character and the *correct target character* appears (the slip is silently smoothed). No red, no "try again," no error state of any kind, anywhere. The caret must never stick waiting for the correct key. **Passages are always all-lowercase** — no proper nouns, no capitals, no shifted punctuation. Shift is a hard two-key chord for our users and Warm-up must never require it. Lives in `apps/tauri/src/routes/WarmUp.svelte`.
- **Practice — measured: slips are visible.** A wrong key *does* appear in the rendered stream, marked with **both amber colour and a wavy underline** so the slip is visible without colour perception (never red). The caret advances past the slip; backspace removes the slip so the user can retype (universal backspace rule applies). Slips are counted internally for the end-of-round readout, but **no live score, percentage, timer, or WPM** is shown during the round. Lives in `apps/tauri/src/routes/Practice.svelte`.

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

## Known design questions / future

Open questions surfaced during development that don't have a settled answer yet. Each is parked here so it isn't forgotten; revisit when the relevant layer is being built out.

- **Coexistence with macOS system autocorrect.** Most macOS text fields run their own autocorrect, so it operates on the same keystroke stream as TypeAssist. The two solve different problems — macOS is dictionary-based (whole known words), TypeAssist is motor/spatial (the specific slip a hand makes) — but they can collide on the same keystroke, and a system correction can easily be mistaken for one of ours. Open question: how the two should coexist. Options to weigh: detect-and-defer when a system correction is in flight; document testing with system autocorrect off; or confirm it's a non-issue in practice. Surfaced during debug-view testing when macOS corrected `cuty → city` while our engine correctly reported `LEFT ALONE`.

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
