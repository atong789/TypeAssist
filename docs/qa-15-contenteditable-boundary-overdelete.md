# QA-15 — Contenteditable boundary over-delete (the `andperseverance` weld)

**Status:** `+ 1` over-delete confirmed in the log; **root-cause REFRAMED
2026-06-24** after two more cases — see *Step (c)*. The original "no trailing
space" explanation is **demoted** (see correction in Step (a)). **Lead
hypothesis:** Docs drops spaces intermittently in L1 capture, so the engine's
line model drifts from Docs' real buffer and the delete lands misaligned. **No
fix re-spec** — PM will decide scope after more real-use testing.
**Priority:** Must-fix-before-beta (correction garbles user text in Google Docs).
**Owner decision (PM):** logged 2026-06-24 as its own root cause, **separate
from the QA-12 buffer-counting family.** Distinct from QA-11/QA-14, but Case 1
(`Pperseverance`, line-start doubled capital) **also routes to QA-14** when Docs
auto-cap is layered on the same drift.

---

## Symptom (confirmed repro, no competing corrector)

In Google Docs, typing `and perseverence` (misspelled), then accepting Jordan's
`perseverence → perseverance` suggestion, produces **`andperseverance`** — the
**space before** the word is eaten, welding it to `and`. Docs did **not** flag
or autocorrect the word, so unlike QA-11/QA-14 there is **no second corrector**
involved. This is purely our injection.

---

## Why this is NOT QA-12 and NOT QA-14

- **Not QA-14 (host auto-cap collision).** No competing corrector fired; the
  mechanism is boundary deletion, not casing.
- **Not QA-12 (line-model / caret miscount).** The over-deleted count is **not**
  derived from the dead-reckoned caret. The immediate-accept delete count is

  ```rust
  // engine.rs — InputEvent::ShiftTap, immediate branch
  let delete_count = (ps.word_len + 1) as u32;          // word + ONE boundary char
  let replacement  = format!("{}{}", ps.target, ps.boundary);
  ```

  `ps.word_len` comes straight from the tokenizer (`tok.end - tok.start`), **not**
  from the caret model, so it is structurally immune to contenteditable caret
  drift. Whatever QA-12 hardened about caret/buffer counting **never touched this
  code path.** The vulnerable term is the hard-coded **`+ 1`**.

## Root cause — FIRST hypothesis (DEMOTED 2026-06-24, kept for the record)

> ⚠️ **This "no trailing space" explanation is no longer the lead theory.** It is
> contradicted by the trailing-space-always-exists argument and the 1.8 s gap (see
> Step (a) correction and Step (c)). Kept here only so the reasoning trail is
> intact; the current lead hypothesis is in **Step (c)**.

The first read was that `delete_count = word_len + 1` assumes **exactly one
physically-deletable boundary char sits immediately left of the caret**, and that
in Docs the *trailing* boundary is absent, so the `+ 1` eats the **leading** space:

- **Native field:** `…and·perseverence·|` (`·` = space, `|` = caret). 13 backspaces
  delete `perseverence·`; retype `perseverance·` → `…and·perseverance·`. Correct.
- **Docs (this theory):** `…and·perseverence|` (no trailing space). 13 backspaces
  delete `·perseverence` → `…and`; retype → `…andperseverance·`.

**Why it's demoted:** the user types the space that *seals the word and fires
Jordan*, so a trailing space provably exists in the input stream; and ~1.8 s
elapsed between FIRE and accept, far too long for Docs to not have committed it.
The `+ 1` over-delete is still **real and measured** — but "trailing space absent"
is not a sound explanation for *why* it lands wrong.

---

## Step (a) — observability added + CONFIRMED LIVE (2026-06-24)

Until now `CORRECTION_APPLIED` logged only `typed` / `target` / `after_len`, so
the over-delete had to be **inferred** by hand (Principle #7 gap for exactly this
failure class). The line now carries the **actual emitted delete count** and the
**boundary char**. Fresh Google Docs repro (`and preseverance`, Shift-accept,
`andperseverance` on screen) produced, identically 3×:

```
FIRE_TIMING        typed="preseverance" target="perseverance" word_len=12 ...
CORRECTION_APPLIED typed="preseverance" target="perseverance" delete_count=13 boundary=' ' after_len=1
```

`word_len=12`, `delete_count=13` (= `word_len + 1`), `boundary=' '`, `after_len=1`.

**What is confirmed:** the `+ 1` is **measured, not inferred** — 13 backspaces were
**sent** against a 12-char word, immediate path, with the model believing a single
trailing boundary.

**Correction (2026-06-24) — earlier over-claim walked back.** A prior version of
this section said the 13th backspace "consumed the **leading** space because no
trailing space was present at the caret" and called the root cause "confirmed."
**That over-claimed.** The log records our injection **intent** (the count we
*sent*), **never Docs' actual buffer or the physical deletion** — we are
content-blind at the moment of deletion (the QA-11 wall). So the log proves the
`+ 1` was *sent*; it does **not** prove *what Docs deleted*, and "no trailing
space" is the wrong explanation (see the demoted hypothesis above and Step (c)).

---

## Step (b) — fix draft (2026-06-24)

> ⚠️ **Predates the Step (c) reframe.** This draft reasons from the now-demoted
> "trailing space absent" theory (parts (i)/(ii), the step-over analysis). Its
> *durable* conclusions still hold — the **content-blind wall**, and that
> **host-scoped suppression** is the only principle-safe mitigation while the
> mechanism is unproven — but the specific "why the trailing space is absent"
> framing is superseded by *Step (c)* (space-drop drift). **No fix re-spec until
> PM decides scope after more real-use testing.**

### The wall: "verify the boundary is a deletable char" is impossible while content-blind

The intended direction — *check the boundary really is a deletable char before
consuming the `+ 1`* — cannot be done by inspection. The L1 tap streams
**keystrokes, not field contents** (`EventTap.swift`), and reading the document
to confirm a trailing space is forbidden (Principle #8/#9 — same wall QA-11 hit).
So the engine can only ever know its **dead-reckoned model** of the line, and
QA-15 is precisely the case where that model is **wrong and unverifiable**: the
model believes one trailing boundary char exists (the space the user typed to
seal the word, `after_len == 1`), but Google Docs' contenteditable does not
present it as a single backspaceable char at the caret.

### Why the obvious "don't delete the boundary" rewrite does NOT fix it

Tempting fix: stop deleting/retyping the boundary — step *over* it instead.
That is the anchored primitive with `after_len = 1`: `Left ×1`, `Backspace ×word_len`,
type `target`, `Right ×1` (boundary never touched). But it **relies on the same
false belief** — that one real char sits to the right of the word end to move
`Left` over. In Docs (no trailing char) `Left ×1` moves the caret *into* the word,
and `Backspace ×word_len` then deletes the wrong span. It does not remove the
dependency on the model being right; it just **relocates the corruption** from the
leading space to the middle of the word. Rejected.

**Conclusion:** neither delete-through nor step-over is safe when the model's
trailing-boundary belief is wrong, and we cannot verify the belief. No
injection-arithmetic fix is robust here.

### Recommended fix — host-scoped suppression (converges with QA-11 Option A)

When the focused app is a **known own-model / contenteditable host** (Google Docs,
and other web editors that maintain their own cursor off-DOM), our dead-reckoned
line model is **structurally untrustworthy**, so **do not inject** — suppress the
accept rather than risk garbling text. Mechanically this reuses the QA-11 Option A
machinery: the ephemeral focused-bundle-id read (M3, in-memory, never persisted —
Principle #8) gates whether a fix fires.

**Important escalation for PM.** QA-11/QA-14 scoped Docs suppression to
**host-redundant** fixes (the ones Docs' own corrector would also make). QA-15 is
worse: `preseverance → perseverance` is **our idiosyncratic** fix, not one Docs
makes, and it *still* garbled. So QA-15 argues the injection **mechanism itself**
is unreliable in Docs — suppression there may need to be **total** (all accepts),
not just host-redundant. **This is a product decision:** QA-15 + QA-11/QA-14 now
point at the same mitigation (suppress in Docs) for **different** root causes, and
QA-15 widens the scope from "redundant fixes" to "possibly all fixes in Docs."

### One evidence gap before any narrower fix

A Docs-specific injection fix (rather than blanket suppression) would only be
possible if we knew **why** the trailing space is absent at the caret:

- **(i) boundary genuinely absent** (Docs defers/trims a lone trailing space) — the
  true `after_len` is 0, so a corrected count would be `word_len` (no `+ 1`); or
- **(ii) boundary present but Docs collapses backspaces 2-for-1** near a word edge
  — a count fix can't help; the primitive is wrong.

The current log cannot disambiguate (i) from (ii) (it shows our *intent*, never the
field's true post-accept state — the same blindness as QA-11). Until we can tell
them apart, **host suppression is the only principle-safe fix.** A smarter,
Docs-aware injection (e.g. delegating word-boundary deletion to the host via
Option+Backspace, letting Docs decide the unit) is a **later research item**, not
this fix.

### Scope guard

Do **not** change the `+ 1` for native fields — it is correct there (the `+ 1`
lands on the real trailing boundary; verified by every non-Docs accept in the
log). The fix is host-scoped, not a change to the injection arithmetic.

---

## Step (c) — LEAD HYPOTHESIS: Docs drops spaces in capture (2026-06-24)

Two more cases broke **both** earlier theories and reframed the root.

### The two cases

- **Case 1 — `Pperseverance`** (doubled capital `P`, at a **line start**). Looks
  like the QA-14 auto-cap collision.
- **Case 2 — `andcontinue`** (two words welded, **mid-sentence**, lowercase, no
  auto-cap): typed `... and continue` (with a `continnue` slip), accepted the fix,
  the space before the word vanished.

One theory can't cover both: "sentence-start auto-cap" doesn't fit Case 2
(mid-sentence), and "no trailing space" can't be right at all — the space is what
*seals the word and fires Jordan*, so a trailing space always exists in the input.

### The Case 2 trace (engine log)

```
FIRE_DECISION      word="continnue" suggestion=Some("continnue->continue")
FIRE_TIMING        typed="continnue" target="continue" word_len=9 ...
CORRECTION_APPLIED typed="continnue" target="continue" delete_count=10 boundary=' ' after_len=1
```

Structurally **identical** to the perseverance welds: `delete_count = word_len + 1`
(10 = 9 + 1), immediate path, model looked clean (the token sealed as `continnue`
alone, so a leading boundary *was* seen).

### The load-bearing find — merged tokens

Words sealed in the same session, just before the accept:

```
resevrance          ← "reseverance" alone (leading space captured)
andresevenance      ← "and reseverance" welded into ONE token
andreseverance      ← again
a
continnue
```

`andreseverance` / `andresevenance` are the smoking gun: the user typed
**"and reseverance"**, the **space after "and" never reached the engine**, so the
tokenizer welded two words into one token. Earlier in the same session "and" and
"resevrance" sealed *separately* — so the space loss is **intermittent**.

**This proves boundary/space chars are not 1:1 between Docs and our L1 capture.**

### Lead hypothesis

**Docs drops spaces intermittently in L1 capture**, so the engine's dead-reckoned
line model **drifts from Docs' real buffer**, and the delete burst lands
**misaligned**. The drift surfaces as:

- a **weld** (Case 2) when a boundary count is off and the delete eats the wrong
  space; and
- a **doubled capital** (Case 1) when the same drift coincides with **Docs
  auto-cap at a line start** — which **also routes to QA-14**.

So: **likely one upstream root** (unreliable Docs boundary capture) with **two
faces**; the faces' off-by-one points in *different directions* (weld = delete
shifted left; doubled-cap = a left char survives), which is why a single uniform
`+ 1` error can't explain both — but a single *capture-drift* root can.

### Hard limit (why this still isn't "proven")

The log records our injection **intent** (the count we *sent*), **never Docs'
actual buffer or the physical deletion** — we are **content-blind at the moment of
deletion** (QA-11 wall). There is also **no per-keystroke logging** in this build
(only sealed-word / replay / resolve / reset events). So the log can prove the
**drift exists** (merged tokens) but **cannot prove it caused any specific weld.**

### Parked — evidence options (PM to decide scope after more real-use testing)

Neither is a fix; both are diagnostics to settle the mechanism:

1. **L1 space-drop logging** — count space keydowns in the tap vs the model's
   boundary count, to measure the drop rate live and correlate drops with welds.
2. **Debug-only Docs readback** — a diagnostic build that reads Docs' buffer around
   an accept to see the physical deletion. Never shipped (Principle #8); diagnostic
   use only.

**No fix re-spec.** PM (Alice) will decide scope (and whether to pursue 1/2) after
more real-use testing.

---

## Step (d) — Case 3: quoted + capitalised, post-QA-17 (2026-07-04)

**First case with surrounding punctuation, and the first since the QA-17 (item 5)
trailing-quote accept path landed.** In Google Docs, typing `"Appplication"`
(leading capital, wrapped in double quotes) and Shift-accepting the
`Appplication → Application` fix produced **`"Aapplication"`** — the capital
stranded at the boundary, the same **doubled-capital face** as Case 1
(`Pperseverance`). **Notes was clean on the identical word.**

**Three hypotheses tested (the PM's ask), and where they land:**

- **Casing bug in our code? NO.** `match_source_case("Appplication","application")`
  → `"Application"` (leading-cap branch; unit-tested), and Notes applied it
  cleanly. Our computed replacement is correct. The "doubled capital" is the
  **symptom** of a misaligned delete (a left char surviving), per Step (c) — not a
  casing-computation error. Where Docs auto-cap layers on top, it also routes to
  QA-14, exactly as Case 1.
- **New QA-17 trailing-quote path? CONTRIBUTORY, not causal.** The trailing-quote
  geometry is provably correct (Notes clean on the same quoted word; the
  `pending_survives` / `immediate_accept_injection` math sends
  `delete_count = word_len + outer_trail_len + 1`). But it **lengthens the delete
  window** (the `"` adds one) and introduces a **leading `"` left-boundary char** —
  more surface for the Step (c) capture-drift to misalign around, which is plausibly
  why the surviving-left-char face stranded the capital right after the quote.
- **Known Docs weld mechanism? YES — this is the root.** Same Step (c) capture-drift
  family: our dead-reckoned line model drifts from Docs' contenteditable buffer, so
  the delete burst lands misaligned. Consistent with Notes-clean / Docs-broken and
  with the doubled-capital face.

**Confirmation still needs the terminal line.** `CORRECTION_APPLIED` is
`tracing::info` (stdout, not persisted — Principle #8), so the accept's
`delete_count` / `after_len` can't be pulled after the fact. To pin the off-by-N,
capture from the dev terminal during a Docs repro:

```
CORRECTION_APPLIED typed="Appplication" target="Application" delete_count=14 boundary=' ' after_len=2
```

`delete_count=14` (= `word_len 12` + `outer_trail_len 1` + `1`) and `after_len=2`
(the trailing `"` + boundary) would confirm our side sent the correct count and
Docs mis-applied it — same content-blind wall as Steps (a)/(c).

**Not a commit blocker** (Docs is documented best-effort; the QA-17 fixes are
correct in native fields, verified in Notes). Logged for the record; folds into
the same host-scoped-suppression decision parked in Step (b)/(c).
