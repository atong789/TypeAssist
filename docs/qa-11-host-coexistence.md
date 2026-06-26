# QA-11 / QA-14 — Host-autocorrect coexistence (Option A)

**Status:** Spec for review — **approved, not built.** Rides with the parked
autocorrect-coexistence pass, after solo testing.
**Priority:** **Must-fix-before-beta** (QA-11 / QA-14 host-collision family).
**Owner decision (PM):** approved 2026-06-24.

---

## Problem (QA-11)

Confirmed repro in Google Docs: at a sentence start, typing `teh`, accepting
Jordan's `teh → the` fix, produces **`Tthe`** — a stray leading capital `T`.

**Root cause — content-blindness, not a log gap.** The engine dead-reckons the
line from the L1 keystroke stream alone; the L1 tap reports keystrokes, not
field contents (`EventTap.swift:165`). Google Docs' sentence-start
auto-capitalisation is an app-internal edit that emits **no keystroke**, so the
engine cannot see it. When Jordan posts its accept burst (delete N + retype
`the `), it races Docs' auto-cap edit on the same field; the two interleave with
neither aware of the other, and a stray `T` survives → `Tthe`. Reproduces at
every sentence start because Docs auto-cap has no toggle.

The collision is therefore **not visible in the engine log** — the log records
Jordan's *intent* (`CORRECTION_APPLIED typed="teh" target="the" after_len=1`),
never the field's true state. That blindness *is* the bug.

Jordan **cannot** detect "the field changed under me" mid-accept and abort: it
can't see the auto-cap edit, and even if it could, it can't atomically interpose
across the app boundary. So this needs a **different guard**, not a mid-accept
check.

---

## Decision: Option A — suppress host-redundant fixes in own-engine apps

Suppress the fixes a host app's own corrector would *also* make (the redundant,
collision-prone ones) when the focused app runs its own correction; keep
Jordan's **idiosyncratic** fixes firing everywhere (his unique value — the host
won't touch those).

Two cheaper/heavier layers are noted but **not** in this change: **B**
(sentence-start suppression) as a follow-on, **C** (AX read-back) shelved.

### A1 — "Common" is a *pair predicate*, not a lane gate ✅ (confirmed)

The engine has two suggestion lanes (`engine.rs:3913-3935`): the **learned
lane** (`shadow_suggestion` over the user's `word_patterns`, tried first) and the
**convergence lane** (`shadow_convergence_scan` over the dictionary, fallback).

**Critical finding:** the repro word `teh → the` lives in the **learned lane**
(`word_patterns.json`: `{"typed":"teh","target":"the","count":5.94}`). A
lane-only gate ("suppress convergence, keep learned") would **miss the actual
repro**. So "common" is defined as a property of the `(typed → target)` pair,
applied to whichever lane surfaced it:

```
is_host_redundant(typed, target, lexicon) :=
       !lexicon.is_known(typed)                       // typed is a non-word the host also flags
    && lexicon.frequency(target) >= COMMON_FREQ_THRESHOLD   // target is a common dict word the host converges to
```

- `Lexicon::frequency` already exists (`lexicon.rs:29`; `the` ≈ 23B).
- `COMMON_FREQ_THRESHOLD` is a **tunable const**; **calibrate during the
  coexistence test** against `words_freq.txt` so high-frequency words trip it and
  mid/long-tail words don't.

Effect: catches `teh → the` from either lane; **keeps** garbles to personal /
learned vocab (`kubernetis → kubernetes`) and to uncommon targets (below
threshold) — the fixes the host won't make. The convergence lane is fully
covered by construction (it only ever emits non-word → common-word). The learned
lane is split: learned-but-common → suppressed, learned-but-idiosyncratic →
kept.

### A2 — Browser-coarse tradeoff ✅ (consciously accepted)

The focused app's bundle ID is the gate input. **Wrinkle:** Google Docs is *not*
a bundle ID — it runs inside the browser, so the frontmost bundle is
`com.google.Chrome` / Safari / Arc, indistinguishable from a plain `<textarea>`
in the same browser without AX inspection (option C).

**Accepted tradeoff:** treat **browsers coarsely** — suppress the host-redundant
lane in any browser. A bare no-autocorrect web textarea therefore also loses
*common* fixes. Worth it: fixing the Docs corruption matters, the host-redundant
lane is the redundant one by definition, browsers increasingly run their own
spellcheck, and Jordan's idiosyncratic lane still fires everywhere.

### A2b — The static own-engine set (explicit) and the over-suppression guard

**The gate keys off "this app CAN / does-by-default autocorrect," not "is
autocorrecting right now."** A static list can only encode capability/default,
not live per-app settings (see *Known limitation* below). The set is therefore
**curated, not mechanical**, and biased to protect deliberate writing surfaces.

**Selection rule.** Include an app **only** if its own corrector is **on by
default** *and* the collision is unsignalled/unavoidable for a typical user —
i.e. everyday native text surfaces and browsers. **Exclude** pro / long-form /
dev editors: those are a deliberate tooling choice, are frequently run with
autocorrect off, and are plausible "Jordan as sole corrector" surfaces. **The
default for any app not listed is EXCLUDE (Jordan fires fully).** We fail toward
firing — Esc-undo is the universal backstop for a collision in an unlisted app;
over-suppressing a writing surface is the outcome we refuse.

**INCLUDE — suppress host-redundant lane** (own corrector on by default):

| App | Bundle ID |
|---|---|
| Notes | `com.apple.Notes` |
| Mail | `com.apple.mail` |
| Messages | `com.apple.MobileSMS` |
| TextEdit | `com.apple.TextEdit` |
| Pages | `com.apple.iWork.Pages` |
| Keynote | `com.apple.iWork.Keynote` |
| Numbers | `com.apple.iWork.Numbers` |
| Freeform | `com.apple.freeform` |
| Safari | `com.apple.Safari` |
| Chrome | `com.google.Chrome` |
| Edge | `com.microsoft.edgemac` |
| Brave | `com.brave.Browser` |
| Arc | `company.thebrowser.Browser` |
| Firefox | `org.mozilla.firefox` |

(Bundle IDs to be re-verified at implementation time.)

**EXCLUDE — Jordan fires the full lane (illustrative; exclude is the default):**

- **Scrivener** (`com.literatureandlatte.scrivener3`) — **explicitly out** per
  PM: primary writing surface, autocorrect **off**, Jordan is the sole
  corrector. Must never be over-suppressed.
- Ulysses, iA Writer, BBEdit, Obsidian, VS Code (`com.microsoft.VSCode`),
  Sublime, JetBrains IDEs, Terminal / iTerm, and any app not in INCLUDE.

**Known limitation — detect vs. infer (revisit if it bites).** A static list
infers "this app autocorrects" from defaults; it cannot read an app's *live*
setting. Two failure directions, both accepted for now:
- **Over-suppression:** an INCLUDE app with its corrector turned *off* loses
  Jordan's common fixes unnecessarily. (Scrivener is kept out precisely to avoid
  this on the PM's surface; the risk remains for INCLUDE apps a user has
  reconfigured.)
- **Under-suppression:** a colliding app *not* on the list still collides;
  Esc-undo is the backstop.

Revisit with option C (AX read-back of the live field/app state) only if these
bite in real use *and* the privacy cost is settled.

### A3 — Plumbing (in-memory, unlogged — Principle #8) ✅ (confirmed)

- **L1 (Swift):** on `didActivateApplication` (already observed by
  `SecureFieldMonitor.swift`; bundle read already done at
  `Accessibility.swift:128`), emit a new `InputEvent::FocusedApp { bundle_id }`
  (new variant in `crates/behavioural-model/src/events.rs`).
- **Engine:** hold `current_focus_app: Option<String>` — a **single variable,
  overwritten** on each app switch. Compared against the static set; **never
  written to any store, never logged as a string.**

**Principle #8 confirmation.** The bundle ID is an ephemeral, in-memory read for
a single decision and is discarded — identical to the M3 mode-selection
precedent CLAUDE.md already blesses. It touches no `~/.typeassist` file and
appears in **no** log line. Precedent to hold: `LINE_RESET trigger=focus` logs
only the tag `focus`, never the app — the new observability field (A5) logs a
**boolean, not the bundle ID**.

### A4 — Hook point

In the fire-decision block (`engine.rs:3942`), after `suggestion` resolves and
before `if allow_list.correction_enabled`:

```rust
let host_suppressed = current_focus_app.as_deref().is_some_and(is_own_engine_app)
    && suggestion.as_ref().is_some_and(|(t, g)| is_host_redundant(t, g, lexicon));
let suggestion = if host_suppressed { None } else { suggestion };
```

### A5 — Observability (Principle #7 — a counted drop, never silent)

- Add `host_suppressed=<bool>` to the existing `FIRE_DECISION` log line
  (**boolean only — no app string**).
- Add a funnel counter `c_corrections.host_suppressed`, surfaced in
  `FUNNEL_DUMP`. A suppressed common fix is a deliberate, visible drop.

---

## B — Follow-on layer (note only; not in this change)

**Sentence-start suppression.** The engine line model already knows caret
position; `caret == line start` (or preceded by `. `) ⇒ host auto-cap will fire.
This is the safety net for the residual A can't reach: a **kept**
(idiosyncratic) fix at a sentence start in an own-engine app, where A
deliberately lets Jordan fire but auto-cap can still collide. Cheap, no new
capability. Sequence A first, add B if residual collisions appear in testing.

## C — Shelved (AX read-back)

The only guard that truly detects "the field changed under me" (read the focused
element's value, verify the word == typed, abort/adapt on mismatch). Revisit
**only if** we decide Jordan must run his kept lane inside own-engine apps **and**
the privacy cost of reading focused-field *content* (heavier than a bundle ID,
even if ephemeral) is settled.

---

## Confirmed decisions (2026-06-24)

1. "Common" = **pair predicate** (A1), not a lane gate — because `teh → the`
   lives in the learned lane and a lane gate would miss the repro.
2. `COMMON_FREQ_THRESHOLD` stays **tunable**; calibrate during the coexistence
   test.
3. **Browser-coarse** accepted (A2): suppress host-redundant lane in any
   browser; a bare textarea also loses common fixes — accepted.
4. Static set is **curated**: native everyday surfaces + browsers IN;
   pro/long-form/dev editors OUT; **Scrivener explicitly OUT**; unlisted apps
   default to EXCLUDE (fire). Detect-vs-infer over/under-suppression noted as a
   known tradeoff to revisit.
5. Bundle-ID read stays **in-memory, single var, unlogged** (Principle #8).
6. **Hold the build** until after solo testing; rides the parked coexistence
   pass. **Must-fix-before-beta.**
