# TypeAssist

Native macOS app that helps users with motor difficulty type more accurately by learning each user's personal motor patterns and correcting typos via a spatial volatility map (not a generic dictionary).

## Nothing leaves the device (Principle #8 — non-negotiable)

All user data — keystrokes, motor map, snapshots, slip patterns, fatigue signals, mode preferences — stays on the user's local machine. No cloud sync, no analytics, no telemetry, no crash reports, no shared learning across users. **The application has no network calls related to user data, ever.** This is structural, not optional: do not add an HTTP client, analytics, or remote logging.

**Rationale:** this is recovery data — it reveals more about a person's body and cognitive state than most medical records. The privacy bar must be **absolute, not best-effort**. Any future feature that would require sending data off-device is **rejected by default — there is no acceptable tradeoff that opens this door.**

**Ephemeral contextual checks are not data collection.** The precise rule is: *nothing leaves the device, and no user activity is stored or logged.* Reading a transient piece of context that exists **only in memory, for a single decision, and is never persisted** is acceptable — it is not collection. The motivating case (M3): reading the **currently focused app's bundle ID** to pick that app's correction mode (Mail → Eager, Notes → Cautious, …). The bundle ID is used for one decision and discarded — never written to disk, never logged, never associated with keystrokes or content. The line is **persistence/logging**, not *observation*: an in-memory read for an immediate choice is fine; recording *which app you were in* (a timeline of your activity) is the surveillance this principle forbids.

## Capture integrity is observable, not assumed (Principle #7 — non-negotiable)

Every stage of the pipeline (**L1 ingest → engine accept → sealing → 5a verdict → 5c observe → persistence**) exposes a cumulative counter reconciled against the previous stage, so the conversion ratio between adjacent stages is auditable in real time. **Silent drops are unacceptable.** For a recovery-tracking app, a stroke survivor whose week of typing produces 1% of the expected data hasn't been *underserved* — the product has **lied to them about their recovery**. That is the worst failure mode the system has, worse than a wrong correction. This is foundational, not a feature.

Concretely: the engine maintains a `Funnel` (`apps/tauri/src-tauri/src/engine.rs`) with `c_keystrokes_received` / `c_keystrokes_accepted` / `c_tokens_sealed` / `c_records_admitted` / `c_verdicts_resolved` (by outcome) / `c_motor_observations` (kept vs slip) / `c_motor_saves`. `c_records_admitted` sits at the boundary that *caused* the first cliff: the motor map now has its own **`MotorLedger`** (`crates/correction-engine/src/motor_ledger.rs`) admitting **every** motor-evidenced word, decoupled from the C5b decision ledger's `should_log` Known-skip (which is a lexicon concern, not a capture one). The shared `OutcomeResolver` verdicts both ledgers; the decision ledger feeds the lexicon proposer, the motor ledger feeds the motor map. Counters are **reconciled per run, never per session**: **Cmd+Shift+F** dumps a structured `FUNNEL_DUMP` line and then **auto-resets** (closes the run); **Cmd+Shift+R** resets without dumping (start a run from zero); a **60 s periodic dump** emits without resetting (cumulative-within-run, so a long run is reconstructable from the log and survives a crash). Any new pipeline stage adds its counter and its reconciliation; never merge a stage that can drop data without a counter that makes the drop visible.

**Scope (M2.5).** Capture integrity is its own milestone, between understanding (M2) and the learning loop (M4). No 5c-proper, Practice mode, or kill-switch-criterion work proceeds until the funnel is healthy (adjacent-stage ratios match expectation on a calibration run). Fix the drop from funnel **data**, never from hypothesis.

## Every meaningful state is preserved (Principle #6 — non-negotiable)

A recovery record is only as trustworthy as the history it keeps. **Losing a day's state is the same failure class as a silent capture drop** (Principle #7) — the product would be unable to honestly show the user where their hands were. So every meaningful moment is checkpointed by design, not by luck:

- The engine writes a dated snapshot to `~/.typeassist/snapshots/YYYY-MM-DD.json` **once per day, on the first event of a new calendar day**, and **on engine startup before any writes to the live motor map**.
- **Snapshots are never overwritten or deleted by the engine — they accumulate.** A dated filename plus a write-only-if-absent guard make each day's first-captured state permanent; the live `~/.typeassist/motor_map.json` always holds the latest.
- This **replaces the previous weekly cadence**, which had already lost a day (no `2026-05-31` snapshot was ever written) — exactly the design flaw this fixes. Never work around a lost state; fix the cadence so it can't be lost.
- The accumulated snapshots are the durable history the Progress view and the (future) therapist export read back; the Practice trend reads them at daily granularity. Implemented in `engine.rs` (the startup snapshot beside the motor-map load; the watchdog writes the new day's file when the calendar date rolls over).

## TypeAssist grows with you, not over you (Principle #9 — non-negotiable)

Unlike system autocorrect or generic typing assistants, TypeAssist does **not** arrive pre-trained. It learns the specific patterns of the specific person using it. Early on, it observes more than it acts. Over time, as it builds confidence about your slips and your recovery, it begins to help. The trade-off is honest: **less help on day one, more correct help on day ninety.** The user is not a passive subject of the algorithm — they are a participant in their own recovery.

**Implications:**
- The **first-run experience must communicate this clearly** — it learns *you*; it starts quiet and earns its help, rather than arriving opinionated.
- The kill-switch (the L2→L3 correction-enable switch) operates **per-user, not by a global threshold** — correction turns on when the map is confident about *this* person's patterns.
- The product must **never feel like a "worse autocorrect."** A generic assistant that's wrong in unfamiliar ways is worse than none; TypeAssist's value is that it's *yours*.
- **Practice mode is the accelerator** — the surface where users actively teach TypeAssist their patterns, building the map's confidence faster than ambient typing alone.

## Architecture — 5 layers

L1 is OS-specific and swappable. L2–L5 are portable. L3 is the load-bearing contract between L2 and L4.

| Layer | Role | Tech | Location |
|---|---|---|---|
| 1 | Input capture (keystrokes, timing, modifiers, correction injection) | Swift · CGEventTap + AX API | `adapters/macos/` |
| 2 | Behavioural model — per-key timing, asymmetry, fatigue, ghost keys, temporal profiles | Rust | `crates/behavioural-model/` |
| 3 | Spatial volatility map — per-key confidence + swap pairs, versioned JSON schema | Rust | `crates/volatility-map/` |
| 4 | Correction engine — lexicon-aware spatial fixes, three tiers, three space-error types, four outcome states | Rust | `crates/correction-engine/` |
| 5 | UI — Tauri 2.x shell + Svelte/TS webview | Tauri + Svelte | `apps/tauri/` |

Persistence: on-device JSON stores under `~/.typeassist/`, written with durable atomic writes + corrupt-file quarantine via `crates/correction-engine/src/persist.rs`. (An earlier unused SQLite scaffold, `crates/storage/`, has been removed.)

## Contracts

- **L1 ↔ L2**: line-delimited JSON over stdio. Wire types in `crates/behavioural-model/src/events.rs`. Schema doc: `docs/contracts/input-events.md`.
- **L2 → L3 → L4**: serde structs in `crates/volatility-map/src/schema.rs`. Versioned. JSON Schema is emitted to `crates/volatility-map/schema/volatility-map.v1.json` by `just schema`.

## Principles (canonical, gap-free 1–9 — the single source of truth)

This is the **one** numbered list; cite these numbers everywhere (code comments, docs, reviews). **1–5** are product tiebreakers (when in doubt, these decide); **6–9** are non-negotiables, each with its own detailed section *above* — the line here is the summary, the section is the contract. There is no Principle #7-gap and no separate principles doc: `docs/architecture.md` points here.

1. **Dignity over diagnosis** — never describe the user by their condition.
2. **Capability not compensation** — "TypeAssist learned a pattern", not "TypeAssist fixed your mistake".
3. **The good day and the bad day both belong** — the app adapts, the user doesn't.
4. **Quiet by default** — no notifications, no streaks, no guilt mechanics.
5. **No sliders, anywhere.** Discrete card selectors only. Mouse path forgiving, keyboard path one-handed-friendly.
6. **Every meaningful state is preserved** *(non-negotiable)* — dated snapshots accumulate and are never overwritten; losing a day's state is the same failure class as a silent capture drop. *(See "Every meaningful state is preserved" above.)*
7. **Capture integrity is observable, not assumed** *(non-negotiable)* — every pipeline stage exposes a counter reconciled against the previous; silent drops are unacceptable. *(See "Capture integrity is observable, not assumed" above.)*
8. **Nothing leaves the device** *(non-negotiable)* — all user data stays on the local machine; an absolute, structural bar — no network I/O for user data, ever. *(See "Nothing leaves the device" above.)*
9. **TypeAssist grows with you, not over you** *(non-negotiable)* — not pre-trained; it learns the specific user; the kill-switch is per-user; it must never feel like a "worse autocorrect". *(See "TypeAssist grows with you, not over you" above.)*

1–5 override implementation convenience — if a feature seems to want a slider, find another shape. 6–9 are not subject to tradeoff at all.

## Recovery physiology

After stroke, fingers recover at physiologically different rates — not by the user's choice. **Thumb and index regain independent control fastest**; the outer three (middle, ring, little) are more tendon-interconnected and recover slowest, **ring and little especially**. This is anatomy, not effort. The engine must be finger-aware about it:

1. **Frame as physiology, not failure.** Higher slip rates and slower improvement on slow-recovery fingers are NORMAL physiology — never the user's failure. UI copy, progress framings, and insights treat a slow ring finger the way physical therapy treats a slow leg: expected, not a deficit.
2. **Weight correction priors by finger.** Downstream of L2, the correction engine should bias confidence by which finger is involved. A slip on a slow-recovery finger is more likely a motor error to smooth; an unusual key under thumb or index is more likely intentional and should be left alone. This applies to the L4 confidence tiers (`Cautious`/`Balanced`/`Eager`) and to swap-pair scoring in L3.
3. **Calibrate progress per finger — internally; never surface a finger in the UI.** Grade each finger against *its own* expected recovery curve so a slow finger is never made to feel like it's lagging — but keep that calibration **internal** (it informs engine priors and copy *tone*, not a per-finger readout). The **Progress view does not surface per-finger**: the "where your hands are gaining ground" per-finger breakdown is **retired**. **Why:** finger attribution is *inferred* from a standard touch-typing hand-map we can't trust for an adapted typist, and showing it implies those fingers *ought* to improve — the opposite of the dignity goal. This is a **refinement** of #3 this session, not a reversal: the per-finger correction *weighting* in #2 is unchanged, and the physiology-not-failure stance in #1 stands. (See *Insight system → Design principles for insight surfaces → Report what's observed, never what's assumed*.)

Sourced from the builder's lived stroke-recovery experience — load-bearing for both engine weighting and UI tone.

## Accessibility standards — non-negotiable

TypeAssist's users have motor difficulties (stroke survivors, arthritis). Large, mouse-forgiving targets and full keyboard operability are the *core of the product*, not enhancements. Every screen must meet all of these:

- **Target size.** Interactive controls are at least **36px tall** (aim for 44px) with comfortable padding. Prefer large hit areas — e.g. make a whole card clickable, with an inner control as the visible affordance.
- **Full keyboard navigation.** Every interactive element is reachable with **Tab** and activatable with **Enter and Space**. Use a logical DOM/tab order (primary navigation, then main content); never use positive `tabindex` — the visual order is the tab order.
- **Primary navigation = a single tab stop.** The sidebar is a **WAI-ARIA vertical tablist with roving tabindex** (matches native macOS sidebars): one Tab stop lands on the *selected* item, **Up/Down** (wrapping) + **Home/End** move focus between items, and Tab/Shift+Tab move out of/into the content. Use **manual activation** — arrows move focus only; **Enter/Space (or click) commits** the screen change — so a stray arrow never navigates (fewer accidental navigations for motor-impaired users). Mark items `role="tab"` with `aria-selected`, and the content region `role="tabpanel"` + `aria-labelledby` the active tab. Don't make the panel wrapper itself a tab stop; Tab should land on the content's first *interactive* element.
- **Visible focus rings — always.** Every focusable control shows a clear, high-contrast ring (the `--focus-ring` token) whenever focused. Style **`:focus`** (not only `:focus-visible`): a keyboard-first app must never hide focus, and `:focus-visible` silently drops when focus is moved *programmatically* (e.g. by the focus trap or roving-tabindex arrows) — which produced a real "vanishing ring" bug. There is also a global `:focus` ring in `app.css` as a backstop.
- **Focus must never escape the app (focus trap).** In a WebView, Tab past the last control hands focus to the host window — a ringless, non-DOM location — before wrapping. A root-level `keydown` handler wraps focus: Tab on the last tabbable → first, Shift+Tab on the first → last (computing tabbables live, respecting roving `tabindex="-1"`). This keeps the ring continuous on every screen, including ones with no content yet. Lives in `apps/tauri/src/App.svelte`.
- **Navigation lands focus on a sensible visible target.** The focus ring must never be invisible after a navigation. The **menu-bar panels** (the Progress dashboard, Practice) are their own windows: on open they land focus on a sensible visible target — Progress on its **selected tab**, Practice on its current-phase control — and **trap focus** inside the window (Tab wraps; **Esc** and click-away dismiss, and there's a close ✕ button, not a back chevron). For an **in-app** sub-view with a back path, focus lands on its primary anchor on arrival (via `.focus()` in `onMount`) and is **restored to the control that opened it** on return (child views dispatch `navigate-back` instead of `navigate`; the parent passes a one-shot `focusTarget` prop, cleared via `tick()` after the child mounts). Because rings are styled on `:focus` (not only `:focus-visible`), programmatic focus shows the ring immediately — no Tab needed.
- **One screen-title style, used everywhere.** Every main-app screen wraps its name in `<header class="screen-header"><h1>…</h1></header>` and gets the **same** title treatment — shared style in `apps/tauri/src/app.css`. An **in-app sub-view** with a back path adds a standalone focusable chevron button (`.screen-back`) before the `<h1>` — that chevron is the **only** visual difference from a top-level header (the title itself is identical). The **menu-bar panels** (Practice, and the Progress dashboard) are separate compact windows with their own header and a close ✕ button (+ Esc + click-away) — they don't use `.screen-header`/`.screen-back`. Where a title has a "· context" suffix (e.g. *Today · Friday, May 23*), wrap the suffix in `<span class="screen-context">` so it renders in the secondary color while the heading otherwise looks identical. Never hand-size or hand-style a title.
- **Fit the launch window — nothing important below the fold.** Every screen must lay out so all of its read-only content sits within the launch window without scrolling. Read-only content is *not* keyboard-reachable, so anything below the fold can only be reached with a mouse — which a keyboard-first app must not require. Pair a tight, compact layout with a Tauri default + `minHeight` that holds the densest screen, and cap any expandable list to what fits — or let it scroll *within* a fixed-size region (as the menu-bar Progress panel's Impact ledger does). Window dimensions live in `apps/tauri/src-tauri/tauri.conf.json`.
- **WCAG AA contrast.** Body and secondary ("quiet") text must clear **4.5:1** against the background in *both* light and dark mode (3:1 for large text and non-text UI). Use the `--text-secondary` token, not translucent gray — gray mixed with `transparent` fails contrast unpredictably over varied backgrounds.
- **Semantic elements.** Use real `<button>`/`<a>` with appropriate ARIA (e.g. `aria-current="page"` on the active nav item), never click-handler `<div>`s, so screen readers announce roles correctly.
- **No sliders, anywhere.** Discrete card selectors only (Principle #5). Enforced by CSS in `apps/tauri/src/app.css`.

Shared accessible tokens live in `apps/tauri/src/app.css` (`--text-secondary`, `--hairline`, `--focus-ring`) plus a global `:focus` ring. Reuse them on every new screen so these rules hold automatically.

## Insight system

TypeAssist surfaces insight across three surfaces — **Today**, **Progress**, **Practice** — each with a distinct job and voice. Never blur them.

**Two kinds of typing data:**
- **Ambient** — normal all-day typing everywhere on the Mac, captured passively. The core product.
- **Deliberate practice** — opt-in structured sessions (Practice mode, Warm-up).

**Content-blind, always.** The app never sees *what* was typed — only motor/timing shape (key, dwell, drift, episode rhythm). Insight speaks to **episode shape and patterns, never content**. Never **store or log** which app was focused — a timeline of app usage is surveillance and is forbidden in insight/history. (This is distinct from the ephemeral focused-app read M3 uses to choose a correction mode — see Principle #8: an in-memory read for a single decision that is never persisted is allowed; *recording* a usage timeline is not.)

### Today — the daily mirror (observational, read-only)

- A short, gentle **narrative readback** of the day. A friend giving a readback, never a clinician.
- **"What I noticed"**: a few specific, recognizable patterns (e.g. "right thumb on the spacebar"). **Descriptive, never prescriptive** — never "you should practice."
- **No clock-time / time-of-day timeline.** (Retired: time-of-day bands are context-free — a user can't connect "hesitant at 2pm" to anything. The meaningful variable is the **typing episode/effort** — cold starts, sustained-effort fatigue, recovery — not the clock.)
- **Observational, never a key-by-key grader.** Today reflects the day in plain language and **never grades you key-by-key** — per-key detail lives only in **Progress**, which is *pulled* (reached from the menu-bar panel via the tray, not linked from Today). Today's **one** call to action is the **warm-up card** (the gentle *Start warm-up*; see *Today carries the warm-up* below); apart from that it observes — no scores, no per-key grading. Today is a sidebar destination, so **no back-arrow**.
- **Honest empty states**: a morning with no data says "the day's just beginning" — never predictions or yesterday's baggage.
- **Today carries the warm-up; Home carries the Corrections invite.** The warm-up card (a gentle, optional **Start warm-up**, weighted to weak keys) lives on **Today**, in every app state. The *passive* "turn on Corrections" invite lives on **Home** (Fluent only). Both are **offered, never insisted on** — no nagging, no guilt. *(This supersedes the earlier "warm-up lives on Home, never on Today": the front-end design places the warm-up on Today.)*

### Progress — the menu-bar dashboard (Statistics + Impact)

A menu-bar dashboard so the user reads their own typing data without Console.app. Its own webview window (label `progress`), mirroring Practice Mode: tray-anchored, click-away to close, no Dock icon, fixed **400×600** — sized so the Impact glance (top 5 Coordination + top 5 Precision + the footer link) fits without internal scroll; longer content (e.g. Statistics) still scrolls within the panel region. Reloads its data each open. Reduce Motion honored by construction (no tab-switch animation — the highlight jumps). Lives in `apps/tauri/src/routes/ProgressPanel.svelte`; reads via the `read_word_patterns` / `read_progress_stats` Tauri commands. **Observe-only** — no correction yet.

- **Navigation — one-hand, no chords (hard rule).** Segmented control `Statistics | Impact`: switch by click, or single key `1`/`2` or `←`/`→` — **never** a modifier combo. Scroll = wheel/trackpad or single `↑`/`↓`/space. The keyboard focus ring follows the selected tab (roving tabindex) — ring and visible tab always agree.

**Statistics tab — how I type.**
- **Words today** + **Slip rate** — live same-day counters (today's in-progress row, not the decayed map).
- **Last 7 days** — 7 uniform volume bars (no highlighted "today"). Hidden until ≥2 days of data exist.
- **What the slip rate is made of** — two rows, **Coordination** ("right keys, right order") and **Precision** ("right key, clean hit"), each a neutral trend line + the current %, with a line stating `X% coordination + Y% precision = Z% slip rate`. The slip rate is shown as the **sum of the two displayed parts**, so the equation always holds on screen. **No per-finger view, no range selector, no time-of-day label.**
- **Granular empty states**: today's numbers show as soon as the user types today; the 7-day bars appear at ≥2 days; the trend lines appear once there are ≥2 weekly points (~2 weeks).

**Impact tab — what the system has learned and is ready to fix.**
- **Passive, read-only ledger** — no buttons, no actions, no tap targets. Rationale: **twelve catches is consent** — if the user didn't want a pattern fixed, it wouldn't have been caught 12×.
- Reads `word_patterns.json` directly (replaces scraping `KILL_SWITCH_DUMP` from Console). Each row's "N obs" is the **decayed** weight the kill-switch actually sees.
- Layout: observe-only banner → `N ready · M observing` summary → **Ready** group (≥12 obs) → "12-observation threshold" divider → **Observing** group (`n / 12`). Each row: `typed → target` (mono) + `coord`/`precis` tag + obs count + Ready/Observing pill.

**Slip definition & classification.**
- A **slip** = an immediate, same-word, small correction (edit distance ≤2) — a motor mistype the user caught. **Not** a left-in typo, **not** an editorial rewrite (rewrites are excluded by the edit-distance guard — validated: 14 corrections in a test session resolved to 7 true slips).
- The slip rate **decomposes** into Coordination + Precision, which partition the slips and sum to the total: **Coordination** = transposition (right keys, wrong order — `teh → the`); **Precision** = wrong adjacent key / doubling / omission, a clean-targeting failure (`wprd → word`).
- Classification is **derived on read** from the `typed → target` pair (keyboard adjacency + edit distance; L2 inter-key timing can sharpen it later) in `crates/correction-engine/src/slip_class.rs`. **Never persisted** — no format change to `motor_map.json` / `word_patterns.json`.

**Data & accumulation.**
- One daily append-only file: `~/.typeassist/progress_snapshots.json` — `{ date, words, slips, coord, precis }`, on-device only. Today's cards = the current in-progress day's row (seeded from disk on startup, so a mid-day restart **resumes** rather than zeroes); the 7-day bars = the last finalized rows; trends = daily rows rolled up to weekly points, plotting the whole accumulated history (no window selector).
- The live model (`motor_map.json`, `word_patterns.json`) keeps **decaying** (~30-day half-life) so correction reflects how the user types *now*; the snapshot file is **history** and is never decayed.
- All dev/test runs use the **`TYPEASSIST_DATA_DIR`** scratch valve so a build under test never writes real `~/.typeassist` (two writers on recovery data is a Principle #6/#8 hazard).

- **Future (not this phase):** "**slips smoothed for you**" (a count of silent corrections, framed as *help given*) lands once correction is live; the **clean-rate** framing ("landed clean 88% this week") and the opt-in **therapist export** (v2 footer) read back this same accumulated history; the footer restates the on-device privacy promise. **Never on this surface, ever:** WPM, streaks, daily scores, comparison to other users, goals/targets, prescriptions.

### Practice — opt-in "breathing exercise for the affected hand" (menu-bar panel)

Practice Mode lives in the **macOS menu bar** as a small dropdown panel (not a window), built for short, frequent use — two minutes between meetings. The whole app is menu-bar-only (Grammarly model): no Dock icon, one tray entry point, with the main window (Today/Progress/Settings) opened *from* the tray. Lives in `apps/tauri/src/routes/PracticePanel.svelte` (its own webview window, label `practice`); shell wiring (tray, Accessory policy, panel window) is in `apps/tauri/src-tauri/src/lib.rs`.

- **Calm, four-phase loop**: `Ready?` (a moment of intention) → typing → `Continue?` (the user controls the pace) → snapshot. Built around a breathing-exercise feel. A round shows sentences in **blocks of 3** (tunable `BLOCK_SIZE`); `Continue?` appears only at the end of a block, so the rhythm isn't broken after one sentence.
- **Slips shown live, but gently — never judgmentally.** As the user types, the actual key is rendered; a wrong key shows *immediately* as a slip marked **amber + wavy underline + a dot** (triple-coded so it reads without colour perception; never red). The caret **advances past** the slip (it never sticks) and backspace removes it. There is **no live score, percentage, timer, or WPM** — honesty about *what was typed*, but no judgement. (This refines the brief's original "no live feedback": the builder decided hiding slips felt dishonest; the calm comes from the gentle marking + absence of scoring, not from concealment.)
- **Real sentences, weighted to weak keys.** Short, real, **all-lowercase** sentences SELECTED from a curated bank (`apps/tauri/src/lib/sentences.txt`), scored by density of the user's weak keys (`query_weakest_keys` / the `engine://motor-stability` report) but with other keys appearing naturally. **Never generated by an LLM** — if repetition bites, grow the bank. Cold start: a sensible common set until the map has learned the user.
- **Snapshot at the end**: "what happened" (keys leaned into, observations added — the motor map's `total_observations` delta across the round) plus, when ≥2 daily snapshots exist, a per-key **trend** derived from the dated archives (`engine://practice-trend`). Early sessions gracefully show "what happened" only.
- **Deferred (clean seams left):** fatigue lines in the snapshot (L2 fatigue is deferred) and mode-calibration (Cautious/Balanced/Eager) — the L2→L3 kill-switch is off, so a mode switch wouldn't act yet. Add both when those layers land.
- All Practice copy lives in **one** place (`apps/tauri/src/lib/practiceCopy.ts`); sentence content in `sentences.txt`.

### Cross-cutting — progress is offered, never imposed

The same number that motivates on a good day can sting on a bad one. Progress is rich and available in the views the user **goes to** (Progress, Practice), framed as capability growing — never a daily verdict, notification, or streak that greets them. **The good day and the bad day both belong.**

### Design principles for insight surfaces (locked)

These govern Today / Progress / Practice and **extend** the non-negotiables (#8 capture integrity, #9 nothing leaves the device, #10 grows with you not over you).

- **Mirror, not coach.** Reflect how the user types *now*; never imply they should return to a baseline they've moved past. A survivor's ring/little fingers opting out is their **adapted normal**, not a gap to close. (This retired the per-finger "still finding footing" framing — see Recovery physiology #3.)
- **Report what's observed, never what's assumed.** Surface **keystrokes** — what the app actually sees — not **finger attribution**, which is *inferred* from a standard touch-typing hand-map that may be wrong for any given user. No finger-level claims in the UI.
- **Mirror, not scoreboard.** No targets, no good/bad coloring; a single neutral color. A flat trend reads as "holding steady," never "no progress." **Never use red for slips.**
- **Facts, not commentary.** No invented or clinical-sounding state words ("smoothing", "holding steady"). The numbers and the line speak for themselves; any interpretive word must come from defined criteria or the user's/therapist's vocabulary — never invented by the app.
- **Voice: observational, warm, never deficit-framed.** Count corrections, not mistakes.
- **Earn the space.** Every element appears only when it has something to say — granular empty states, no padding for its own sake.
- **Respect the user's locale.** Spelling and language follow the system setting (en-US, en-GB, en-IN…), never a hardcoded default, with a user override. *(Open item: Practice Mode currently shows British spelling on a US-set Mac — the word bank isn't yet reading the OS locale. Fix pending.)*

## Typing surfaces

TypeAssist sees user typing on three kinds of surface: the **ambient** OS-wide capture (no UI), **Warm-up** (opt-in, unmeasured), and **Practice** (opt-in, the menu-bar breathing panel). These share one rule and diverge on another.

- **Backspace always works (universal).** On every typing surface, backspace moves the caret back one character so the user can retype. Never block backspace, never discourage it — self-correction is signal, not failure (see Correction-engine state model → `SelfCorrected`).
- **Warm-up — smooth and advance.** A wrong key never blocks and never displays as an error: the caret advances one character and the *correct target character* appears (the slip is silently smoothed). No red, no "try again," no error state of any kind, anywhere. The caret must never stick waiting for the correct key. **Passages are always all-lowercase** — no proper nouns, no capitals, no shifted punctuation. Shift is a hard two-key chord for our users and Warm-up must never require it. Lives in `apps/tauri/src/routes/WarmUp.svelte`.
- **Practice — slips visible live, gently.** A wrong key appears *immediately* in the rendered stream, marked with **amber colour + a wavy underline + a dot** (triple-coded so the slip reads without colour perception; never red). The caret advances past the slip; backspace removes it (universal backspace rule applies). There is **no live score, percentage, timer, or WPM** — the surface is honest about what was typed but never judgmental, and measurement (keys, observations, trend) surfaces only in the end-of-round **snapshot**. All-lowercase sentences from the curated bank. Lives in `apps/tauri/src/routes/PracticePanel.svelte`. (The slip *capture* still flows through the ambient tap regardless.) See "Insight system → Practice" for the full flow.

## Correction-engine state model

- **Confidence tiers** (engine mode — how aggressive corrections are): `Cautious` (acts on High confidence only), `Balanced` (Medium+), `Eager` (Low+). Defined in `crates/correction-engine/src/lib.rs` as `ConfidenceTier`.
- **Space-error types**: `MissingSpace`, `ExtraSpace`, `ModifierDrift`.
- **Outcome states** (per word): `CleanHit`, `SelfCorrected`, `UncorrectedMiss`, `TwoKeysTogether`. All four are intentional — do not collapse to three. Self-corrections via backspace are signal, not failure.

### Outcome resolver (5a) — verdict state machine + limitations

The resolver is an **event + idle state machine** (`resolver::decide_verdict`), not a debounce. Core rule: **never fire a verdict mid-edit.** Four definitive triggers:
- **`CorrectedToOther` / `CorrectedToSuggestion`** — fire *immediately* when a successor token seals at the original's `start` with different content (a seal is unambiguous; no wait).
- **`Kept`** — fire when the token is still intact, the caret is **not** in/at its span, and its region has been idle ≥ `KEPT_IDLE_THRESHOLD_MS` (~5s; sized for slow-typing/stroke-survivor notice-pauses).
- **`Abandoned`** — fire when the token is wiped, no successor, the caret has moved ≥ `ABANDONED_CARET_MARGIN` chars from its `start`, and idle ≥ `ABANDONED_IDLE_THRESHOLD_MS` (~10s).
- otherwise stay `Pending`.

The thresholds + margin are **tunable** module constants (raise for slow typists). Idle is driven by **both** keystroke ticks and the engine's 1s watchdog tick (`tick_resolver` is called from the watchdog) — without the watchdog, an idle-due verdict would never fire when the user stops. `caret` is threaded through `OutcomeResolver::tick`. This replaced a single content-stability debounce that produced premature `Kept` (leading edge of backspace), premature `Abandoned` (mid-retype empty span), and silent non-resolution (user stops, no tick).

**Known limitations:**
- **Long in-place replacements vs `ABANDONED_CARET_MARGIN`.** Abandoned is held off while the caret stays within the margin (default 8 chars) of a wiped token's start. Retyping a replacement **longer than the margin** *without sealing it* (no boundary typed) and then pausing past the abandon idle can mis-fire `Abandoned`. Normal retypes seal (a space/punctuation) → `CorrectedToOther` fires first, so this only bites unsealed long edits. Tune the margin if real usage hits it.
- **Selection-replace is not covered.** Mouse-select-and-overwrite corrections (select a word, type over it) may not move the caret *through* the token's region the way backspace-then-retype does — and selection events may not even reach the engine via the sidecar (the L1 tap streams keystrokes, not selection/caret state). So a select-and-replace correction can still resolve `Kept` and miss the slip. **TODO:** add a selection-aware signal in a future iteration.

### Motor map (5c) — v0 design notes

The motor map (`crates/correction-engine/src/motor_map.rs`) is the engine's first per-user learner: a `HashMap<char, SlipDistribution>` keyed by *intended* character, built passively from resolved `Kept` / `CorrectedToOther` outcomes (30-day half-life decay). It is **observe-and-store only** — the **L2→L3 kill-switch stays OFF**; nothing here feeds a correction back until the map is flipped manually after the gathered data looks sensible.

**Persistence.** The live `~/.typeassist/motor_map.json` is flushed by `engine::flush_motor_map` on a **time cadence** (every `MOTOR_FLUSH_INTERVAL_MS`, 2s, when `MotorMap::has_unsaved()`), driven by the 1s watchdog. This is the primary durability path — the post-loop shutdown `save_to` does **not** reliably run under `tauri dev` (no `RunEvent`/exit hook in `lib.rs`, so the engine task is aborted at its await on app exit). **Dated snapshots** (`snapshots/YYYY-MM-DD.json`) are the durable history, independent of the live-file cadence, written per **Principle #6**: **daily** (the watchdog checks every ~10 min and writes when a day has elapsed) **plus one at startup before any writes** (preserving the as-loaded state). The per-day filename dedupes, so repeated launches keep the day's opening state. The Practice trend (`engine://practice-trend`) reads these dated files, so daily snapshots also give the trend daily (not weekly) granularity.

Four conscious v0 simplifications — **intended behaviour, not bugs.** Don't "fix" them without a product decision:

1. **Reads use `last_now`, not wall-clock.** `query_*` / `confidence` decay against the most recent observation time (the map's read methods take no `now`), so a query made long after the last keystroke shows slightly stale values. Fine live; revisit with a `refresh(now)` if cold queries ever matter.
2. **Transpositions log as two substitutions, not a swap.** Alignment is plain Levenshtein (no Damerau transposition op, no spatial cost), so `teh→the` records as two subs — spurious cross-pair noise that washes out at scale. Revisit with Damerau-Levenshtein only if real data shows transpositions dominate.
3. **Pruning at `1e-4` drops single-observation slips after ~390 days.** A slip seen once and never again decays below the prune floor (~13 half-lives) and is forgotten. This is the intended forgetting policy, not a leak.
4. **Outcomes are observed per resolver transition, with no retraction.** The resolver is revisable (a record can flip `Kept` → `CorrectedToOther`); the proposer retracts on flip, but the motor map does not. So a word kept-then-corrected counts its `Kept` positives *and* the correction's matches/slips — a mild upward bias on confidence for re-edited words. Acceptable while the kill-switch is off and we're only gathering data; add retraction (or count only the final outcome at anchor retirement) before flipping.

### Live correction (M3 Step 1) — manual allow-list, off by default

The first feature that **modifies live typing**. Built conservatively: the default behaviour is to do nothing. Deliberately a **manual-first** approach, distinct from the *automatic* per-pattern classifier (`word_pattern.rs` / `kill_switch.rs`), which stays **observe-only** and is NOT used to gate Step-1 corrections.

#### Automatic classifier (`kill_switch.rs`) — suggest-only, risk-tiered (observe-only)

The per-pattern classifier (`classify` → `PatternReadiness`) was rebuilt 2026-06-18 and is still **observe-only** (only debug-logged; nothing reads it into an injection). Its model:

- **No silent path — ever.** The Tier-1 "silent auto-fix" outcome is **retired**. Every correction is a **suggestion the user confirms** (confirm-to-accept). `PatternReadiness` has exactly two arms: `Suggest { tier }` and `Observe { reason }`. No confidence, however high, marks a pattern for silent application (Principle #9 — never a "worse autocorrect").
- **Risk-tiered evidence bar** (replaces the flat 12): a **non-word source** (`teh → the`) clears a **low bar** (`NONWORD_SOURCE_EVIDENCE_BAR`, placeholder ~4 — calibrate once the suggestion UI gives accept/undo signal); a **real-word source** (in the dictionary *or* the user's learned vocab — the **hard vocabulary rule**) must clear the **high bar** (`REALWORD_SOURCE_EVIDENCE_BAR`, 12). `is_known` already unions dictionary + learned vocab, so one call covers both.
- **Motor-Map-aware bar (2026-06-18, observe-only).** The evidence bar is *eased* by the **affectedness** of the keys the slip touches — physiology substitutes for repetition: a slip on a key the motor map shows slipping often is far likelier a true motor miss than a change of mind, so it needs fewer sightings. `MotorMap::affectedness(key)` = the decayed slip rate (`1 − confidence`), **gated by `MIN_SAMPLES`** (under-sampled → 0; no easing without evidence). `build_facts` takes the **max** affectedness over `slip_class::involved_keys` (one strongly-affected key suffices). `evidence_bar(typed_is_known, affectedness)` relaxes the tier base toward a floor, **graded linearly**, saturating at `STRONG_AFFECTEDNESS` (0.20): non-word base 4 → floor `AFFECTED_BAR_FLOOR` (2); real-word base 12 → floor = the non-word base (4), so a real-word slip stays strictly more cautious even fully eased. As the hand recovers and slip rates fall, the bar tightens on its own. **Easing only lowers the repetition requirement — it never crosses a structural gate** (single-motor shape, single-letter source/target, real-word/vocab caution, never-silent all checked first); a unit test pins that max affectedness can't rescue a rejected pair. The teach-stop (3-strike undo brake) keeps a generous bar from nagging once live. `classify`/`classify_explained` take `&MotorMap`; `classify_explained` returns affectedness + bar_used + base_bar + `surfaced_early`. The shadow log marks `EARLY(affected-key bonus)` rows with `affected=.. bar=used/base`; the Impact UI (`read_word_patterns`) loads `motor_map.json` so its `ready` reflects the same eased bar.
- **Target gate:** target not a real word → always `Observe` (never steer toward a non-word); a real-but-uncommon target still earns a suggestion (commonness no longer gates now that nothing is silent).
- **Structural disqualifiers, at any count:** the strict **single-motor-error filter** (`slip_class::single_motor_edit` — one adjacent-key substitution / transposition / dropped letter / extra letter, *stricter* than `classify_slip`'s edit-distance ≤2, so it drops word-swaps like `so → for`, non-adjacent subs, and cross-token merge artifacts); **single-letter source** (`s → is`) and **single-letter target** (intent-ambiguous, never nominated — `a`/`i` are known words so they take the cautious real-word path and are never themselves a target). Plus the existing **3-strike undo brake** → `Observe { BrakeTripped }`.
- **Future suggestion UI — accept key is Shift, not Tab.** When the confirm-to-accept UI is built, the accept gesture is a **deliberate isolated Shift tap** while a suggestion shows (reachable by *either* hand — an accessibility choice for one-handed/motor-impaired users; Tab is harder to reach one-handed). Undo stays a bare Escape (above). *(Not built yet — recorded so the key choice isn't lost.)*
- **Impact/Progress UI reconciled (2026-06-18).** `read_word_patterns` (`apps/tauri/src-tauri/src/lib.rs`) now drives the Impact ledger off the risk-tiered `classify`, not the lenient `classify_slip`/flat-12 it used before. A row appears **only** if the pattern is a genuine motor slip (`Suggest`, or `Observe{InsufficientEvidence|Stale|BrakeTripped}`); structural `Observe` reasons (word-swap / single-letter / non-word target / merge / unseen) are dropped, so they no longer masquerade as slips. `ready` is the real readiness (`Suggest` cleared its tier bar); the coord/precis tag comes from `single_motor_edit` (Transposition = coord, else precis). On the real store this took the ledger from ≈457 lenient rows to ~334 genuine slips. Both `ProgressPanel.svelte` (menu-bar glance) and `Progress.svelte` (in-app) consume it; neither renders pills, so no Svelte change was needed.
- **Live shadow dry-run log (observe-only, 2026-06-18).** With capture running, whenever the classifier WOULD surface a suggestion for a just-sealed word, one line is appended to **`~/.typeassist/shadow_suggestions.log`** (honors `TYPEASSIST_DATA_DIR`): `<now> would-suggest  typed -> target  tier=…  evidence=<decayed weight>  edit_dist=<levenshtein>`. **Applies nothing** — independent of the (off) master gate. Tail with `tail -f ~/.typeassist/shadow_suggestions.log`. Hooked at the end-of-line seal in `engine.rs` (`shadow_suggestion` / `append_shadow_log`); no new privacy surface (the same pairs already live in `word_patterns.json`).

- **Source of truth: a manual allow-list** — `~/.typeassist/allow_list.json` (`apps/tauri/src-tauri/src/allow_list.rs`): a global **master gate** (`correction_enabled`, default **off**) + the user-enabled `typed → target` patterns. Empty list OR gate off ⇒ nothing is corrected. The **engine task is the sole writer**; the UI reads it read-only and mutates via `EngineControl` (no two-writer race — Principle #6/#8). Honors `TYPEASSIST_DATA_DIR`.
- **Where it fires:** only at the **forward end-of-line word boundary** (space / punctuation) when the sealed Word matches an enabled pattern. **Not on Return** (Return often commits/sends — unsafe to post-correct) and **not on mid-line/replay re-tokenisation**. Injection reuses `OutboundCommand::InjectCorrection { delete_count, replacement }` (delete the word + boundary, retype target + boundary).
- **Echo-skip is load-bearing.** The L1 tap re-captures our own injected keystrokes (`.cgSessionEventTap` sees posted events). An `eventSourceUserData` tag did **not** survive the post→tap round-trip in practice, so echo is dropped **engine-side by exact count** (`pending_echo` = `delete_count` + replacement chars) — skipped before the funnel, the pipeline, and the undo check. Never re-introduce learning/correcting from injected events. *(Follow-up: the count-based skip can drift if echo events are dropped/merged — add a robustness check.)*
- **One-key undo + teach-stop:** within `UNDO_WINDOW_MS` after a fix, a **bare Escape** reverts it (reverse injection) **and removes the pattern from the allow-list** (won't recur until re-enabled). Any other key is an implicit accept (disarm). Escape reaches the engine because the L1 tap special-cases **keycode 53 → U+001B** (its unicode string is otherwise empty). Escape is observed, not suppressed (it still reaches the app).
- **Surfaces:** menu-bar **"Enable corrections"** check (instant global off) + **Corrections…** panel (`AllowlistPanel.svelte`, discrete on/off toggles over learned patterns, no sliders) + a universal **cue** HUD (`Cue.svelte`, shown by Rust without focus; not anchored to the word, since per-word geometry is native-Cocoa-only). The Progress→Impact tab stays read-only.
- **Observability (Principle #7):** every fix/undo logs `CORRECTION_APPLIED` / `CORRECTION_UNDONE` and increments the funnel's `c_corrections {applied, undone}`.
- **Caret-move safety (Fix-B) — a fix never fires against a stale line model.** The engine dead-reckons the current line from keystrokes alone, so a caret move it can't observe would let a correction delete at the wrong position (the `edndd` desync). An immediate **line reset** — the same one newline runs: clear `line_buf`/`line_dwells`/`caret`/anchors, `tokenizer.reset_line()`, emit `EVT_LINE_RESET`, **and disarm any pending Escape-undo** (its revert would inject at the stale caret) — is triggered on **exactly three discrete, content-free signals** (Principle #9 — no coordinates, no text, only a `mouse`/`focus`/`updown` log tag):
  1. **Mouse / trackpad click** — the L1 tap also watches `leftMouseDown`/`rightMouseDown`/`otherMouseDown` (a trackpad click *is* a mouse-down) → emits `InputEvent::CaretMoved`.
  2. **Focus / app change** — the secure-field `AXObserver`'s focused-element change + the `didActivateApplication` app switch → `CaretMoved`.
  3. **Up / Down / PageUp / PageDown** — engine-side, where `nav_action` would otherwise no-op into a silent desync.

  Plain **Left/Right/Home/End stay caret-only** via `nav_action` (the model caret tracks them, so no reset is needed). Bias toward resetting: an extra reset only skips the *next* word's correction (safe); a missed reset is what deletes the wrong thing. **Lesson — `kAXSelectedTextChangedNotification` is deliberately NOT a caret-move trigger.** It fires on every caret advance during **normal typing**, so it conflates typing with navigation — using it as a reset signal wiped the line after each keystroke and starved correction (the engine could never accumulate a word). It stays *subscribed* only for the secure-field re-eval (which emits nothing to stdout). **Don't re-introduce selection-changed as a reset signal — it can't be told apart from typing.**
- **macOS autocorrect coexistence (open):** the OS corrects common dictionary typos (e.g. `teh→the`) on the same stream — indistinguishable to the user from ours and it races our seal. For now, observe/report (deferring-to-autocorrect is a later step). Our gate stops *our* corrections instantly; only the OS lingers.

## Ghost-key signals (L2)

Ghost-key detections are **low-confidence candidate signals only** — the aggregator flags phantom-like patterns (graze-short dwell, rapid same-key repeats, key-then-immediate-backspace) but cannot confirm intent. Confidence stays low until the slip-detection loop provides labeled corrections.

## Target platform

macOS 13 (Ventura) or later. Windows and Android adapters are future work; the L1 boundary is shaped to make them pluggable. L2–L4 must never call OS-specific APIs directly.

## Build & dev

> **⚠️ This Mac does NOT have `just` installed — never recommend, relay, or run any `just <recipe>` command here. Always give the plain shell equivalent instead.** The `justfile` recipes below are reference only; translate each to its underlying command. The ones used most:
> - **Normal dev (real data):** `cd ~/Projects/typeassist/apps/tauri && npm run tauri dev`
> - **Sandbox preview (throwaway data — never touches real `~/.typeassist`):** `cd ~/Projects/typeassist/apps/tauri && TYPEASSIST_DATA_DIR="$HOME/.typeassist-sandbox" npm run tauri dev`
> - **Clean the sandbox:** `rm -rf ~/.typeassist-sandbox`

`just` is the canonical entry point in the repo, but (per the note above) **do not invoke it on this machine** — use the shell equivalents. From the repo root:

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

- **L2 fatigue and temporal aggregators are deferred until after L4 correction exists.** Both are **modifiers** — they adjust *how much help* the correction engine gives (e.g. lean in more as a hand or finger tires within a session; bias differently for cold-start morning vs steady afternoon). With no correction engine to modulate, there's nothing for them to act on, so building them now would be observation-without-purpose. Revisit once L4 is live — the fatigue-aware "help more when you're tiring" idea is valuable then.

- **TODO — Privacy & Terms route is unbuilt (placeholder only).** The `privacy` route in `apps/tauri/src/App.svelte` renders only the generic "Shell only — this screen's content is coming next." placeholder; there is no real copy yet. **Canonical copy spec lives in design doc v23** — the *What I learn*, *What I never keep*, and *Why it works this way* (philosophy) strings. When this route is built, author the page to match design-doc v23 verbatim (mind the app's em-dash + curly-quote punctuation style). Note v23's copy already accounts for the C5e local vocabulary tally (the "everyday vocabulary / private count of dictionary words" line).

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
│   └── correction-engine/ # L4 — engine + `persist.rs` (durable JSON store I/O)
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
