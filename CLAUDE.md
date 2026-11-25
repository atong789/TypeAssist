# TypeAssist

Native macOS app that helps users with motor difficulty type more accurately by learning each user's personal motor patterns and correcting typos via a spatial volatility map (not a generic dictionary).

**Naming.** The user-facing product is **TenCalmDigits** and its assistant voice is **Jordan** (what the UI, menu bar and permission lists show; bundle id `app.tencalmdigits`). **TypeAssist** is the internal codename that survives in the repo, the crate/binary names and the on-device data dir `~/.typeassist/`. Both names appear in this file; they are the same app.

## Nothing leaves the device (Principle #8 — non-negotiable)

All user data — keystrokes, motor map, snapshots, slip patterns, fatigue signals, mode preferences — stays on the user's local machine. No cloud sync, no analytics, no telemetry, no crash reports, no shared learning across users. **The application has no network calls related to user data, ever.** This is structural, not optional: do not add an HTTP client, analytics, or remote logging.

**Rationale:** this is recovery data — it reveals more about a person's body and cognitive state than most medical records. The privacy bar must be **absolute, not best-effort**. Any future feature that would require sending data off-device is **rejected by default — there is no acceptable tradeoff that opens this door.**

**Ephemeral contextual checks are not data collection.** The precise rule is: *nothing leaves the device, and no user activity is stored or logged.* Reading a transient piece of context that exists **only in memory, for a single decision, and is never persisted** is acceptable — it is not collection. The motivating case (M3): reading the **currently focused app's bundle ID** to pick that app's correction mode (Mail → Eager, Notes → Cautious, …). The bundle ID is used for one decision and discarded — never written to disk, never logged, never associated with keystrokes or content. The line is **persistence/logging**, not *observation*: an in-memory read for an immediate choice is fine; recording *which app you were in* (a timeline of your activity) is the surveillance this principle forbids.

## Capture integrity is observable, not assumed (Principle #7 — non-negotiable)

Every stage of the pipeline (**L1 ingest → engine accept → sealing → 5a verdict → 5c observe → persistence**) exposes a cumulative counter reconciled against the previous stage, so the conversion ratio between adjacent stages is auditable in real time. **Silent drops are unacceptable.** For a recovery-tracking app, a user recovering fine motor control whose week of typing produces 1% of the expected data hasn't been *underserved* — the product has **lied to them about their recovery**. That is the worst failure mode the system has, worse than a wrong correction. This is foundational, not a feature.

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

Files under `~/.typeassist/` (all honor `TYPEASSIST_DATA_DIR`; the engine task is the sole writer of every one):

| File / dir | What it holds | Notes |
|---|---|---|
| `motor_map.json`, `snapshots/YYYY-MM-DD.json` | Per-key slip distributions; daily dated archives | Principle #6 |
| `word_patterns.json` | Learned `typed → target` patterns (decayed weight) + per-pattern bubble `outcomes` block | decays (~30-day half-life) |
| `word_freq.json`, `word_freq_snapshots/` | Private count of kept, known vocabulary words | gated by `is_known`; observe-only |
| `learned_lexicon.json` | The Confirmed learned-words set (so it survives a restart) | read on demand by `read_learned_lexicon` |
| `progress_snapshots.json` | One row per day: words, slips, coord, precis, bubble scorecard counts | never decayed |
| `allow_list.json` | The Corrections master gate (`correction_enabled`) | patterns list retired |
| `guess_accuracy.json` | Observe-only guesser scoreboard | diagnostic |
| `resolution_diary.jsonl`, `resolution_diary.on`/`.off`, `device_id` | 14-day per-event bubble outcomes + its consent flags + random per-install id | see *Resolution diary* |
| `shadow_suggestions.log`, `bubble_debug.log` (+ `bubble_debug.on`), `debug.log`, `inject_config` | Local diagnostics / dev switches | not learning; not in backups |

**Progress read commands** (`apps/tauri/src-tauri/src/progress_reads.rs`, `lib.rs`; all pure reads — no engine round-trip, no writes, and a bad file is never quarantined from a read): `read_resolution_diary` (14-day outcomes + `enabled` + `tracking_since`), `read_learned_lexicon` (the learned set from `learned_lexicon.json` — *not* `word_freq.json`), `read_pattern_outcomes` (all-time per-pattern `shown / accepted / self_matched / undone` + `typed`/`target`, most-ignored first, including outcome-only pairs that Impact hides; `state: "untracked"` means no number may be inferred — never zeros), plus the existing `read_word_patterns` / `read_progress_stats`. `cargo test -p typeassist-app real_state_smoke -- --ignored --nocapture` runs the three reads read-only against the real (or `TYPEASSIST_DATA_DIR`) data dir.

**Settings & data layer.** Settings hosts the Corrections switch (same state as the tray check), the read-only language line, **Open at login** (a real `SMAppService` login item, ON by default once), and **Back up / Restore / Delete everything** (`apps/tauri/src-tauri/src/backup.rs`): one OS-portable, versioned bundle (`schema_version`, currently 2 — it carries the learned stores, the dated archives and the diary, never machine paths); **restore is a true replace, never a merge**, validated whole-bundle before any write; the engine performs the writes. A **Reconnect** panel appears when macOS revokes Accessibility/Input Monitoring, and the tray shows a stopped-capture state with auto-reconnect.

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

After an injury or condition affecting hand motor control, fingers recover at physiologically different rates — not by the user's choice. **Thumb and index regain independent control fastest**; the outer three (middle, ring, little) are more tendon-interconnected and recover slowest, **ring and little especially**. This is anatomy, not effort. The engine must be finger-aware about it:

1. **Frame as physiology, not failure.** Higher slip rates and slower improvement on slow-recovery fingers are NORMAL physiology — never the user's failure. UI copy, progress framings, and insights treat a slow ring finger the way physical therapy treats a slow leg: expected, not a deficit.
2. **Weight correction priors by finger.** Downstream of L2, the correction engine should bias confidence by which finger is involved. A slip on a slow-recovery finger is more likely a motor error to smooth; an unusual key under thumb or index is more likely intentional and should be left alone. This applies to the L4 confidence tiers (`Cautious`/`Balanced`/`Eager`) and to swap-pair scoring in L3.
3. **Calibrate progress per finger — internally; never surface a finger in the UI.** Grade each finger against *its own* expected recovery curve so a slow finger is never made to feel like it's lagging — but keep that calibration **internal** (it informs engine priors and copy *tone*, not a per-finger readout). The **Progress view does not surface per-finger**: the "where your hands are gaining ground" per-finger breakdown is **retired**. **Why:** finger attribution is *inferred* from a standard touch-typing hand-map we can't trust for an adapted typist, and showing it implies those fingers *ought* to improve — the opposite of the dignity goal. This is a **refinement** of #3 this session, not a reversal: the per-finger correction *weighting* in #2 is unchanged, and the physiology-not-failure stance in #1 stands. (See *Insight system → Design principles for insight surfaces → Report what's observed, never what's assumed*.)

Sourced from the builder's own lived experience with motor impairment and hands-on accessibility research — load-bearing for both engine weighting and UI tone.

## Accessibility standards — non-negotiable

TypeAssist's users have motor difficulties (limited fine motor control, hand tremor, arthritis, and similar conditions). Large, mouse-forgiving targets and full keyboard operability are the *core of the product*, not enhancements. Every screen must meet all of these:

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
- **Deliberate practice** — opt-in structured sessions: the Practice panel, which the UI calls **"Warm-up"** (tray item, Today's warm-up card). They are one surface.

**Content-blind, always.** The app never sees *what* was typed — only motor/timing shape (key, dwell, drift, episode rhythm). Insight speaks to **episode shape and patterns, never content**. Never **store or log** which app was focused — a timeline of app usage is surveillance and is forbidden in insight/history. (This is distinct from the ephemeral focused-app read M3 uses to choose a correction mode — see Principle #8: an in-memory read for a single decision that is never persisted is allowed; *recording* a usage timeline is not.)

### Today — the daily mirror (observational, read-only)

- A short, gentle **narrative readback** of the day. A friend giving a readback, never a clinician.
- **"What I noticed"**: a few specific, recognizable patterns (e.g. "right thumb on the spacebar"). **Descriptive, never prescriptive** — never "you should practice."
- **No clock-time / time-of-day timeline.** (Retired: time-of-day bands are context-free — a user can't connect "hesitant at 2pm" to anything. The meaningful variable is the **typing episode/effort** — cold starts, sustained-effort fatigue, recovery — not the clock.)
- **Observational, never a key-by-key grader.** Today reflects the day in plain language and **never grades you key-by-key** — per-key detail lives only in **Progress**, which is *pulled* (reached from the menu-bar panel via the tray, not linked from Today). Today's calls to action are its two gentle offers — the **warm-up card** (the gentle *Start warm-up*) and, once there's data, the **Corrections offer** (see *Today carries both gentle offers* below); apart from those it observes — no scores, no per-key grading. Today is a sidebar destination, so **no back-arrow**.
- **Honest empty states**: a morning with no data says "the day's just beginning" — never predictions or yesterday's baggage.
- **Today carries both gentle offers: the warm-up and the Corrections invite.** There is **no Home screen** — it was retired in the front-end redesign and its invite moved onto Today. The warm-up card (a gentle, optional **Start warm-up**, weighted to weak keys) is on **Today** in every state (with no learned data yet, Today shows the welcome / "How I learn" state with a "Start here" warm-up). Once there's data, Today also shows the passive Corrections offer (shown once ≥3 patterns are ready, or a persistent on/off read once decided). Both are **offered, never insisted on** — no nagging, no guilt.

### Progress — the menu-bar dashboard (Statistics + Impact)

A menu-bar dashboard so the user reads their own typing data without Console.app. Its own webview window (label `progress`), mirroring Practice Mode: tray-anchored, click-away to close, no Dock icon, fixed **400×600** — sized so the Impact glance (top 5 Coordination + top 5 Precision + the footer link) fits without internal scroll; longer content (e.g. Statistics) still scrolls within the panel region. Reloads its data each open. Reduce Motion honored by construction (no tab-switch animation — the highlight jumps). Lives in `apps/tauri/src/routes/ProgressPanel.svelte`; reads via the `read_word_patterns` / `read_progress_stats` Tauri commands. **Read-only** — it reads and reports; nothing on this panel changes anything or acts on the user's typing.

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
- One daily append-only file: `~/.typeassist/progress_snapshots.json` — `{ date, words, slips, coord, precis, bubbles_shown, accepted, self_matched }` (the last three are the bubble scorecard counters — counts only, no words), on-device only. Today's cards = the current in-progress day's row (seeded from disk on startup, so a mid-day restart **resumes** rather than zeroes); the 7-day bars = the last finalized rows; trends = daily rows rolled up to weekly points, plotting the whole accumulated history (no window selector).
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

### Resolution diary — per-event bubble outcomes (consent-gated, local, 14 days)

An append-only `~/.typeassist/resolution_diary.jsonl` (honors `TYPEASSIST_DATA_DIR`), one row per bubble resolution: `{v, id, ts, dev, typed, target, outcome: taken|retyped|ignored, supersedes?}`. It is a per-event, timestamped layer *beside* the all-time decayed stores (unchanged) so Progress can show the actual words for the last 7 days / week-over-week and two Macs can **merge**. Code: `apps/tauri/src-tauri/src/resolution_diary.rs`; capture hooks in `engine.rs` (the `BubbleLedger` owns the diary).

- **It stores typed words — a deliberate, bounded exception to "content-blind".** Exactly four content fields (typed, target, outcome, ts) — no app id, no context, no position. **Consent-gated — no recorded choice = off** (details below); opting out **deletes the log**. Hard **14-day retention** and a 2 MB cap (oldest trimmed first). Never network (Principle #8); merge is a user-initiated file import/export.
- **Principle #6 exception, stated plainly:** this file is deliberately pruned — short-lived *view* data, not recovery history (the dated snapshots remain the durable record).
- **Principle #7:** `c_decided` vs `c_appended` are reconciled at the funnel-dump cadence (`DIARY_DUMP`; a gap logs `DIARY_INTEGRITY_BROKEN`).
- **Merge = union by event `id`** (idempotent). A `retyped` row carries `supersedes: <ignored row id>`; the reader folds the pair so one bubble counts once. `dev` is a random per-install id (`device_id`), never hardware-derived, and is **not** in the backup. The diary rides in the backup bundle (schema v2).
- **`tracking_since` ("collecting since").** The moment consent was given / the user switched the note on is stamped (epoch ms) *inside the `resolution_diary.on` flag* and returned by `read_resolution_diary`. It is **durable** — it does NOT move as rows prune (unlike `oldest_ts`, the oldest *retained* row), survives restarts, and a repeat consent or switch-on never moves it; off → `None`; switching back on starts a new period. An empty legacy flag falls back to its own creation time.
- **`retyped` reads low** — it inherits the self-match limits (see `BubbleLedger`): it needs a sealing boundary (space/punctuation) after the fix, an intact line model (a click/focus change resets it), and the fix inside the 30 s window. A partial edit that keeps a correct prefix *does* count.
- **Default-on, gated on consent (wording signed off by the product owner).** Collection starts only when **onboarding completes**: step 3 shows the consent line ("To show you your progress, I keep a quiet 14-day note of the words you slip on and whether you took the word I offered — never your passwords, never the sentences around them, and never off this Mac."), and `completeOnboarding()` in `App.svelte` — the single place — calls `record_diary_consent` → `ResolutionDiary::consent` writes the `resolution_diary.on` flag. Fresh install that never finishes onboarding = **no collection** (no flag, no log, no device id). Existing users who skipped this onboarding have no recorded choice → off. **Only the `on` flag enables it** — there is no env-var or other back door. The Settings "14-day progress note" switch (the only other caller of `set_resolution_diary`) turns it off anytime: off **deletes the log** and writes `resolution_diary.off`, which makes any later consent (e.g. a replayed onboarding) a no-op; switching it back on clears `off`. The Privacy page's "What I never keep" was updated to match ("keep no log of your sentences — only a quiet 14-day note of the words you slip on and what you did, so I can show your progress."). Locked by `resolution_diary::tests` (behaviour) and `consent_lock.rs` (frontend wiring + verbatim copy).

### Cross-cutting — progress is offered, never imposed

The same number that motivates on a good day can sting on a bad one. Progress is rich and available in the views the user **goes to** (Progress, Practice), framed as capability growing — never a daily verdict, notification, or streak that greets them. **The good day and the bad day both belong.**

### Design principles for insight surfaces (locked)

These govern Today / Progress / Practice and **extend** the non-negotiables (#7 capture integrity, #8 nothing leaves the device, #9 grows with you not over you — the canonical 1–9 list above).

- **Mirror, not coach.** Reflect how the user types *now*; never imply they should return to a baseline they've moved past. A user's ring/little fingers opting out is their **adapted normal**, not a gap to close. (This retired the per-finger "still finding footing" framing — see Recovery physiology #3.)
- **Report what's observed, never what's assumed.** Surface **keystrokes** — what the app actually sees — not **finger attribution**, which is *inferred* from a standard touch-typing hand-map that may be wrong for any given user. No finger-level claims in the UI.
- **Mirror, not scoreboard.** No targets, no good/bad coloring; a single neutral color. A flat trend reads as "holding steady," never "no progress." **Never use red for slips.**
- **Facts, not commentary.** No invented or clinical-sounding state words ("smoothing", "holding steady"). The numbers and the line speak for themselves; any interpretive word must come from defined criteria or the user's/therapist's vocabulary — never invented by the app.
- **Voice: observational, warm, never deficit-framed.** Count corrections, not mistakes.
- **Earn the space.** Every element appears only when it has something to say — granular empty states, no padding for its own sake.
- **Respect the user's locale.** Spelling and language follow the system setting (en-US, en-GB, en-IN…), never a hardcoded default, with a user override. *(Resolved: Practice and Warm-up read the OS locale via `resolveVariant(loadSpellingPref())` in `apps/tauri/src/lib/locale.ts`; the sentence bank is stored British and Americanized through the locale `PAIRS` map (`sentenceBank.ts` fails loudly on an unpaired British word). Settings shows the active language read-only — no picker, Principle #5. The same variant also drives the engine's suggestion spelling.)*
- **Locale defines the correct spelling; the opposite variant is a slip against it** *(UK English, v0.3.0 — supersedes the earlier "spelling variant is authorship, never flagged").* The lexicon bundles both US and UK spellings, but membership is **locale-gated**: once L5 reports the system locale (`set_spelling_variant` → `Lexicon::set_spelling_variant`), the **opposite-locale** spelling of a VarCon variant pair is excluded from `is_known` — on a British system `color`/`colors`/`coloured…` are unknown, on an American system `colour…` are. So a garble converges **only** on the locale spelling (`coluor → colour`, deterministically, independent of incidental learning) and a cleanly-typed opposite variant routes through the slip engine toward the locale spelling (symmetric US↔UK). **Nothing is auto-applied** — the user still confirms via Shift or dismisses (Principle #9). Two guarantees keep it from regressing: (1) the gate wins over the runtime-learned set (a learned variant can never be `is_known`), and (2) the **learner is guarded** — `LexiconProposer::note_record` never confirms a `spelling_variant::is_variant_word`, so a variant can't be re-learned back into "known." Novel words (names, jargon) are not in the variant map and learn normally. Before a locale is set the gate is off (both variants known — the pre-locale tolerant default, so startup and tests are unaffected). Lives in `crates/correction-engine/src/{lexicon,spelling_variant,lexicon_proposal}.rs`; `LEXICON_VERSION` is now **4**.

## Typing surfaces

TypeAssist sees user typing on two kinds of surface: the **ambient** OS-wide capture (no UI) and **Practice** (opt-in, the menu-bar breathing panel — labelled **"Warm-up"** in the UI). *(There used to be a separate in-app Warm-up surface with silent smooth-and-advance; it no longer exists — `WarmUp.svelte` is gone and the "Warm-up" entry points (tray item, Today card) open the Practice panel.)* These share one rule.

- **Backspace always works (universal).** On every typing surface, backspace moves the caret back one character so the user can retype. Never block backspace, never discourage it — self-correction is signal, not failure (see Correction-engine state model → `SelfCorrected`).
- **Retired: "smooth and advance" warm-up.** The old rule — a wrong key silently shows the *correct target character* and the caret advances — applied to the retired standalone Warm-up and is **superseded** by Practice's honest, gentle slip marking below. What survives from it: **no red, no "try again," no error state of any kind**; the caret never sticks waiting for the correct key; and **passages are always all-lowercase** (no proper nouns, capitals or shifted punctuation — Shift is a hard two-key chord for our users).
- **Practice — slips visible live, gently.** A wrong key appears *immediately* in the rendered stream, marked with **amber colour + a wavy underline + a dot** (triple-coded so the slip reads without colour perception; never red). The caret advances past the slip; backspace removes it (universal backspace rule applies). There is **no live score, percentage, timer, or WPM** — the surface is honest about what was typed but never judgmental, and measurement (keys, observations, trend) surfaces only in the end-of-round **snapshot**. All-lowercase sentences from the curated bank. Lives in `apps/tauri/src/routes/PracticePanel.svelte`. (The slip *capture* still flows through the ambient tap regardless.) See "Insight system → Practice" for the full flow.

## Correction-engine state model

- **Confidence tiers** (engine mode — how aggressive corrections are): `Cautious` (acts on High confidence only), `Balanced` (Medium+), `Eager` (Low+). Defined in `crates/correction-engine/src/lib.rs` as `ConfidenceTier`.
- **Space-error types**: `MissingSpace`, `ExtraSpace`, `ModifierDrift`.
- **Outcome states** (per word): `CleanHit`, `SelfCorrected`, `UncorrectedMiss`, `TwoKeysTogether`. All four are intentional — do not collapse to three. Self-corrections via backspace are signal, not failure.

### Outcome resolver (5a) — verdict state machine + limitations

The resolver is an **event + idle state machine** (`resolver::decide_verdict`), not a debounce. Core rule: **never fire a verdict mid-edit.** Four definitive triggers:
- **`CorrectedToOther` / `CorrectedToSuggestion`** — fire *immediately* when a successor token seals at the original's `start` with different content (a seal is unambiguous; no wait).
- **`Kept`** — fire when the token is still intact, the caret is **not** in/at its span, and its region has been idle ≥ `KEPT_IDLE_THRESHOLD_MS` (~5s; sized for slow-typing/motor-impairment notice-pauses).
- **`Abandoned`** — fire when the token is wiped, no successor, the caret has moved ≥ `ABANDONED_CARET_MARGIN` chars from its `start`, and idle ≥ `ABANDONED_IDLE_THRESHOLD_MS` (~10s).
- otherwise stay `Pending`.

The thresholds + margin are **tunable** module constants (raise for slow typists). Idle is driven by **both** keystroke ticks and the engine's 1s watchdog tick (`tick_resolver` is called from the watchdog) — without the watchdog, an idle-due verdict would never fire when the user stops. `caret` is threaded through `OutcomeResolver::tick`. This replaced a single content-stability debounce that produced premature `Kept` (leading edge of backspace), premature `Abandoned` (mid-retype empty span), and silent non-resolution (user stops, no tick).

**Known limitations:**
- **Long in-place replacements vs `ABANDONED_CARET_MARGIN`.** Abandoned is held off while the caret stays within the margin (default 8 chars) of a wiped token's start. Retyping a replacement **longer than the margin** *without sealing it* (no boundary typed) and then pausing past the abandon idle can mis-fire `Abandoned`. Normal retypes seal (a space/punctuation) → `CorrectedToOther` fires first, so this only bites unsealed long edits. Tune the margin if real usage hits it.
- **Selection-replace is not covered.** Mouse-select-and-overwrite corrections (select a word, type over it) may not move the caret *through* the token's region the way backspace-then-retype does — and selection events may not even reach the engine via the sidecar (the L1 tap streams keystrokes, not selection/caret state). So a select-and-replace correction can still resolve `Kept` and miss the slip. **TODO:** add a selection-aware signal in a future iteration.

### Motor map (5c) — v0 design notes

The motor map (`crates/correction-engine/src/motor_map.rs`) is the engine's first per-user learner: a `HashMap<char, SlipDistribution>` keyed by *intended* character, built passively from resolved `Kept` / `CorrectedToOther` outcomes (30-day half-life decay). The map itself is **observe-and-store**: corrections never write into it, and it is never used to rewrite text directly. It reaches live suggestions in exactly one way — `MotorMap::affectedness(key)` (the decayed slip rate) eases the classifier's evidence bar (see *Live correction*), and only ever behind the user's Corrections switch.

**Persistence.** The live `~/.typeassist/motor_map.json` is flushed by `engine::flush_motor_map` on a **time cadence** (every `MOTOR_FLUSH_INTERVAL_MS`, 2s, when `MotorMap::has_unsaved()`), driven by the 1s watchdog. This is the primary durability path — the post-loop shutdown `save_to` does **not** reliably run under `tauri dev` (no `RunEvent`/exit hook in `lib.rs`, so the engine task is aborted at its await on app exit). **Dated snapshots** (`snapshots/YYYY-MM-DD.json`) are the durable history, independent of the live-file cadence, written per **Principle #6**: **daily** (the watchdog checks every ~10 min and writes when a day has elapsed) **plus one at startup before any writes** (preserving the as-loaded state). The per-day filename dedupes, so repeated launches keep the day's opening state. The Practice trend (`engine://practice-trend`) reads these dated files, so daily snapshots also give the trend daily (not weekly) granularity.

Four conscious v0 simplifications — **intended behaviour, not bugs.** Don't "fix" them without a product decision:

1. **Reads use `last_now`, not wall-clock.** `query_*` / `confidence` decay against the most recent observation time (the map's read methods take no `now`), so a query made long after the last keystroke shows slightly stale values. Fine live; revisit with a `refresh(now)` if cold queries ever matter.
2. **Transpositions log as two substitutions, not a swap.** Alignment is plain Levenshtein (no Damerau transposition op, no spatial cost), so `teh→the` records as two subs — spurious cross-pair noise that washes out at scale. Revisit with Damerau-Levenshtein only if real data shows transpositions dominate.
3. **Pruning at `1e-4` drops single-observation slips after ~390 days.** A slip seen once and never again decays below the prune floor (~13 half-lives) and is forgotten. This is the intended forgetting policy, not a leak.
4. **Outcomes are observed per resolver transition, with no retraction.** The resolver is revisable (a record can flip `Kept` → `CorrectedToOther`); the proposer retracts on flip, but the motor map does not. So a word kept-then-corrected counts its `Kept` positives *and* the correction's matches/slips — a mild upward bias on confidence for re-edited words. Acceptable while the kill-switch is off and we're only gathering data; add retraction (or count only the final outcome at anchor retirement) before flipping.

### Live correction (M3) — suggest-only bubble, off by default

The first feature that **modifies live typing**, built conservatively: **the default behaviour is to do nothing** (the master gate `correction_enabled` defaults off and is the user's to turn on — Settings ▸ Corrections or the tray's "Enable corrections"), and **nothing is ever applied silently**. Every correction is a **suggestion the user confirms** — the *bubble*, below. (History: Step 1 was a manual per-pattern allow-list that auto-applied fixes; that path was replaced at go-live by the confirm-to-accept bubble, and the per-pattern allow-list enables were retired.)

#### Classifier (`kill_switch.rs`) — suggest-only, risk-tiered; drives the live bubble

The per-pattern classifier (`classify` → `PatternReadiness`) was rebuilt 2026-06-18 as suggest-only. It now **feeds the live bubble's *learned lane*** (via `shadow_suggestion`, below) and the Progress → Impact ledger; the `KILL_SWITCH_CLASSIFY` debug line is observe-only. Its model:

- **No silent path — ever.** The Tier-1 "silent auto-fix" outcome is **retired**. Every correction is a **suggestion the user confirms** (confirm-to-accept). `PatternReadiness` has exactly two arms: `Suggest { tier }` and `Observe { reason }`. No confidence, however high, marks a pattern for silent application (Principle #9 — never a "worse autocorrect").
- **Risk-tiered evidence bar** (replaces the flat 12): a **non-word source** (`teh → the`) clears a **low bar** (`NONWORD_SOURCE_EVIDENCE_BAR`, placeholder ~4 — calibrate once the suggestion UI gives accept/undo signal); a **real-word source** (in the dictionary *or* the user's learned vocab — the **hard vocabulary rule**) must clear the **high bar** (`REALWORD_SOURCE_EVIDENCE_BAR`, 12). `is_known` already unions dictionary + learned vocab, so one call covers both.
- **Motor-Map-aware bar (2026-06-18).** The evidence bar is *eased* by the **affectedness** of the keys the slip touches — physiology substitutes for repetition: a slip on a key the motor map shows slipping often is far likelier a true motor miss than a change of mind, so it needs fewer sightings. `MotorMap::affectedness(key)` = the decayed slip rate (`1 − confidence`), **gated by `MIN_SAMPLES`** (under-sampled → 0; no easing without evidence). `build_facts` takes the **max** affectedness over `slip_class::involved_keys` (one strongly-affected key suffices). `evidence_bar(typed_is_known, affectedness)` relaxes the tier base toward a floor, **graded linearly**, saturating at `STRONG_AFFECTEDNESS` (0.20): non-word base 4 → floor `AFFECTED_BAR_FLOOR` (2); real-word base 12 → floor = the non-word base (4), so a real-word slip stays strictly more cautious even fully eased. As the hand recovers and slip rates fall, the bar tightens on its own. **Easing only lowers the repetition requirement — it never crosses a structural gate** (single-motor shape, single-letter source/target, real-word/vocab caution, never-silent all checked first); a unit test pins that max affectedness can't rescue a rejected pair. The teach-stop (3-strike undo brake) keeps a generous bar from nagging once live. `classify`/`classify_explained` take `&MotorMap`; `classify_explained` returns affectedness + bar_used + base_bar + `surfaced_early`. The shadow log marks `EARLY(affected-key bonus)` rows with `affected=.. bar=used/base`; the Impact UI (`read_word_patterns`) loads `motor_map.json` so its `ready` reflects the same eased bar.
- **Target gate:** target not a real word → always `Observe` (never steer toward a non-word); a real-but-uncommon target still earns a suggestion (commonness no longer gates now that nothing is silent).
- **Structural disqualifiers, at any count:** the strict **single-motor-error filter** (`slip_class::single_motor_edit` — one adjacent-key substitution / transposition / dropped letter / extra letter, *stricter* than `classify_slip`'s edit-distance ≤2, so it drops word-swaps like `so → for`, non-adjacent subs, and cross-token merge artifacts); **single-letter source** (`s → is`) and **single-letter target** (intent-ambiguous, never nominated — `a`/`i` are known words so they take the cautious real-word path and are never themselves a target). Plus the existing **3-strike undo brake** → `Observe { BrakeTripped }`.
- **The bubble (built; M3 go-live).** When the master gate is on, a just-sealed word that yields a suggestion shows a small bubble (`Cue.svelte`, its own non-focus window, label `cue`, ~320×112; shown by Rust without stealing focus; not anchored to the word, since per-word geometry is native-Cocoa-only). The suggestion comes from two lanes, tried in order: **learned** (`shadow_suggestion` — the user's own `word_patterns` through the risk-tiered classifier above) then **bold** (`shadow_convergence_scan` — a dictionary non-word with exactly one single-motor-edit candidate, or a frequency-dominant one; no learned pattern needed, so a fresh install can still suggest). Contraction/possessive-only fixes are dropped, and the suggested spelling follows the locale variant. **Accept = a deliberate isolated Shift tap** (L1 `ShiftTap` — reachable by either hand; an accessibility choice, Tab is harder one-handed), which injects the fix. **Undo = a bare Escape** within `UNDO_WINDOW_MS` (6 s) after an applied fix — it **reverts only** (no teach-stop; Esc no longer removes anything). An unaccepted bubble disappears on any other keystroke past the word, backspace, caret move, or after `SUGGESTION_TIMEOUT_MS` (5 s) — a missed accept is fine. **Anchored accept is OFF** (`ANCHORED_ACCEPT_ENABLED = false`): a Shift after the caret has typed on past the word did a wrong-place replace in a live field (`settgings` → `Settingsettings`), so Shift accepts **only** on the immediate caret-at-rest path (`after_len == 1`); re-enabling needs L1 to confirm the word at the position from the AX field. **Watch Dog coexistence:** `CompetitorSense` behaviourally senses a competing autocorrect and then **stands down** on host-redundant (common) fixes (`is_host_redundant`), counted as `competitor_deferred` in the funnel; idiosyncratic fixes always fire. See `docs/qa-11-host-coexistence.md`.
- **Impact/Progress UI reconciled (2026-06-18).** `read_word_patterns` (`apps/tauri/src-tauri/src/lib.rs`) now drives the Impact ledger off the risk-tiered `classify`, not the lenient `classify_slip`/flat-12 it used before. A row appears **only** if the pattern is a genuine motor slip (`Suggest`, or `Observe{InsufficientEvidence|Stale|BrakeTripped}`); structural `Observe` reasons (word-swap / single-letter / non-word target / merge / unseen) are dropped, so they no longer masquerade as slips. `ready` is the real readiness (`Suggest` cleared its tier bar); the coord/precis tag comes from `single_motor_edit` (Transposition = coord, else precis). On the real store this took the ledger from ≈457 lenient rows to ~334 genuine slips. Both `ProgressPanel.svelte` (menu-bar glance) and `Progress.svelte` (in-app) consume it; neither renders pills, so no Svelte change was needed.
- **Live shadow dry-run log (2026-06-18).** With capture running, whenever the classifier WOULD surface a suggestion for a just-sealed word, one line is appended to **`~/.typeassist/shadow_suggestions.log`** (honors `TYPEASSIST_DATA_DIR`): `<now> would-suggest  typed -> target  tier=…  evidence=<decayed weight>  edit_dist=<levenshtein>`. The log itself applies nothing and is written whether or not the master gate is on (the gate only decides whether a bubble then shows). Tail with `tail -f ~/.typeassist/shadow_suggestions.log`. Hooked at the end-of-line seal in `engine.rs` (`shadow_suggestion` / `append_shadow_log`); no new privacy surface (the same pairs already live in `word_patterns.json`).

- **State: the master gate (`allow_list.json`).** `~/.typeassist/allow_list.json` (`apps/tauri/src-tauri/src/allow_list.rs`) holds the global **master gate** (`correction_enabled`, default **off**). Its per-pattern `patterns` list is a retired field — kept for file compatibility and always empty; the manual per-pattern enables and the `Corrections…` panel (`AllowlistPanel.svelte`) no longer exist. Gate off ⇒ nothing is suggested or corrected. The **engine task is the sole writer**; the UI reads it read-only and mutates via `EngineControl` (no two-writer race — Principle #6/#8). Honors `TYPEASSIST_DATA_DIR`.
- **Where it fires:** only at the **forward end-of-line word boundary** (space / punctuation) when the sealed Word yields a suggestion from the learned or bold lane (and the gate is on). **Not on Return** (Return often commits/sends — unsafe to post-correct) and **not on mid-line/replay re-tokenisation**. Injection reuses `OutboundCommand::InjectCorrection { delete_count, replacement }` (delete the word + boundary, retype target + boundary).
- **Echo-skip is load-bearing.** The L1 tap re-captures our own injected keystrokes (`.cgSessionEventTap` sees posted events). An `eventSourceUserData` tag did **not** survive the post→tap round-trip in practice, so echo is dropped **engine-side by exact count** (`pending_echo` = `delete_count` + replacement chars) — skipped before the funnel, the pipeline, and the undo check. Never re-introduce learning/correcting from injected events. *(Follow-up: the count-based skip can drift if echo events are dropped/merged — add a robustness check.)*
- **One-key undo (revert-only).** Within `UNDO_WINDOW_MS` after an applied fix, a **bare Escape** reverts it (reverse injection) and is recorded as an `undone` outcome; any other key is an implicit accept (disarm). **There is no teach-stop** — Escape does not remove the pattern. The 3-strike undo brake (`WordPatternStore::note_undo` / `consecutive_undos` → `BrakeTripped`) still exists in the crate and the classifier honours it, but **no live code path calls `note_undo`/`note_accept` today** (only tests do), so the brake is dormant until the undo signal is wired in on purpose. Escape reaches the engine because the L1 tap special-cases **keycode 53 → U+001B** (its unicode string is otherwise empty). Escape is observed, not suppressed (it still reaches the app).
- **Surfaces:** the single menu-bar (tray) menu has an **"Enable corrections"** check (instant global off) — the **same state** as the **Corrections switch in Settings** (one source of truth, synced via `corrections://state`) — plus the bubble (`Cue.svelte`). Today also carries the passive Corrections offer. The Progress → Impact tab stays read-only.
- **Observability (Principle #7):** every fix/undo logs `CORRECTION_APPLIED` / `CORRECTION_UNDONE` and increments the funnel's `c_corrections {applied, undone}`. Bubble outcomes are also counted (shown / accepted / self-matched / undone) independently of the stores that record them and reconciled in `FUNNEL_DUMP` (an untracked store reads *untracked*, never a false 0); the daily scorecard lands in `progress_snapshots.json`, and the per-pattern tallies live in an `outcomes` block in `word_patterns.json`.
- **Caret-move safety (Fix-B) — a fix never fires against a stale line model.** The engine dead-reckons the current line from keystrokes alone, so a caret move it can't observe would let a correction delete at the wrong position (the `edndd` desync). An immediate **line reset** — the same one newline runs: clear `line_buf`/`line_dwells`/`caret`/anchors, `tokenizer.reset_line()`, emit `EVT_LINE_RESET`, **and disarm any pending Escape-undo** (its revert would inject at the stale caret) — is triggered on **exactly three discrete, content-free signals** (Principle #9 — no coordinates, no text, only a `mouse`/`focus`/`updown` log tag):
  1. **Mouse / trackpad click** — the L1 tap also watches `leftMouseDown`/`rightMouseDown`/`otherMouseDown` (a trackpad click *is* a mouse-down) → emits `InputEvent::CaretMoved`.
  2. **Focus / app change** — the secure-field `AXObserver`'s focused-element change + the `didActivateApplication` app switch → `CaretMoved`.
  3. **Up / Down / PageUp / PageDown** — engine-side, where `nav_action` would otherwise no-op into a silent desync.

  Plain **Left/Right/Home/End stay caret-only** via `nav_action` (the model caret tracks them, so no reset is needed). Bias toward resetting: an extra reset only skips the *next* word's correction (safe); a missed reset is what deletes the wrong thing. **Lesson — `kAXSelectedTextChangedNotification` is deliberately NOT a caret-move trigger.** It fires on every caret advance during **normal typing**, so it conflates typing with navigation — using it as a reset signal wiped the line after each keystroke and starved correction (the engine could never accumulate a word). It stays *subscribed* only for the secure-field re-eval (which emits nothing to stdout). **Don't re-introduce selection-changed as a reset signal — it can't be told apart from typing.**
- **macOS autocorrect coexistence (partly addressed).** The OS (and apps like Google Docs) correct common dictionary typos on the same stream — indistinguishable to the user from ours and they race our seal. **Mitigated:** the Watch Dog stand-down above defers common fixes when a competitor is sensed. **Still open:** the accept-burst weld (`Tthe` in Docs sentence-start auto-cap, QA-11/QA-14/QA-15) — the approved fix in `docs/qa-11-host-coexistence.md` is not built; until then anchored accept stays off and the engine fails safe.

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

**Releasing.** The distributed build is a Developer-ID-signed, **notarized + stapled Universal (arm64 + x86_64) DMG** — follow `docs/releasing-beta.md` and run `docs/beta-smoke-test.md` against every DMG before it ships.

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

- **Coexistence with macOS system autocorrect.** Most macOS text fields run their own autocorrect, so it operates on the same keystroke stream as TypeAssist. The two solve different problems — macOS is dictionary-based (whole known words), TypeAssist is motor/spatial (the specific slip a hand makes) — but they can collide on the same keystroke, and a system correction can easily be mistaken for one of ours. Open question: how the two should coexist. Options to weigh: detect-and-defer when a system correction is in flight; document testing with system autocorrect off; or confirm it's a non-issue in practice. Surfaced during debug-view testing when macOS corrected `cuty → city` while our engine correctly reported `LEFT ALONE`. *(Status: a behavioural detect-and-defer stand-down for common fixes is built — see Live correction → Watch Dog coexistence; the host-collision weld is still open per `docs/qa-11-host-coexistence.md`.)*

- **L2 fatigue and temporal aggregators are deferred until after L4 correction exists.** Both are **modifiers** — they adjust *how much help* the correction engine gives (e.g. lean in more as a hand or finger tires within a session; bias differently for cold-start morning vs steady afternoon). With no correction engine to modulate, there's nothing for them to act on, so building them now would be observation-without-purpose. Revisit once L4 is live — the fatigue-aware "help more when you're tiring" idea is valuable then.

- **Privacy & Terms is built; Terms is still a beta placeholder.** The `privacy` route in `apps/tauri/src/App.svelte` is live: the *What I learn* / *What I never keep* copy (kept in sync with what the app really stores — e.g. the 14-day progress note) plus a Terms of Use marked `DRAFT — beta placeholder` that still carries literal `[LEGAL ENTITY / YOUR NAME]` placeholders awaiting legal review. The earlier design-doc *Why it works this way* (philosophy) block is not on the page. Whenever the app starts storing something new, update *What I learn / What I never keep* in the same PR (mind the em-dash + curly-quote punctuation style).

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
│   └── tauri/             # L5 — Tauri + Svelte (built: onboarding, Today, Progress, Settings, menu-bar panels, bubble)
└── docs/
    ├── architecture.md
    └── contracts/
        ├── input-events.md
        └── volatility-map.md
```
