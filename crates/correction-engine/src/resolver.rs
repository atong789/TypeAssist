//! Outcome resolver — Component 5a of the L4 Observing brief.
//!
//! Watches the live [`crate::AnchorTracker`] state for each Pending
//! [`crate::log::LogRecord`] and resolves the outcome (`Kept`,
//! `CorrectedToSuggestion`, `CorrectedToOther`, `Abandoned`). Still
//! **observe-only** — the resolver writes back through
//! [`crate::log::DecisionLedger::resolve_outcome`] and surfaces the
//! transitions on the panel; it does NOT learn, persist, or inject.
//!
//! ## Re-association by position, not by identity
//!
//! When the user corrects a word, the original record's [`SpanAnchor`]
//! may be left holding the wrong content — either truncated (partial
//! backspace + retype past the old end goes through the `p >= end` /
//! "after-ignore" branch in [`crate::AnchorTracker::apply_insert`], so
//! the original anchor doesn't grow back) or `Void(_)` (full backspace
//! takes the anchor through `end <= start`). The replacement word
//! seals as a **separate, new** anchor at the same position. C5a
//! resolves this by reading "what word now starts where the original
//! word started" — re-association by *position*, not by anchor
//! identity. The successor lookup is the read-back the brief asked us
//! to land cleanly.
//!
//! The reference position is `original_anchor.start`. For `Tracking`
//! anchors this stays live with shifts; for `Void` anchors C2 freezes
//! it at the moment of void — which is exactly where any *immediate*
//! retype lands (the dominant correction shape). C2 is the single
//! source of position truth; the resolver carries no shadow math.
//!
//! **Known C5a limitation.** If the user backspaces a word, then
//! navigates AWAY and edits elsewhere, then comes back and retypes at
//! the *current* position of the deleted word, the original record's
//! frozen `anchor.start` no longer points where the new word landed —
//! the resolver will see no successor and resolve as `Abandoned`. The
//! fix would be a `live_position` field on [`SpanAnchor`] that always
//! tracks shifts independently of resize/void; C5a doesn't pay for
//! that complexity. Revisit if real usage shows the workflow.
//!
//! ## Verdict state machine — event + idle, never mid-edit
//!
//! Each Pending record waits for one of **four definitive triggers** in
//! [`decide_verdict`]. The load-bearing principle: **never fire a verdict
//! mid-edit.** If chars are landing in or near a token's region, the user
//! is correcting — hold `Pending` until they seal a successor or go idle.
//!
//! | trigger | condition | when it fires |
//! |---|---|---|
//! | `CorrectedToOther` / `CorrectedToSuggestion` | a successor token sealed at X's `start` with content ≠ `original_text` (≠/= `top_candidate`) | **immediately** — a seal is unambiguous, no idle wait |
//! | `Kept` | X still `Tracking`, slice == `original_text`, caret not in/at X's span, region idle ≥ [`KEPT_IDLE_THRESHOLD_MS`] | on the tick (keystroke **or watchdog**) that crosses the idle bar |
//! | `Abandoned` | X `Void`, no successor, caret moved away from X's `start`, region idle ≥ [`ABANDONED_IDLE_THRESHOLD_MS`] | on the tick that crosses the idle bar |
//! | (none) | a successor hasn't sealed, the caret is working in X's region, or not enough idle has passed | stays `Pending` |
//!
//! This replaced a single content-stability *debounce* that conflated
//! all four outcomes and, sampled only on keystrokes, produced three
//! failure modes: a premature `Kept` at the leading edge of a backspace,
//! a premature `Abandoned` mid-retype (empty span read as abandoned), and
//! silent non-resolution when the user stopped (no tick to fire on).
//! The split — **event-driven** corrections (fire on the seal) vs.
//! **idle-driven** Kept/Abandoned (fire on a real pause, via a watchdog
//! tick that runs even with no keystrokes) — addresses all three by
//! construction.
//!
//! Idle is measured as `now − stable_since_ms`, where `stable_since_ms`
//! resets whenever the per-record observation tuple
//! `(state, post_edit, caret_at_trailing_edge)` changes — i.e. on any
//! edit that touches X's content, state, or trailing-edge caret.
//!
//! Thresholds ([`KEPT_IDLE_THRESHOLD_MS`], [`ABANDONED_IDLE_THRESHOLD_MS`],
//! [`ABANDONED_CARET_MARGIN`]) are **tunable** module constants — slow
//! typists (e.g. those with limited fine motor control) take longer pauses and
//! may need them raised.
//!
//! `CorrectedToSuggestion` is **independent of the decision arm** — the
//! candidate is stored on every loggable record regardless of whether the
//! mode acted on it (the `bullon → bullion` case under Cautious resolves
//! to `CorrectedToSuggestion` because "bullion" was in the score report).
//! Match is **case-sensitive exact**; case normalisation is upstream.
//!
//! ## Revisability + GC
//!
//! A resolved record re-resolves if a later trigger maps to a different
//! outcome (Kept → CorrectedToOther when the user comes back and corrects);
//! the resolver compares the computed outcome against the record's CURRENT
//! ledger outcome and only emits on a difference. Once the original anchor
//! is retired (line reset → `anchors.clear()`), the record's resolver
//! entry is GC'd on the next tick and the last resolution stands.

use std::collections::HashMap;

use crate::anchor::{AnchorState, SpanAnchor};
use crate::log::Outcome;

/// Idle period (ms) of no edits in a token's region before an untouched,
/// still-`Tracking` token resolves `Kept`. **Tunable.** Set to 5s to cover
/// the motor-impaired typist's "I see the typo, let me think, OK I'll fix it"
/// window — it must comfortably exceed a user's typical notice-pause so a
/// deliberate correction-after-a-beat isn't locked as `Kept` first (which
/// then revises to a correction and double-counts in the 5c motor map).
/// Raising it reduces, but does not eliminate, that window.
pub const KEPT_IDLE_THRESHOLD_MS: u64 = 5_000;

/// Idle period (ms) after a token is wiped — caret moved away, no
/// replacement sealed — before it resolves `Abandoned`. **Starting point —
/// tunable.** Long, because a slow in-place retype must never be mistaken
/// for walking away.
pub const ABANDONED_IDLE_THRESHOLD_MS: u64 = 10_000;

/// How close (in chars) the caret must stay to a wiped token's frozen
/// `start` to count as "still working here" — holds off `Abandoned` while
/// a replacement is being retyped in place. **Tunable.**
pub const ABANDONED_CARET_MARGIN: usize = 8;

/// Resolver version. Bump on any change to the resolution policy that
/// downstream Components or the debug panel could observe.
///
/// v4 — verdict-emission rebuilt from a single content-stability debounce
/// into an event + idle state machine (see module docs). Corrections fire
/// on the successor seal; Kept/Abandoned fire on a real idle period via a
/// watchdog-driven tick; nothing fires mid-edit.
pub const RESOLVER_VERSION: u32 = 4;

/// Output of [`compute_post_edit_text`]. Captures whether the user has
/// reached a state the resolver can classify, or is **mid-edit** —
/// specifically the truncated-Tracking case where the anchor's content
/// is a proper prefix of `original_text` and no successor has sealed
/// yet (a transposition fix like `wordl → world` walks through this
/// state: A shrinks to `"wor"` while the user types the replacement
/// `"ld"` that the seal will lift into a successor anchor).
///
/// Mid-edit observations are still cached so subsequent edits within
/// the truncation keep resetting the debounce — but the resolver does
/// NOT emit a transition on a mid-edit observation. The record stays
/// at its previous outcome until either:
///   * the user commits (a successor seals → `Resolved`), or
///   * the original's content matches `original_text` again (`Resolved` → Kept).
///
/// Without this gate, a pause inside a correction (e.g. user
/// backspaces past mid-word and looks at the screen for > debounce)
/// triggers a premature CorrectedToOther on the truncated prefix, and
/// the subsequent successor seal doesn't reliably re-tick because
/// ticks are keystroke-gated. See the module doc-comment for the
/// full trace.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PostEdit {
    /// User has reached a committable state — classify it.
    Resolved(Vec<char>),
    /// Mid-edit: anchor is Tracking with content that is a proper
    /// prefix of `original_text` AND no successor anchor exists at the
    /// same start. Content is captured so two different truncations
    /// (e.g. `"wor"` vs `"wo"`) compare as different observations and
    /// reset the debounce timer.
    Incomplete(Vec<char>),
}

/// What the resolver remembers about an anchor at last observation.
/// Keyed on `(state, post_edit, caret_at_trailing_edge)`. A token's
/// absolute *position* is still NOT in the tuple — a pure shift
/// (insert/delete elsewhere on the line) doesn't reset the debounce —
/// but whether the caret sits at the token's right edge IS, because that
/// edge flipping is the leading signal of a seal-then-correct gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    state: AnchorState,
    post_edit: PostEdit,
    /// `caret == anchor.end` — the caret is parked at this token's
    /// trailing boundary, so the next backspace eats into it. Deleting
    /// the separator after a just-sealed word moves the caret here, which
    /// changes the observation and resets the debounce — preventing the
    /// resolver from locking a premature `Kept` at the instant the user
    /// starts backspacing to correct. See the "Outcome resolver (5a) —
    /// known limitations" note in CLAUDE.md.
    caret_at_trailing_edge: bool,
}

#[derive(Debug, Clone)]
struct Observed {
    obs: Observation,
    stable_since_ms: u64,
}

/// Per-anchor outcome resolver. Owns the per-anchor observation cache
/// (last-seen tuple + when it was first observed) used to measure region
/// idle time. Stateless across runs — lives in process memory only, like
/// the [`DecisionLedger`] it resolves into.
#[derive(Debug)]
pub struct OutcomeResolver {
    last_seen: HashMap<u32, Observed>,
    kept_idle_ms: u64,
    abandoned_idle_ms: u64,
}

impl Default for OutcomeResolver {
    fn default() -> Self {
        Self::with_thresholds(KEPT_IDLE_THRESHOLD_MS, ABANDONED_IDLE_THRESHOLD_MS)
    }
}

impl OutcomeResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct with explicit idle thresholds. Production uses
    /// [`Self::new`] (the module-constant defaults); tests and future
    /// per-user tuning pass small/custom values.
    pub fn with_thresholds(kept_idle_ms: u64, abandoned_idle_ms: u64) -> Self {
        Self {
            last_seen: HashMap::new(),
            kept_idle_ms,
            abandoned_idle_ms,
        }
    }

    pub fn kept_idle_ms(&self) -> u64 {
        self.kept_idle_ms
    }

    pub fn abandoned_idle_ms(&self) -> u64 {
        self.abandoned_idle_ms
    }

    /// Test-only inspector: time at which the current observation for
    /// `anchor_id` was first seen (`None` if the resolver hasn't
    /// observed that anchor since the last GC).
    #[cfg(test)]
    pub fn stable_since_ms(&self, anchor_id: u32) -> Option<u64> {
        self.last_seen.get(&anchor_id).map(|o| o.stable_since_ms)
    }

    /// Single resolution pass. Call after every anchor-affecting edit
    /// (insert, delete, line reset replay). Returns the list of
    /// `(record_id, new_outcome)` transitions to apply to the ledger —
    /// the caller is responsible for `ledger.resolve_outcome(...)` and
    /// for surfacing the updates on whatever side-channel the host
    /// uses (Tauri event, in this codebase).
    ///
    /// The reason for not mutating the ledger here is so that callers
    /// can decide whether to short-circuit on identical outcomes, batch
    /// emissions, etc. — the resolver itself is a pure observer.
    ///
    /// **Garbage collection.** Anchors that disappeared from the
    /// tracker (line reset) have their cache entries removed at the
    /// start of each tick. Records whose anchor is gone are left as
    /// they are; their last resolution stands per the C5a brief.
    ///
    /// `caret` is the host's current caret index in `line_buf`. It feeds
    /// both the `caret_at_trailing_edge` observation (idle-clock reset) and
    /// [`decide_verdict`]'s caret-region gating that holds `Kept` /
    /// `Abandoned` while the user is working at the token.
    ///
    /// **Must be called on idle too, not only on keystrokes.** `Kept` and
    /// `Abandoned` fire on elapsed idle, so the host drives this from its
    /// periodic watchdog as well as from edit events — otherwise a verdict
    /// that comes due while the user is paused would never fire.
    pub fn tick<'a, R: ResolvableRecord + 'a>(
        &mut self,
        now_ms: u64,
        anchors: &[SpanAnchor],
        line_buf: &[char],
        caret: usize,
        records: impl IntoIterator<Item = &'a R>,
    ) -> Vec<(u64, Outcome)> {
        // GC: drop cache entries for anchors that are no longer tracked.
        // Cheap because the typical line carries a handful of anchors.
        let live: std::collections::HashSet<u32> = anchors.iter().map(|a| a.id).collect();
        self.last_seen.retain(|id, _| live.contains(id));

        // Walk the records, compute per-record observation, refresh the
        // cache, and queue transitions for any record whose stable
        // observation maps to a different outcome than what it currently
        // holds. Generic over [`ResolvableRecord`] so the same verdict
        // machine drives BOTH the C5b decision ledger (unknown words, for
        // the lexicon proposer) and the C5c motor ledger (every word).
        let mut changes = Vec::new();
        for record in records {
            let Some(original) = anchors.iter().find(|a| a.id == record.anchor_id()) else {
                continue;
            };

            let post_edit =
                compute_post_edit_text(record.original_text(), original, anchors, line_buf);
            let obs = Observation {
                state: original.state.clone(),
                post_edit,
                caret_at_trailing_edge: caret == original.end,
            };

            match self.last_seen.get_mut(&original.id) {
                Some(stored) if stored.obs == obs => {
                    // Stable — do not reset the timer.
                }
                Some(stored) => {
                    stored.obs = obs.clone();
                    stored.stable_since_ms = now_ms;
                }
                None => {
                    self.last_seen.insert(
                        original.id,
                        Observed {
                            obs: obs.clone(),
                            stable_since_ms: now_ms,
                        },
                    );
                }
            }

            // Region idle = time since the observation tuple last changed.
            // Saturating to guard against a non-monotonic `now_ms` (test
            // fixtures, time skew) — a negative delta would wrap huge.
            let stable_since = self.last_seen[&original.id].stable_since_ms;
            let idle_ms = now_ms.saturating_sub(stable_since);

            // The verdict state machine. Returns None to stay Pending
            // (mid-edit, or not enough idle) — never fire mid-edit.
            let Some(computed) = decide_verdict(
                record.original_text(),
                record.top_candidate(),
                original,
                anchors,
                line_buf,
                caret,
                idle_ms,
                self.kept_idle_ms,
                self.abandoned_idle_ms,
            ) else {
                continue;
            };
            if record.current_outcome() != computed {
                changes.push((record.record_id(), computed));
            }
        }
        changes
    }
}

/// A record the [`OutcomeResolver`] can resolve a verdict for. Implemented
/// by both the C5b [`crate::log::LogRecord`] (decision ledger — unknown
/// words, carries a candidate) and the C5c [`crate::motor_ledger::MotorRecord`]
/// (motor ledger — every word, candidate-agnostic). The resolver only needs
/// these five projections; everything else about a record is the consumer's
/// business.
pub trait ResolvableRecord {
    fn record_id(&self) -> u64;
    fn anchor_id(&self) -> u32;
    fn original_text(&self) -> &str;
    /// The engine's top candidate, if any. `Some` only for the decision
    /// ledger (drives `CorrectedToSuggestion`); the motor ledger returns
    /// `None` so every correction reads as a `CorrectedToOther` slip.
    fn top_candidate(&self) -> Option<&str>;
    fn current_outcome(&self) -> Outcome;
}

/// Compute the post-edit observation for this record.
///
/// Algorithm:
///   1. If the original anchor is `Tracking` AND its content slice
///      equals `record.original_text` → `Resolved(content)` (Kept happy
///      path).
///   2. Otherwise look for a **successor** — a `Tracking` anchor whose
///      `start` equals the original's `start`, with the largest `end`
///      (most recent / longest seal at this position), excluding the
///      original itself by id. If found, `Resolved(successor)`.
///   3. Otherwise, if the original is still `Tracking` and its content
///      is a *proper prefix* of `original_text` (`len < original` AND
///      the content matches the original's leading chars), the user is
///      mid-edit through inside-deletes and hasn't committed yet →
///      `Incomplete(content)`. Resolver skips classification.
///   4. Otherwise (Tracking with content that's not a prefix — e.g. an
///      in-place mutation that grew or substituted) → `Resolved(content)`,
///      treated as a committed change.
///   5. Otherwise (`Void`, no successor at frozen `start`) →
///      `Resolved(empty)`, which classifies as `Abandoned`.
fn compute_post_edit_text(
    original_text: &str,
    original: &SpanAnchor,
    anchors: &[SpanAnchor],
    line_buf: &[char],
) -> PostEdit {
    if matches!(original.state, AnchorState::Tracking) {
        let content = slice_chars(line_buf, original.start, original.end);
        let orig_chars: Vec<char> = original_text.chars().collect();
        if content == orig_chars {
            return PostEdit::Resolved(content);
        }
        // Content drifted on a Tracking anchor — try a successor first.
        if let Some(succ) = find_successor(original.id, original.start, anchors, line_buf) {
            return resolved_unless_straddles(succ);
        }
        // No successor. Distinguish mid-edit truncation (the user has
        // shrunk the word via inside-deletes and may still type more)
        // from a committed in-place edit (substitution, grow, etc.).
        // Proper prefix = the only shape inside-deletes can produce.
        if is_proper_prefix(&content, &orig_chars) {
            return PostEdit::Incomplete(content);
        }
        return resolved_unless_straddles(content);
    }

    // Void path: anchor.start is frozen at the void position. An
    // immediate retype seals a new token at this position. No incomplete
    // state for Void anchors — deletion is unambiguous.
    resolved_unless_straddles(
        find_successor(original.id, original.start, anchors, line_buf).unwrap_or_default(),
    )
}

/// **Fix-B caret-desync straddle guard (source fix).** The engine dead-reckons
/// `line_buf` from keystrokes; a caret move it can't observe (the `edndd` /
/// caret-desync class) can leave a `Tracking` anchor's `[start, end)` span
/// stretched **across a word boundary**, so [`compute_post_edit_text`] slices a
/// fragment of the *next* token onto this one — the cross-token merge
/// (`have if` recovered as `have if` / `havif`) that was poisoning capture.
///
/// A correctly-tracked word's core never contains a boundary character: the
/// tokenizer seals on whitespace, so any whitespace in a recovered post-edit is
/// proof the span straddled a boundary and the line model is stale. Such a read
/// is **not** a resolvable correction — return [`PostEdit::Incomplete`] so the
/// merged text never reaches a verdict or [`post_edit_text`] (which then yields
/// `None`, skipping capture in the store, the motor map, and the scoreboard
/// alike). Better to skip one correction than to learn a phantom merged word.
fn resolved_unless_straddles(content: Vec<char>) -> PostEdit {
    if content.iter().any(|&c| is_line_boundary_char(c)) {
        PostEdit::Incomplete(content)
    } else {
        PostEdit::Resolved(content)
    }
}

/// A character the tokenizer treats as a hard word boundary — whitespace. A
/// recovered single-word post-edit must never contain one (see
/// [`resolved_unless_straddles`]). Apostrophe / hyphen are deliberately NOT here:
/// they are legitimate intra-word characters (`don't`, `well-known`).
fn is_line_boundary_char(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\n' || c == '\r'
}

/// The post-edit text the resolver would classify for `record`, as a
/// `String` — the word the user's edits landed on.
///
/// Returns `None` when the record's anchor is no longer tracked, or when
/// the edit is still mid-flight (an `Incomplete` truncation the resolver
/// wouldn't classify yet). Component 5c (the motor map) calls this to
/// recover the *corrected* word for a `CorrectedToOther` outcome without
/// re-deriving the anchor / successor logic that lives here — the same
/// computation [`OutcomeResolver::tick`] runs internally, just surfaced
/// as text instead of being folded straight into an [`Outcome`].
pub fn post_edit_text<R: ResolvableRecord>(
    record: &R,
    anchors: &[SpanAnchor],
    line_buf: &[char],
) -> Option<String> {
    let original = anchors.iter().find(|a| a.id == record.anchor_id())?;
    match compute_post_edit_text(record.original_text(), original, anchors, line_buf) {
        PostEdit::Resolved(chars) => Some(chars.into_iter().collect()),
        PostEdit::Incomplete(_) => None,
    }
}

/// Slice-level proper-prefix check: `content` is strictly shorter than
/// `original` AND `original` starts with `content`. The exact shape an
/// inside-delete sequence on a `Tracking` anchor produces — and the
/// shape that distinguishes mid-truncation from a committed in-place
/// edit.
fn is_proper_prefix(content: &[char], original: &[char]) -> bool {
    content.len() < original.len() && original.starts_with(content)
}

/// Find the largest-end `Tracking` anchor whose `start == pos`,
/// excluding `exclude_id`. Returns the anchor and its content slice from
/// `line_buf`, or `None` if no candidate exists.
fn find_successor_anchor<'a>(
    exclude_id: u32,
    pos: usize,
    anchors: &'a [SpanAnchor],
    line_buf: &[char],
) -> Option<(&'a SpanAnchor, Vec<char>)> {
    anchors
        .iter()
        .filter(|a| matches!(a.state, AnchorState::Tracking))
        .filter(|a| a.id != exclude_id)
        .filter(|a| a.start == pos)
        .max_by_key(|a| a.end)
        .map(|a| (a, slice_chars(line_buf, a.start, a.end)))
}

/// Content-only convenience over [`find_successor_anchor`] — the
/// post-edit-text path doesn't need the anchor, just the resolved word.
fn find_successor(
    exclude_id: u32,
    pos: usize,
    anchors: &[SpanAnchor],
    line_buf: &[char],
) -> Option<Vec<char>> {
    find_successor_anchor(exclude_id, pos, anchors, line_buf).map(|(_, content)| content)
}

/// True iff `succ` is `orig` with one or more characters *inserted* and
/// nothing deleted or substituted — every character of `orig` survives,
/// in order, as a shared prefix + shared suffix, with `succ` longer in the
/// middle. This is the shape of a word still being **typed** (each
/// keystroke grows it), not a correction to a different word.
///
/// It is the fingerprint of the desync cascade: when the line buffer
/// holds a stale tail to the RIGHT of the caret (e.g. a residual
/// `"edndd"`), every keystroke re-seals a one-char-longer prefix at the
/// same `start` — `oedndd → obedndd → obsedndd → …` — and each pair is a
/// pure insertion (the shared `"edndd"` suffix + a growing prefix).
fn is_forward_growth(orig: &str, succ: &[char]) -> bool {
    let o: Vec<char> = orig.chars().collect();
    if o.len() >= succ.len() {
        return false; // a successor that didn't grow can't be forward growth
    }
    // Longest common prefix.
    let mut p = 0;
    while p < o.len() && o[p] == succ[p] {
        p += 1;
    }
    // Longest common suffix, not overlapping the matched prefix in `orig`.
    let mut s = 0;
    while s < o.len() - p && o[o.len() - 1 - s] == succ[succ.len() - 1 - s] {
        s += 1;
    }
    // All of `orig` is accounted for by the shared prefix + suffix ⇒ the
    // only difference is inserted chars in the middle ⇒ forward growth.
    p + s == o.len()
}

/// Defensive char-slice: an [`AnchorTracker`] keeps positions inside
/// `line_buf`, but if a future refactor breaks that invariant we'd
/// rather see an empty observation than panic.
fn slice_chars(line_buf: &[char], start: usize, end: usize) -> Vec<char> {
    if end <= line_buf.len() && start <= end {
        line_buf[start..end].to_vec()
    } else {
        Vec::new()
    }
}

/// Map a **committed, non-empty** text to a corrected/kept outcome.
/// `text == original` ⇒ `Kept` (a reseal of the same word); `== candidate`
/// ⇒ `CorrectedToSuggestion`; otherwise `CorrectedToOther`. Never returns
/// `Abandoned` (the empty/idle path) — the caller decides when a text is
/// committed (a successor seal). Case-sensitive exact match.
fn classify_resolved(text: &[char], original: &str, top_candidate: Option<&str>) -> Outcome {
    let now: String = text.iter().collect();
    if now == original {
        return Outcome::Kept;
    }
    if let Some(cand) = top_candidate {
        if now == cand {
            return Outcome::CorrectedToSuggestion;
        }
    }
    Outcome::CorrectedToOther
}

/// Caret sits within — or at either boundary of — this anchor's span, so
/// the user may be about to edit X. Inclusive of `end` so the
/// trailing-edge (about-to-backspace) caret counts as "in region": the
/// seal-then-correct guard that holds `Kept`.
fn caret_in_region(caret: usize, anchor: &SpanAnchor) -> bool {
    caret >= anchor.start && caret <= anchor.end
}

/// Caret is still within [`ABANDONED_CARET_MARGIN`] chars of a wiped
/// token's frozen `start` — the user is likely retyping a replacement in
/// place, so `Abandoned` is held off.
fn caret_near(caret: usize, start: usize) -> bool {
    caret.abs_diff(start) <= ABANDONED_CARET_MARGIN
}

/// The verdict state machine (Component 5a). Returns the outcome to emit,
/// or `None` to stay `Pending`. See the module-level table. Core rule:
/// **never fire mid-edit** — corrections fire only on a definitive
/// successor seal; `Kept`/`Abandoned` only after a real idle period and
/// only when the caret has left the token's working area.
#[allow(clippy::too_many_arguments)]
fn decide_verdict(
    original_text: &str,
    top_candidate: Option<&str>,
    original: &SpanAnchor,
    anchors: &[SpanAnchor],
    line_buf: &[char],
    caret: usize,
    idle_ms: u64,
    kept_idle_ms: u64,
    abandoned_idle_ms: u64,
) -> Option<Outcome> {
    // 1. Definitive: a replacement token has sealed at X's start with
    //    non-empty content. A seal is unambiguous — fire immediately,
    //    no idle wait. (Identical content ⇒ reseal ⇒ Kept.)
    if let Some((succ_anchor, succ)) =
        find_successor_anchor(original.id, original.start, anchors, line_buf)
    {
        if !succ.is_empty() {
            // Forward-growth hold: the "successor" is the same word still
            // being typed (a pure insertion that preserves all of
            // `original_text`) AND the caret is still inside it. That is
            // mid-edit growth, not a correction — hold, per the resolver's
            // core "never fire mid-edit" rule. Without this, a desynced
            // line buffer that leaves a stale tail to the right of the
            // caret makes every keystroke re-seal a one-char-longer prefix,
            // manufacturing a `CorrectedToOther` cascade (the `o→ob→obs…`
            // "edndd"-tail artifacts). Once the caret leaves the word
            // (a real boundary committed it), this releases and a genuine
            // insertion slip — `wrd → word` — still fires and is learned.
            if is_forward_growth(original_text, &succ) && caret_in_region(caret, succ_anchor) {
                return None;
            }
            return Some(classify_resolved(&succ, original_text, top_candidate));
        }
    }

    // 2. No successor — gate on anchor state, caret, and idle.
    match original.state {
        AnchorState::Tracking => {
            let slice = slice_chars(line_buf, original.start, original.end);
            let orig: Vec<char> = original_text.chars().collect();
            if slice != orig {
                // In-place edit with no seal yet → mid-edit, hold.
                return None;
            }
            // X is intact. Hold while the caret is working in/at X;
            // otherwise resolve Kept once the region has gone idle.
            if caret_in_region(caret, original) {
                return None;
            }
            (idle_ms >= kept_idle_ms).then_some(Outcome::Kept)
        }
        AnchorState::Void { .. } => {
            // Wiped, nothing sealed in its place. Hold while the caret is
            // still near the wipe site (an in-progress retype); otherwise
            // resolve Abandoned once a long idle has passed.
            if caret_near(caret, original.start) {
                return None;
            }
            (idle_ms >= abandoned_idle_ms).then_some(Outcome::Abandoned)
        }
    }
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchor::AnchorTracker;
    use crate::decision::{DecisionOutcome, LeaveAloneReason};
    use crate::log::DecisionLedger;
    use crate::score::Confidence;
    use crate::ConfidenceTier;

    // ---- Fixtures --------------------------------------------------------

    fn would_correct(original: &str, suggested: &str, score: f64) -> DecisionOutcome {
        DecisionOutcome::WouldCorrect {
            original: original.to_string(),
            suggested: suggested.to_string(),
            confidence: Confidence::High,
            score,
            runner_up_score: None,
        }
    }

    fn leave_alone(original: &str, reason: LeaveAloneReason) -> DecisionOutcome {
        DecisionOutcome::LeaveAlone {
            original: original.to_string(),
            reason,
        }
    }

    /// Append a record carrying a candidate sourced from the score
    /// report (the post-(a) shape — candidate present regardless of
    /// the decision arm).
    fn log_with_candidate(
        ledger: &mut DecisionLedger,
        decision: DecisionOutcome,
        anchor_id: u32,
        top_candidate: Option<&str>,
        top_score: Option<f64>,
    ) -> u64 {
        // Motor evidence / token motor are irrelevant for resolver
        // tests — they exercise the C5a state machine, not the C5b
        // lexicon proposer. Default to None when there's no candidate,
        // 0.5 (neutral) otherwise.
        let top_motor_evidence = if top_candidate.is_some() {
            Some(0.5)
        } else {
            None
        };
        ledger.append(
            0,
            decision,
            anchor_id,
            ConfidenceTier::Eager,
            top_candidate.map(|s| s.to_string()),
            top_score,
            top_motor_evidence,
            Some(Confidence::High),
            None,
        )
    }

    // Idle thresholds for tests — small so mock timestamps can cross
    // them. `FAR` parks the caret away from any test anchor (no region
    // guard); the verdict then turns purely on the trigger under test.
    const KEPT: u64 = 100;
    const ABANDON: u64 = 300;
    const FAR: usize = usize::MAX;

    fn resolver() -> OutcomeResolver {
        OutcomeResolver::with_thresholds(KEPT, ABANDON)
    }

    /// Tick shim — `anchors.anchors()` + arg order, once.
    fn r_tick(
        r: &mut OutcomeResolver,
        now: u64,
        anchors: &AnchorTracker,
        line: &[char],
        caret: usize,
        ledger: &DecisionLedger,
    ) -> Vec<(u64, Outcome)> {
        r.tick(now, anchors.anchors(), line, caret, ledger.iter())
    }

    // ---- Event-driven corrections (fire on the successor seal) ----------

    #[test]
    fn successor_seal_resolves_corrected_to_suggestion_immediately() {
        // "bullon " sealed; user backspaces "on" and retypes "ion ",
        // sealing "bullion" as a successor at [0,7). The seal is a
        // definitive event — CorrectedToSuggestion fires on the very tick
        // the successor is present, with NO idle wait.
        let mut line: Vec<char> = "bullon ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "bullon").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("bullon", LeaveAloneReason::BelowActiveTier),
            aid,
            Some("bullion"),
            Some(0.47),
        );

        anchors.apply_delete(6, ' ');
        line.remove(6);
        anchors.apply_delete(5, 'n');
        line.remove(5);
        anchors.apply_delete(4, 'o');
        line.remove(4);
        for (p, c) in [(4, 'i'), (5, 'o'), (6, 'n'), (7, ' ')] {
            anchors.apply_insert(p, c);
            line.insert(p, c);
        }
        anchors.try_register(0, 7, "bullion").unwrap();

        // Fires at the first tick after the seal — idle is 0 here.
        let changes = r_tick(&mut resolver(), 0, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToSuggestion)]);
    }

    #[test]
    fn successor_seal_resolves_corrected_to_other_immediately() {
        // Full backspace + retype "hello ": void at [0,0), "hello" seals.
        // ≠ original "bullon" and ≠ candidate "bullion" → CorrectedToOther,
        // immediately on the seal tick.
        let mut line: Vec<char> = "bullon ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "bullon").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("bullon", LeaveAloneReason::BelowActiveTier),
            aid,
            Some("bullion"),
            Some(0.47),
        );

        anchors.apply_delete(6, ' ');
        line.remove(6);
        for (p, c) in [(5, 'n'), (4, 'o'), (3, 'l'), (2, 'l'), (1, 'u'), (0, 'b')] {
            anchors.apply_delete(p, c);
            line.remove(p);
        }
        for (i, c) in "hello ".chars().enumerate() {
            anchors.apply_insert(i, c);
            line.insert(i, c);
        }
        anchors.try_register(0, 5, "hello").unwrap();

        let changes = r_tick(&mut resolver(), 0, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    // ---- Forward-growth hold — the "edndd" desync cascade ---------------

    #[test]
    fn edndd_forward_growth_cascade_is_held_while_typing() {
        // REPRODUCTION of the stale-tail artifact cascade. A desynced line
        // buffer holds a residual 5-char tail ("edndd") to the RIGHT of the
        // caret. As the user types "obstacles", every keystroke re-seals a
        // one-char-longer prefix token at start 0:
        //   oedndd, obedndd, obsedndd, … , obstaclesedndd
        // Each prefix is a Pending record; the longest is the live successor
        // at [0, 14). The caret sits at the typing edge (index 9, just before
        // the residual "edndd"), so it is INSIDE the successor span.
        //
        // Pre-fix: decide_verdict step 1 paired each prefix with the longest
        // successor and fired CorrectedToOther for every one — the junk
        // `typed → target` pairs. Post-fix: each pair is a pure forward
        // growth under the caret, so the resolver HOLDS and fires nothing.
        let line: Vec<char> = "obstaclesedndd".chars().collect(); // 14 chars
        let typing_edge = 9; // caret just after "obstacles", before "edndd"

        let prefixes = [
            "o",
            "ob",
            "obs",
            "obst",
            "obsta",
            "obstac",
            "obstacl",
            "obstacle",
            "obstacles",
        ];
        let mut anchors = AnchorTracker::new();
        let mut ledger = DecisionLedger::new();
        let mut rids = Vec::new();
        for (i, p) in prefixes.iter().enumerate() {
            let core = format!("{p}edndd");
            let end = p.chars().count() + 5; // prefix + "edndd"
            let aid = anchors.try_register(0, end, &core).unwrap();
            // The longest prefix is the live word the user is on — no Pending
            // verdict is owed for it; the owed verdicts are the shorter ones.
            if i + 1 < prefixes.len() {
                rids.push(log_with_candidate(
                    &mut ledger,
                    leave_alone(&core, LeaveAloneReason::BelowActiveTier),
                    aid,
                    None,
                    None,
                ));
            }
        }

        // One tick with the caret at the typing edge: NOTHING resolves.
        let changes = r_tick(&mut resolver(), 0, &anchors, &line, typing_edge, &ledger);
        assert!(
            changes.is_empty(),
            "forward-growth cascade must be held while the caret is inside the \
             growing word; got {changes:?}"
        );
        assert_eq!(rids.len(), 8, "test premise: 8 owed prefix verdicts");
    }

    #[test]
    fn genuine_insertion_slip_still_fires_once_committed() {
        // The complement: a REAL insertion slip the user finished and moved
        // past. "wrd" sealed, then they inserted "o" → "word" and the caret
        // left the word (parked at FAR). Even though "word" is a forward
        // growth of "wrd", the caret is NOT in its region — so it commits as
        // CorrectedToOther and is still learned. The hold only suppresses
        // growth happening UNDER the caret.
        let line: Vec<char> = "word".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "wrd").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("wrd", LeaveAloneReason::BelowActiveTier),
            aid,
            None,
            None,
        );
        anchors.try_register(0, 4, "word").unwrap();

        let changes = r_tick(&mut resolver(), 0, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    #[test]
    fn is_forward_growth_distinguishes_growth_from_correction() {
        let cs: Vec<char> = "obedndd".chars().collect();
        assert!(is_forward_growth("oedndd", &cs)); // grew by one, shared tail
        let cs: Vec<char> = "word".chars().collect();
        assert!(is_forward_growth("wrd", &cs)); // inserted 'o'
                                                // Transposition (teh→the) is NOT growth — equal length, real fix.
        let cs: Vec<char> = "the".chars().collect();
        assert!(!is_forward_growth("teh", &cs));
        // Substitution (cat→cot) is NOT growth.
        let cs: Vec<char> = "cot".chars().collect();
        assert!(!is_forward_growth("cat", &cs));
        // Different word of greater length but with a deletion is NOT a pure
        // insertion: "hello" from "bullon" — no shared prefix+suffix cover.
        let cs: Vec<char> = "hello".chars().collect();
        assert!(!is_forward_growth("bullon", &cs));
    }

    // ---- Kept — idle-gated, caret-region-guarded ------------------------

    #[test]
    fn untouched_word_resolves_kept_after_idle() {
        // "teh " sealed, never touched, caret elsewhere. No verdict until
        // the region has been idle ≥ KEPT; then Kept.
        let line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            would_correct("teh", "the", 0.82),
            aid,
            Some("the"),
            Some(0.82),
        );

        let mut r = resolver();
        assert!(r_tick(&mut r, 0, &anchors, &line, FAR, &ledger).is_empty());
        assert!(
            r_tick(&mut r, KEPT - 1, &anchors, &line, FAR, &ledger).is_empty(),
            "no Kept before the idle threshold"
        );
        let changes = r_tick(&mut r, KEPT, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
    }

    #[test]
    fn kept_is_held_while_caret_is_in_the_word_region() {
        // Same untouched word, but the caret is parked inside/at the span
        // (the user is poised to edit it). Even far past the idle bar, no
        // Kept fires — only once the caret leaves does Kept resolve.
        let line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            would_correct("teh", "the", 0.82),
            aid,
            Some("the"),
            Some(0.82),
        );

        let mut r = resolver();
        // caret == end (3): the about-to-backspace trailing edge counts
        // as "in region" — the seal-then-correct guard.
        r_tick(&mut r, 0, &anchors, &line, 3, &ledger);
        assert!(
            r_tick(&mut r, 10 * KEPT, &anchors, &line, 3, &ledger).is_empty(),
            "caret at the trailing edge must hold Kept indefinitely"
        );
        // Caret leaves → Kept resolves after a fresh idle period.
        r_tick(&mut r, 10 * KEPT, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, 11 * KEPT, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
    }

    // ---- Abandoned — idle + caret-distance-gated ------------------------

    #[test]
    fn wiped_word_resolves_abandoned_after_idle_when_caret_away() {
        // "bullon " fully deleted, line empty, caret moved away. After a
        // long idle with no successor, Abandoned.
        let mut line: Vec<char> = "bullon ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "bullon").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("bullon", LeaveAloneReason::BelowActiveTier),
            aid,
            Some("bullion"),
            Some(0.47),
        );

        anchors.apply_delete(6, ' ');
        line.remove(6);
        for (p, c) in [(5, 'n'), (4, 'o'), (3, 'l'), (2, 'l'), (1, 'u'), (0, 'b')] {
            anchors.apply_delete(p, c);
            line.remove(p);
        }
        assert!(line.is_empty());

        let mut r = resolver();
        assert!(r_tick(&mut r, 0, &anchors, &line, FAR, &ledger).is_empty());
        assert!(
            r_tick(&mut r, ABANDON - 1, &anchors, &line, FAR, &ledger).is_empty(),
            "no Abandoned before the idle threshold"
        );
        let changes = r_tick(&mut r, ABANDON, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Abandoned)]);
    }

    #[test]
    fn abandoned_is_held_while_caret_is_near_the_wipe_site() {
        // Same wipe, but the caret stays at the deletion site (an
        // in-progress in-place retype). Even far past the idle bar, no
        // Abandoned — the caret-near guard holds it Pending.
        let mut line: Vec<char> = "bullon ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "bullon").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("bullon", LeaveAloneReason::BelowActiveTier),
            aid,
            Some("bullion"),
            Some(0.47),
        );

        anchors.apply_delete(6, ' ');
        line.remove(6);
        for (p, c) in [(5, 'n'), (4, 'o'), (3, 'l'), (2, 'l'), (1, 'u'), (0, 'b')] {
            anchors.apply_delete(p, c);
            line.remove(p);
        }

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, 0, &ledger);
        assert!(
            r_tick(&mut r, 100 * ABANDON, &anchors, &line, 0, &ledger).is_empty(),
            "caret at the wipe site must hold Abandoned indefinitely"
        );
        assert_eq!(ledger.get(rid).unwrap().outcome, Outcome::Pending);
    }

    // ---- Mid-edit holds Pending (no verdict mid-edit) -------------------

    #[test]
    fn truncation_holds_pending_even_past_idle() {
        // Kept word, then the user shrinks it to a proper prefix ("wor")
        // and pauses. Tracking with a changed slice and no successor =
        // mid-edit: the resolver must NOT fire any verdict, even far past
        // the idle bar. The record stays at its prior outcome (Kept).
        let mut line: Vec<char> = "wordl ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 5, "wordl").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("wordl", LeaveAloneReason::BelowActiveTier),
            aid,
            Some("world"),
            Some(0.50),
        );

        let mut r = resolver();
        // Phase 1: resolve Kept on the untouched word.
        r_tick(&mut r, 0, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, KEPT, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Phase 2: backspace ' ', 'l', 'd' → anchor truncates to "wor".
        anchors.apply_delete(5, ' ');
        line.remove(5);
        anchors.apply_delete(4, 'l');
        line.remove(4);
        anchors.apply_delete(3, 'd');
        line.remove(3);

        r_tick(&mut r, 200, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, 200 + 100 * KEPT, &anchors, &line, FAR, &ledger);
        assert!(
            changes.is_empty(),
            "truncated mid-edit must not flip Kept → anything"
        );
        assert_eq!(ledger.get(rid).unwrap().outcome, Outcome::Kept);
    }

    #[test]
    fn truncation_then_seal_resolves_to_corrected_to_suggestion() {
        // Continuation: after truncating to "wor", the user retypes and
        // seals "world". The seal is the definitive event → Kept revises
        // to CorrectedToSuggestion immediately, no idle wait.
        let mut line: Vec<char> = "wordl ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 5, "wordl").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("wordl", LeaveAloneReason::BelowActiveTier),
            aid,
            Some("world"),
            Some(0.50),
        );

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, FAR, &ledger);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Truncate to "wor" — mid-edit, no transition.
        anchors.apply_delete(5, ' ');
        line.remove(5);
        anchors.apply_delete(4, 'l');
        line.remove(4);
        anchors.apply_delete(3, 'd');
        line.remove(3);
        let changes = r_tick(&mut r, 200, &anchors, &line, 3, &ledger);
        assert!(changes.is_empty(), "mid-edit holds");

        // Commit: type 'l', 'd', ' ' — trailing space seals "world".
        anchors.apply_insert(3, 'l');
        line.insert(3, 'l');
        anchors.apply_insert(4, 'd');
        line.insert(4, 'd');
        anchors.apply_insert(5, ' ');
        line.insert(5, ' ');
        anchors.try_register(0, 5, "world").unwrap();

        // Fires on the seal tick — no debounce.
        let changes = r_tick(&mut r, 260, &anchors, &line, 5, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToSuggestion)]);
    }

    // ---- Revisable transitions (latest write wins) ----------------------

    #[test]
    fn revisable_kept_then_full_delete_re_resolves_to_abandoned() {
        // Phase 1: "bullon " resolves Kept (idle). Phase 2: backspace
        // everything, caret moves away, long idle → revises to Abandoned.
        let mut line: Vec<char> = "bullon ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "bullon").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("bullon", LeaveAloneReason::BelowActiveTier),
            aid,
            Some("bullion"),
            Some(0.47),
        );

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, KEPT, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)], "phase 1: Kept");
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        anchors.apply_delete(6, ' ');
        line.remove(6);
        for (p, c) in [(5, 'n'), (4, 'o'), (3, 'l'), (2, 'l'), (1, 'u'), (0, 'b')] {
            anchors.apply_delete(p, c);
            line.remove(p);
        }
        assert!(line.is_empty());

        // Caret away + long idle → Kept revises to Abandoned.
        r_tick(&mut r, 200, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, 200 + ABANDON, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Abandoned)]);
    }

    #[test]
    fn revisable_abandoned_then_retype_re_resolves_to_corrected_to_other() {
        // Phase 1: "bullon " wiped, caret away, long idle → Abandoned.
        // Phase 2: retype "hello " — the seal revises to CorrectedToOther
        // immediately.
        let mut line: Vec<char> = "bullon ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "bullon").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("bullon", LeaveAloneReason::BelowActiveTier),
            aid,
            Some("bullion"),
            Some(0.47),
        );

        anchors.apply_delete(6, ' ');
        line.remove(6);
        for (p, c) in [(5, 'n'), (4, 'o'), (3, 'l'), (2, 'l'), (1, 'u'), (0, 'b')] {
            anchors.apply_delete(p, c);
            line.remove(p);
        }
        assert!(line.is_empty());

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, ABANDON, &anchors, &line, FAR, &ledger);
        assert_eq!(
            changes,
            vec![(rid, Outcome::Abandoned)],
            "phase 1: Abandoned"
        );
        assert!(ledger.resolve_outcome(rid, Outcome::Abandoned));

        for (i, c) in "hello ".chars().enumerate() {
            anchors.apply_insert(i, c);
            line.insert(i, c);
        }
        anchors.try_register(0, 5, "hello").unwrap();

        // Seal → immediate revise to CorrectedToOther.
        let changes = r_tick(&mut r, ABANDON + 50, &anchors, &line, 6, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    // ---- Seal-then-correct gesture: intermediate-state coverage ---------

    #[test]
    fn worxc_to_word_gesture_no_verdict_until_seal_then_corrected_to_other() {
        // The live acceptance gesture: `worxc ` → backspace through it →
        // `word `. Asserts the verdict state at EVERY intermediate point
        // (after each backspace, after each replacement char), not just at
        // the end — the previous fix passed end-state tests but failed
        // live mid-gesture. The two failure modes this pins:
        //   * premature Kept at the leading edge of backspacing, and
        //   * premature Abandoned mid-retype (empty span read as
        //     abandoned while the caret is right there typing).
        // Nothing may fire until `word` seals; then CorrectedToOther.
        //
        // Stress: most ticks below run at idle FAR past KEPT/ABANDON, so
        // the *caret guards* — not luck of timing — are what hold the
        // verdict. Unknown word / no candidate so the result can't be
        // misread as CorrectedToSuggestion.
        let mut line: Vec<char> = "worxc ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 5, "worxc").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("worxc", LeaveAloneReason::NoCandidates),
            aid,
            None,
            None,
        );
        let big = 100 * ABANDON; // idle well past every threshold
        let mut r = resolver();

        let pending = |r: &mut OutcomeResolver,
                       t: u64,
                       anchors: &AnchorTracker,
                       line: &[char],
                       caret: usize,
                       ledger: &DecisionLedger,
                       msg: &str| {
            assert!(
                r_tick(r, t, anchors, line, caret, ledger).is_empty(),
                "{msg}"
            );
            assert_eq!(ledger.get(rid).unwrap().outcome, Outcome::Pending, "{msg}");
        };

        // Seal: caret after the space (6) is NOT in worxc's span [0,5).
        // Hold idle below KEPT here — a real notice-pause is short.
        pending(&mut r, 0, &anchors, &line, 6, &ledger, "at seal");
        pending(
            &mut r,
            KEPT - 1,
            &anchors,
            &line,
            6,
            &ledger,
            "sub-KEPT notice pause",
        );

        // Backspace the trailing space → caret 5 == worxc.end (in region).
        // Even far past the idle bar, the caret-in-region guard holds Kept.
        anchors.apply_delete(5, ' ');
        line.remove(5);
        pending(
            &mut r,
            big,
            &anchors,
            &line,
            5,
            &ledger,
            "space deleted, caret at edge",
        );

        // Backspace through worxc (c,x,r,o,w); caret 4→0. Each tick at
        // huge idle. Tracking-shrink is mid-edit; the final delete voids.
        let mut t = big;
        for (p, c) in [(4, 'c'), (3, 'x'), (2, 'r'), (1, 'o'), (0, 'w')] {
            anchors.apply_delete(p, c);
            line.remove(p);
            t += big;
            pending(&mut r, t, &anchors, &line, p, &ledger, "mid backspace");
        }
        assert!(line.is_empty());

        // Empty span, caret at the wipe site (0), idle ≫ ABANDON — this is
        // exactly the premature-Abandoned window. The caret-near guard
        // must hold it Pending.
        t += big;
        pending(
            &mut r,
            t,
            &anchors,
            &line,
            0,
            &ledger,
            "wiped, caret at site, idle past ABANDON",
        );

        // Retype "word"; caret 1→4. No successor until the space seals,
        // and idle keeps growing — but the caret stays within
        // ABANDONED_CARET_MARGIN of the wipe site, so no Abandoned fires.
        for (i, c) in "word".chars().enumerate() {
            anchors.apply_insert(i, c);
            line.insert(i, c);
            t += big;
            pending(
                &mut r,
                t,
                &anchors,
                &line,
                i + 1,
                &ledger,
                "mid retype, no premature Abandoned",
            );
        }

        // Seal "word ": the definitive event. CorrectedToOther fires now,
        // and only now — never Kept, never Abandoned, at any point above.
        anchors.apply_insert(4, ' ');
        line.insert(4, ' ');
        anchors.try_register(0, 4, "word").unwrap();
        let changes = r_tick(&mut r, t + 10, &anchors, &line, 5, &ledger);
        assert_eq!(
            changes,
            vec![(rid, Outcome::CorrectedToOther)],
            "worxc → word resolves CorrectedToOther on the seal — never Kept/Abandoned"
        );
    }

    // ---- Position shift vs content edit --------------------------------

    #[test]
    fn position_shift_only_does_not_flip_kept() {
        // "teh foo" — "teh" at [0,3) resolves Kept. Typing 'x' at the
        // start shifts "teh" to [1,4) with unchanged content. The Kept
        // resolution must persist (no re-emit, no flip).
        let mut line: Vec<char> = "teh foo".chars().collect();
        let mut anchors = AnchorTracker::new();
        let teh = anchors.try_register(0, 3, "teh").unwrap();
        anchors.try_register(4, 7, "foo").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            would_correct("teh", "the", 0.82),
            teh,
            Some("the"),
            Some(0.82),
        );

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, KEPT, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Insert 'x' at p=0 → "teh" shifts to [1,4); content still "teh".
        anchors.apply_insert(0, 'x');
        line.insert(0, 'x');

        let changes = r_tick(&mut r, 100 * KEPT, &anchors, &line, FAR, &ledger);
        assert!(
            changes.is_empty(),
            "shift-only must not re-resolve a Kept record"
        );
    }

    // ---- Revisability ---------------------------------------------------

    #[test]
    fn kept_record_re_resolves_when_corrected_by_reseal() {
        // "teh " resolves Kept. The user then corrects it the realistic
        // way — shrink to "t", retype "he", seal "the" as a successor.
        // The seal revises Kept → CorrectedToSuggestion.
        let mut line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            would_correct("teh", "the", 0.82),
            aid,
            Some("the"),
            Some(0.82),
        );

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, KEPT, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Shrink "teh" → "t" (backspace ' ', 'h', 'e'), retype "he ",
        // sealing "the" as a successor at [0,3).
        anchors.apply_delete(3, ' ');
        line.remove(3);
        anchors.apply_delete(2, 'h');
        line.remove(2);
        anchors.apply_delete(1, 'e');
        line.remove(1);
        for (p, c) in [(1, 'h'), (2, 'e'), (3, ' ')] {
            anchors.apply_insert(p, c);
            line.insert(p, c);
        }
        anchors.try_register(0, 3, "the").unwrap();

        // The seal revises immediately.
        let changes = r_tick(&mut r, KEPT + 50, &anchors, &line, 4, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToSuggestion)]);
    }

    // ---- Edge cases ------------------------------------------------------

    #[test]
    fn no_candidates_record_with_unchanged_content_resolves_to_kept() {
        // Names like "Soumyo" land on LeaveAlone(NoCandidates) — no
        // top_candidate. Untouched + idle → Kept regardless.
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "Soumyo").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("Soumyo", LeaveAloneReason::NoCandidates),
            aid,
            None,
            None,
        );
        let line: Vec<char> = "Soumyo ".chars().collect();

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, KEPT, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
    }

    #[test]
    fn no_candidates_record_with_resealed_content_resolves_to_corrected_to_other() {
        // Soumyo with no candidate: a successor seal with different
        // content is CorrectedToOther (ToSuggestion is unreachable),
        // immediately on the seal.
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "Soumyo").unwrap();
        anchors.apply_delete(5, 'o');
        anchors.apply_delete(4, 'y');
        anchors.try_register(0, 6, "Saumyo").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("Soumyo", LeaveAloneReason::NoCandidates),
            aid,
            None,
            None,
        );
        let line: Vec<char> = "Saumyo ".chars().collect();

        let changes = r_tick(&mut resolver(), 0, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    #[test]
    fn idle_clock_resets_on_each_content_edit() {
        // The idle clock (`stable_since_ms`) resets whenever the
        // observation tuple changes. Two inside-deletes shrink
        // "teh" → "te" → "t"; each is a distinct observation, so each
        // resets the clock — and the record never resolves while
        // truncated (mid-edit hold), even past the idle bar.
        let mut line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            would_correct("teh", "the", 0.82),
            aid,
            Some("the"),
            Some(0.82),
        );

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, 3, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(0));

        anchors.apply_delete(2, 'h');
        line.remove(2);
        r_tick(&mut r, 50, &anchors, &line, 2, &ledger);
        assert_eq!(
            r.stable_since_ms(aid),
            Some(50),
            "edit resets the idle clock"
        );

        anchors.apply_delete(1, 'e');
        line.remove(1);
        r_tick(&mut r, 80, &anchors, &line, 1, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(80));

        // Truncated, far past the idle bar → still no verdict (mid-edit).
        let changes = r_tick(&mut r, 80 + 100 * KEPT, &anchors, &line, 1, &ledger);
        assert!(changes.is_empty(), "truncated mid-edit must hold");
        assert_eq!(ledger.get(rid).unwrap().outcome, Outcome::Pending);
    }

    #[test]
    fn line_reset_garbage_collects_resolver_cache() {
        let mut line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let _rid = log_with_candidate(
            &mut ledger,
            would_correct("teh", "the", 0.82),
            aid,
            Some("the"),
            Some(0.82),
        );

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, FAR, &ledger);
        assert!(r.stable_since_ms(aid).is_some());

        anchors.clear();
        line.clear();
        r_tick(&mut r, 1, &anchors, &line, FAR, &ledger);
        assert!(r.stable_since_ms(aid).is_none());
    }

    #[test]
    fn does_not_re_emit_unchanged_resolution() {
        let line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            would_correct("teh", "the", 0.82),
            aid,
            Some("the"),
            Some(0.82),
        );

        let mut r = resolver();
        r_tick(&mut r, 0, &anchors, &line, FAR, &ledger);
        let changes = r_tick(&mut r, KEPT, &anchors, &line, FAR, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Kept already holds — later ticks recompute Kept but emit nothing.
        assert!(r_tick(&mut r, 2 * KEPT, &anchors, &line, FAR, &ledger).is_empty());
        assert!(r_tick(&mut r, 100 * KEPT, &anchors, &line, FAR, &ledger).is_empty());
    }

    // ---- classify_resolved (committed-text → outcome) -------------------

    fn cs(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn classify_resolved_kept_path() {
        assert_eq!(
            classify_resolved(&cs("teh"), "teh", Some("the")),
            Outcome::Kept
        );
    }

    #[test]
    fn classify_resolved_corrected_to_suggestion_path() {
        assert_eq!(
            classify_resolved(&cs("the"), "teh", Some("the")),
            Outcome::CorrectedToSuggestion
        );
    }

    #[test]
    fn classify_resolved_corrected_to_other_path() {
        assert_eq!(
            classify_resolved(&cs("tax"), "teh", Some("the")),
            Outcome::CorrectedToOther
        );
        // No candidate → any non-matching text is ToOther.
        assert_eq!(
            classify_resolved(&cs("tax"), "teh", None),
            Outcome::CorrectedToOther
        );
    }

    // ---- is_proper_prefix ----------------------------------------------

    #[test]
    fn proper_prefix_detection() {
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        // True prefixes — what inside-deletes produce.
        assert!(is_proper_prefix(&chars("wor"), &chars("wordl")));
        assert!(is_proper_prefix(&chars(""), &chars("wordl")));
        assert!(is_proper_prefix(&chars("w"), &chars("wordl")));
        // Equal length — not a *proper* prefix.
        assert!(!is_proper_prefix(&chars("wordl"), &chars("wordl")));
        // Same start but diverges — not a prefix.
        assert!(!is_proper_prefix(&chars("wol"), &chars("wordl")));
        // Longer than original (e.g. mid-line insert grew the anchor) —
        // can't be a prefix of original.
        assert!(!is_proper_prefix(&chars("bullion"), &chars("bullon")));
    }

    // ---- Fix-B caret-desync straddle guard (no cross-token merge) --------

    #[test]
    fn caret_jump_straddle_does_not_capture_a_merge() {
        // Reproduces the caret-desync class. The user typed "have if". A caret
        // move the engine couldn't observe left the "have" anchor's span
        // stretched across the boundary into the next word, so its tracked span
        // is now [0,7) over the whole "have if" — the exact desync that made
        // `compute_post_edit_text` recover a cross-token merge and poison
        // capture (`have → havif`).
        let line: Vec<char> = "have if".chars().collect();
        let mut anchors = AnchorTracker::new();
        // end = 7 (not 4): the stale span straddles the space.
        let aid = anchors.try_register(0, 7, "have").unwrap();
        let mut ledger = DecisionLedger::new();
        log_with_candidate(
            &mut ledger,
            leave_alone("have", LeaveAloneReason::BelowActiveTier),
            aid,
            None,
            None,
        );
        let rec = ledger.iter().next().unwrap();

        // The straddle guard must refuse to recover a boundary-crossing span:
        // no merged text enters capture (post_edit_text → None), so the
        // CorrectedToOther arm in the engine skips the store, the motor map,
        // AND the scoreboard together.
        let recovered = post_edit_text(rec, anchors.anchors(), &line);
        assert_eq!(
            recovered, None,
            "a span straddling a word boundary must not capture a merge, got {recovered:?}"
        );
    }

    #[test]
    fn in_place_correction_without_boundary_still_resolves() {
        // Regression guard: a normal in-place edit (no boundary char in the
        // recovered span) must STILL be captured — the straddle guard only
        // rejects boundary-crossing reads, never legitimate single-word fixes.
        let line: Vec<char> = "havs".chars().collect(); // "have" edited in place
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 4, "have").unwrap();
        let mut ledger = DecisionLedger::new();
        log_with_candidate(
            &mut ledger,
            leave_alone("have", LeaveAloneReason::BelowActiveTier),
            aid,
            None,
            None,
        );
        let rec = ledger.iter().next().unwrap();

        assert_eq!(
            post_edit_text(rec, anchors.anchors(), &line).as_deref(),
            Some("havs"),
            "a boundary-free in-place edit must still resolve for capture"
        );
    }
}
