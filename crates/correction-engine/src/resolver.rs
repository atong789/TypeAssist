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
//! ## Classification
//!
//! Given the post-edit observation:
//!
//! | post-edit observation                          | Outcome                 |
//! |------------------------------------------------|-------------------------|
//! | `Resolved(empty)`                              | `Abandoned`             |
//! | `Resolved(text) == record.original_text`        | `Kept`                  |
//! | `Resolved(text) == record.top_candidate`        | `CorrectedToSuggestion` |
//! | `Resolved(other non-empty word)`                | `CorrectedToOther`      |
//! | `Incomplete(_)` (mid-edit truncation)           | no transition           |
//!
//! `Incomplete` is the load-bearing addition for transpositions and
//! similar shrink-then-retype corrections (e.g. `wordl → world`). The
//! user's anchor shrinks via inside-deletes to a proper prefix of
//! `original_text` while they prepare to retype past the truncation —
//! during that window there's no successor yet, but the truncated
//! prefix isn't a committed outcome either. Treating it as committed
//! produced a premature `CorrectedToOther` that wouldn't recover when
//! the eventual seal landed (ticks are keystroke-gated; a pause after
//! the seal leaves the wrong classification in place). `Incomplete`
//! keeps the record at its previous outcome until either a successor
//! seals or the user reverts.
//!
//! `CorrectedToSuggestion` is **independent of the decision arm** —
//! the candidate is stored on every loggable record regardless of
//! whether the mode chose to act on it. The `bullon → bullion` case
//! under Cautious resolves to `CorrectedToSuggestion` because the
//! candidate ("bullion") was in the score report, even though
//! Cautious's `LeaveAlone(BelowActiveTier)` wouldn't have fired the
//! correction.
//!
//! Match is **case-sensitive exact** between content chars and
//! `original_text` / `top_candidate`. C5a doesn't try to normalise case;
//! the lexicon's case-insensitive lookups happen upstream when the
//! candidate is generated. Refine in C5b if real usage shows we need it.
//!
//! ## Debounce + revisability
//!
//! The resolver tracks the per-record observation tuple
//! `(original_state, post_edit_text)`. Each tick that detects a change
//! resets the per-anchor `stable_since_ms` timestamp; a record only
//! resolves once [`DEFAULT_DEBOUNCE_MS`] elapses without further
//! change. A resolved record re-resolves if the observation later
//! changes — the resolver compares the newly computed outcome against
//! the record's CURRENT outcome in the ledger and only emits a
//! transition when they differ. Once the original anchor is retired
//! (line reset → `anchors.clear()`), the record's resolver entry is
//! GC'd on the next tick and the last resolution stands forever.

use std::collections::HashMap;

use crate::anchor::{AnchorState, SpanAnchor};
use crate::log::{DecisionLedger, Outcome};

/// Default debounce — short enough to feel live in the debug panel, long
/// enough to swallow normal mid-word pauses (a typist hovering at ~3 cps
/// has ~330 ms between keystrokes). Tune from real usage data.
pub const DEFAULT_DEBOUNCE_MS: u64 = 600;

/// Resolver version. Bump on any change to the resolution policy that
/// downstream Components or the debug panel could observe.
pub const RESOLVER_VERSION: u32 = 3;

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
/// Deliberately keyed on `(state, post_edit)` — position is NOT part
/// of the stability tuple, so a pure shift (insert/delete elsewhere on
/// the line) doesn't reset the debounce.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    state: AnchorState,
    post_edit: PostEdit,
}

#[derive(Debug, Clone)]
struct Observed {
    obs: Observation,
    stable_since_ms: u64,
}

/// Per-anchor outcome resolver. Owns the per-anchor observation cache
/// (last-seen state, when the current state was first observed) needed
/// to apply the debounce. Stateless across runs — lives in process
/// memory only, like the [`DecisionLedger`] it resolves into.
#[derive(Debug)]
pub struct OutcomeResolver {
    last_seen: HashMap<u32, Observed>,
    debounce_ms: u64,
}

impl Default for OutcomeResolver {
    fn default() -> Self {
        Self::with_debounce_ms(DEFAULT_DEBOUNCE_MS)
    }
}

impl OutcomeResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_debounce_ms(debounce_ms: u64) -> Self {
        Self {
            last_seen: HashMap::new(),
            debounce_ms,
        }
    }

    pub fn debounce_ms(&self) -> u64 {
        self.debounce_ms
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
    pub fn tick(
        &mut self,
        now_ms: u64,
        anchors: &[SpanAnchor],
        line_buf: &[char],
        ledger: &DecisionLedger,
    ) -> Vec<(u64, Outcome)> {
        // GC: drop cache entries for anchors that are no longer tracked.
        // Cheap because the typical line carries a handful of anchors.
        let live: std::collections::HashSet<u32> = anchors.iter().map(|a| a.id).collect();
        self.last_seen.retain(|id, _| live.contains(id));

        // Walk the ledger, compute per-record observation, refresh the
        // cache, and queue transitions for any record whose stable
        // observation maps to a different outcome than what the ledger
        // currently holds.
        let mut changes = Vec::new();
        for record in ledger.iter() {
            let Some(original) = anchors.iter().find(|a| a.id == record.anchor_id) else {
                continue;
            };

            let post_edit = compute_post_edit_text(record, original, anchors, line_buf);
            let obs = Observation {
                state: original.state.clone(),
                post_edit,
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

            // Saturating to guard against a non-monotonic `now_ms` (test
            // fixtures, time skew). A negative delta would otherwise
            // wrap into a huge u64 and short-circuit the debounce.
            let stable_since = self.last_seen[&original.id].stable_since_ms;
            let elapsed = now_ms.saturating_sub(stable_since);
            if elapsed < self.debounce_ms {
                continue;
            }
            let Some(computed) = classify(
                &obs.post_edit,
                &record.original_text,
                record.top_candidate.as_deref(),
            ) else {
                // Mid-edit — record stays at its previous outcome until
                // the user commits (or the observation resolves back to
                // the original content for Kept).
                continue;
            };
            if record.outcome != computed {
                changes.push((record.id, computed));
            }
        }
        changes
    }
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
    record: &crate::log::LogRecord,
    original: &SpanAnchor,
    anchors: &[SpanAnchor],
    line_buf: &[char],
) -> PostEdit {
    if matches!(original.state, AnchorState::Tracking) {
        let content = slice_chars(line_buf, original.start, original.end);
        let orig_chars: Vec<char> = record.original_text.chars().collect();
        if content == orig_chars {
            return PostEdit::Resolved(content);
        }
        // Content drifted on a Tracking anchor — try a successor first.
        if let Some(succ) = find_successor(original.id, original.start, anchors, line_buf) {
            return PostEdit::Resolved(succ);
        }
        // No successor. Distinguish mid-edit truncation (the user has
        // shrunk the word via inside-deletes and may still type more)
        // from a committed in-place edit (substitution, grow, etc.).
        // Proper prefix = the only shape inside-deletes can produce.
        if is_proper_prefix(&content, &orig_chars) {
            return PostEdit::Incomplete(content);
        }
        return PostEdit::Resolved(content);
    }

    // Void path: anchor.start is frozen at the void position. An
    // immediate retype seals a new token at this position. No incomplete
    // state for Void anchors — deletion is unambiguous.
    PostEdit::Resolved(
        find_successor(original.id, original.start, anchors, line_buf).unwrap_or_default(),
    )
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
/// excluding `exclude_id`. Returns the anchor's content slice from
/// `line_buf`, or `None` if no candidate exists.
fn find_successor(
    exclude_id: u32,
    pos: usize,
    anchors: &[SpanAnchor],
    line_buf: &[char],
) -> Option<Vec<char>> {
    anchors
        .iter()
        .filter(|a| matches!(a.state, AnchorState::Tracking))
        .filter(|a| a.id != exclude_id)
        .filter(|a| a.start == pos)
        .max_by_key(|a| a.end)
        .map(|a| slice_chars(line_buf, a.start, a.end))
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

/// Pure classifier. `None` ⇒ mid-edit, don't emit a transition.
/// `Some(outcome)` ⇒ the committed outcome for the post-edit text.
fn classify(post_edit: &PostEdit, original: &str, top_candidate: Option<&str>) -> Option<Outcome> {
    let text = match post_edit {
        PostEdit::Incomplete(_) => return None,
        PostEdit::Resolved(t) => t,
    };
    if text.is_empty() {
        return Some(Outcome::Abandoned);
    }
    let now: String = text.iter().collect();
    if now == original {
        return Some(Outcome::Kept);
    }
    if let Some(cand) = top_candidate {
        if now == cand {
            return Some(Outcome::CorrectedToSuggestion);
        }
    }
    Some(Outcome::CorrectedToOther)
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchor::{AnchorTracker, VoidReason};
    use crate::decision::{DecisionOutcome, LeaveAloneReason};
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
        ledger.append(
            0,
            decision,
            anchor_id,
            ConfidenceTier::Eager,
            top_candidate.map(|s| s.to_string()),
            top_score,
            Some(Confidence::High),
        )
    }

    // ---- Core outcomes --------------------------------------------------

    #[test]
    fn pending_resolves_to_kept_when_word_is_untouched() {
        // Type "teh ", token seals, anchor [0,3) for "teh". User does
        // not touch the word again. After the debounce elapses, the
        // record resolves to Kept.
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        let changes = r.tick(0, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty(), "no resolution before debounce");
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
    }

    #[test]
    fn mid_line_insert_resolves_to_corrected_to_suggestion() {
        // "bullon" sealed at [0,6). User navigates back, inserts 'i'
        // mid-word: apply_insert(4, 'i') lands inside [0,6), end+=1 →
        // anchor grows to [0,7) and its content slice is now "bullion".
        // The tokenizer rebuild seals "bullion" as a SEPARATE anchor at
        // [0,7) (different original_core → no dedupe).
        let mut line: Vec<char> = "bullon ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "bullon").unwrap();
        let mut ledger = DecisionLedger::new();
        // Cautious tier with the candidate stored (LeaveAlone arm) —
        // this is the failing real-world case the brief flagged.
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("bullon", LeaveAloneReason::BelowActiveTier),
            aid,
            Some("bullion"),
            Some(0.47),
        );

        anchors.apply_insert(4, 'i');
        line.insert(4, 'i');
        // Engine-side: the mid-line insert triggers a tokenizer replay
        // that re-seals "bullion" as a new anchor. Model that here.
        let _new_aid = anchors.try_register(0, 7, "bullion").unwrap();

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(
            changes,
            vec![(rid, Outcome::CorrectedToSuggestion)],
            "candidate-stored + successor-lookup must attribute ToSuggestion"
        );
    }

    #[test]
    fn partial_backspace_and_retype_resolves_to_corrected_to_suggestion() {
        // "bullon " sealed, then user backspaces "on" (two inside
        // deletes shrink the anchor to [0,4) content "bull"). Typing
        // "ion " appends past the anchor's end (all "after-ignore" for
        // the original anchor); the trailing space seals "bullion" as
        // a new anchor at [0,7).
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

        // Backspace ' ', 'n', 'o'.
        anchors.apply_delete(6, ' '); // after-ignore (no merge partner)
        line.remove(6);
        anchors.apply_delete(5, 'n'); // inside [0,6) → end=5
        line.remove(5);
        anchors.apply_delete(4, 'o'); // inside [0,5) → end=4
        line.remove(4);
        // Now line "bull", original anchor [0,4) Tracking content "bull".

        // Type 'i','o','n',' '. Each insert lands at p >= anchor.end=4,
        // i.e. the "after-ignore" branch — original anchor doesn't grow.
        anchors.apply_insert(4, 'i');
        line.insert(4, 'i');
        anchors.apply_insert(5, 'o');
        line.insert(5, 'o');
        anchors.apply_insert(6, 'n');
        line.insert(6, 'n');
        anchors.apply_insert(7, ' ');
        line.insert(7, ' ');

        // Trailing space seals "bullion" as a NEW anchor at [0,7).
        let _new_aid = anchors.try_register(0, 7, "bullion").unwrap();

        let original = anchors.anchors().iter().find(|a| a.id == aid).unwrap();
        assert_eq!(
            (original.start, original.end),
            (0, 4),
            "original anchor stays at [0,4) — its end never grew back"
        );

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(
            changes,
            vec![(rid, Outcome::CorrectedToSuggestion)],
            "successor at original.start=0 must find the new anchor's content 'bullion'"
        );
    }

    #[test]
    fn full_backspace_and_retype_resolves_to_corrected_to_other() {
        // Type "bullon ", backspace everything, retype "hello ". Original
        // anchor voids `Deleted` at [0,0); "hello" seals at [0,5). The
        // resolver's successor lookup at frozen anchor.start=0 finds
        // "hello" and classifies it as ToOther (hello ≠ original
        // "bullon" and ≠ candidate "bullion").
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

        // Backspace ' ' (after-ignore) then 'n','o','l','l','u','b'
        // (six inside-deletes, last one voids the anchor).
        anchors.apply_delete(6, ' ');
        line.remove(6);
        for (p, c) in [(5, 'n'), (4, 'o'), (3, 'l'), (2, 'l'), (1, 'u'), (0, 'b')] {
            anchors.apply_delete(p, c);
            line.remove(p);
        }
        let original = anchors.anchors().iter().find(|a| a.id == aid).unwrap();
        assert!(matches!(
            original.state,
            AnchorState::Void {
                reason: VoidReason::Deleted
            }
        ));
        assert_eq!(
            (original.start, original.end),
            (0, 0),
            "void freezes the anchor's start at the position the word died — \
             the resolver's reference point for successor lookup"
        );

        // Retype "hello ". Each insert at p<=0 would shift a Tracking
        // anchor, but the original is Void and C2 leaves it frozen —
        // exactly what the resolver needs to find the replacement.
        for (i, c) in "hello ".chars().enumerate() {
            anchors.apply_insert(i, c);
            line.insert(i, c);
        }
        // Trailing space seals "hello" at [0,5) → new anchor B.
        let _new_aid = anchors.try_register(0, 5, "hello").unwrap();

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(
            changes,
            vec![(rid, Outcome::CorrectedToOther)],
            "successor at frozen anchor.start=0 must find 'hello' across the void"
        );
    }

    #[test]
    fn delete_in_place_waits_for_move_on_then_resolves_abandoned() {
        // Pins the keystroke-gated trigger semantics that surfaced in
        // manual testing: emptying a span doesn't itself fire a
        // resolution — the resolver only acts on subsequent ticks past
        // the debounce window. "Delete in place" (no further tick) =
        // record stays at its previous outcome (Pending here). "Move
        // on" (a later tick) = Abandoned fires.
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

        // Backspace everything — the "delete in place" moment.
        anchors.apply_delete(6, ' ');
        line.remove(6);
        for (p, c) in [(5, 'n'), (4, 'o'), (3, 'l'), (2, 'l'), (1, 'u'), (0, 'b')] {
            anchors.apply_delete(p, c);
            line.remove(p);
        }
        assert!(line.is_empty());

        let mut r = OutcomeResolver::with_debounce_ms(100);
        // First tick — the keystroke that completed the delete. Stamps
        // the new observation; elapsed=0 → no transition. The record
        // stays Pending: the user has emptied the span but not yet
        // "moved on."
        let changes = r.tick(0, anchors.anchors(), &line, &ledger);
        assert!(
            changes.is_empty(),
            "delete-in-place must not resolve at the deletion tick — that's the wait-for-move-on contract"
        );
        assert_eq!(ledger.get(rid).unwrap().outcome, Outcome::Pending);

        // Still inside the debounce window — no move-on yet.
        let changes = r.tick(50, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty());
        assert_eq!(ledger.get(rid).unwrap().outcome, Outcome::Pending);

        // Move-on: a later tick past the debounce window. Models any
        // subsequent keystroke (typing somewhere else, hitting return,
        // anything that wakes the resolver). Abandoned fires.
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Abandoned)]);
    }

    #[test]
    fn whole_word_deleted_with_empty_line_resolves_to_abandoned() {
        // Same as the full-backspace setup but with no retype — line is
        // empty after the deletes. Successor lookup finds nothing →
        // Abandoned.
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Abandoned)]);
    }

    // ---- Mid-edit incomplete state (wordl → world fix) -----------------

    #[test]
    fn wordl_to_world_via_transposition_resolves_to_corrected_to_suggestion() {
        // The reported real-world failure. User types "wordl ", record
        // logged with top_candidate="world" (LeaveAlone(BelowActiveTier)
        // under Cautious). Then corrects the transposition by
        // backspacing 'l' and 'd' (anchor shrinks via inside-deletes
        // to [0,3) "wor") and retyping 'l', 'd', ' ' (each insert is
        // "after-ignore" so the original anchor stays at [0,3); the
        // trailing space seals "world" as a separate anchor B at
        // [0,5)). The successor lookup at A.start=0 must find B and
        // classify CorrectedToSuggestion.
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

        // Backspace ' ' (after-ignore), 'l' (inside), 'd' (inside).
        anchors.apply_delete(5, ' ');
        line.remove(5);
        anchors.apply_delete(4, 'l');
        line.remove(4);
        anchors.apply_delete(3, 'd');
        line.remove(3);
        let original = anchors.anchors().iter().find(|a| a.id == aid).unwrap();
        assert_eq!(
            (original.start, original.end),
            (0, 3),
            "anchor shrinks to [0,3) 'wor'"
        );

        // Type 'l', 'd', ' '. Each insert is "after-ignore" for A — A
        // stays [0,3); the trailing space seals "world" as a new anchor.
        anchors.apply_insert(3, 'l');
        line.insert(3, 'l');
        anchors.apply_insert(4, 'd');
        line.insert(4, 'd');
        anchors.apply_insert(5, ' ');
        line.insert(5, ' ');
        let new_aid = anchors.try_register(0, 5, "world").unwrap();
        assert_ne!(new_aid, aid, "new anchor for 'world' is a separate id");

        // Confirm the engine-observable state matches the panel
        // description: TWO Tracking anchors at start=0.
        let at_start_0: Vec<&SpanAnchor> = anchors
            .anchors()
            .iter()
            .filter(|a| matches!(a.state, AnchorState::Tracking) && a.start == 0)
            .collect();
        assert_eq!(at_start_0.len(), 2);

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(
            changes,
            vec![(rid, Outcome::CorrectedToSuggestion)],
            "successor lookup must pick B (largest end at start=0) and \
             classify against the stored candidate"
        );
    }

    #[test]
    fn truncated_mid_edit_does_not_prematurely_resolve_to_other() {
        // The pause-during-correction case from the wordl trace: user
        // shrinks the anchor to a proper prefix, pauses past debounce,
        // and never commits. The resolver must NOT fire ToOther on the
        // truncated stub — record stays at its previous outcome.
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        // Phase 1: resolve Kept on the untouched word.
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Phase 2: backspace ' ', 'l', 'd' — anchor truncates to "wor".
        anchors.apply_delete(5, ' ');
        line.remove(5);
        anchors.apply_delete(4, 'l');
        line.remove(4);
        anchors.apply_delete(3, 'd');
        line.remove(3);

        // Pause past debounce — observation is Incomplete("wor"), so
        // no transition fires. Record stays Kept (its prior outcome).
        r.tick(200, anchors.anchors(), &line, &ledger);
        let changes = r.tick(500, anchors.anchors(), &line, &ledger);
        assert!(
            changes.is_empty(),
            "Incomplete mid-edit must not flip Kept → ToOther"
        );
        assert_eq!(ledger.get(rid).unwrap().outcome, Outcome::Kept);
    }

    #[test]
    fn truncated_mid_edit_resolves_when_successor_seals() {
        // Continuation of the previous case: after the truncation, the
        // user retypes and seals a successor. Past the next debounce
        // the resolver fires the correct transition (Kept → ToSuggestion
        // here).
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Truncate to "wor" — Incomplete observation, no transition.
        anchors.apply_delete(5, ' ');
        line.remove(5);
        anchors.apply_delete(4, 'l');
        line.remove(4);
        anchors.apply_delete(3, 'd');
        line.remove(3);
        r.tick(200, anchors.anchors(), &line, &ledger);
        let changes = r.tick(400, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty(), "Incomplete should hold the resolution");

        // Commit: type 'l', 'd', ' ' — trailing space seals "world".
        anchors.apply_insert(3, 'l');
        line.insert(3, 'l');
        anchors.apply_insert(4, 'd');
        line.insert(4, 'd');
        anchors.apply_insert(5, ' ');
        line.insert(5, ' ');
        anchors.try_register(0, 5, "world").unwrap();

        // First tick after the seal — observation changed from
        // Incomplete("wor") to Resolved("world") via successor lookup.
        // stable_since resets to this tick.
        r.tick(500, anchors.anchors(), &line, &ledger);
        // Past the post-commit debounce: flip Kept → ToSuggestion.
        let changes = r.tick(650, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToSuggestion)]);
    }

    // ---- Revisable transitions (Component 5a's "latest write wins") ----
    //
    // These pin the two transitions that scrambled in manual debug-panel
    // testing (likely premature debounce firing during slow editing).
    // Each test resolves once, applies the next phase of edits, and
    // asserts the SECOND transition fires — which is the contract.

    #[test]
    fn revisable_kept_then_full_delete_re_resolves_to_abandoned() {
        // Phase 1: type "bullon ", record resolves Kept after debounce.
        // Phase 2: backspace everything, anchor voids Deleted, record
        // re-resolves Kept → Abandoned.
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        // Phase 1 — resolve Kept on untouched word.
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)], "phase 1: Kept");
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Phase 2 — full backspace through ' ' (after-ignore) and the
        // six letters (six inside-deletes, the last voids).
        anchors.apply_delete(6, ' ');
        line.remove(6);
        for (p, c) in [(5, 'n'), (4, 'o'), (3, 'l'), (2, 'l'), (1, 'u'), (0, 'b')] {
            anchors.apply_delete(p, c);
            line.remove(p);
        }
        let original = anchors.anchors().iter().find(|a| a.id == aid).unwrap();
        assert!(matches!(
            original.state,
            AnchorState::Void {
                reason: VoidReason::Deleted
            }
        ));
        assert!(line.is_empty());

        // First tick after the deletes — observation changed
        // (Tracking,"bullon") → (Void,empty). stable_since resets.
        let changes = r.tick(200, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty(), "debounce not yet elapsed after delete");

        // Past debounce: record revises from Kept → Abandoned.
        let changes = r.tick(350, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Abandoned)]);
    }

    #[test]
    fn revisable_abandoned_then_retype_re_resolves_to_corrected_to_other() {
        // Phase 1: type "bullon ", backspace it entirely, record
        // resolves Abandoned. Phase 2: retype "hello " — successor B
        // seals at [0,5). Record re-resolves Abandoned → ToOther.
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

        // Phase 1: backspace ' ' then six letters; void the anchor.
        anchors.apply_delete(6, ' ');
        line.remove(6);
        for (p, c) in [(5, 'n'), (4, 'o'), (3, 'l'), (2, 'l'), (1, 'u'), (0, 'b')] {
            anchors.apply_delete(p, c);
            line.remove(p);
        }
        assert!(line.is_empty());

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Abandoned)], "phase 1: Abandoned");
        assert!(ledger.resolve_outcome(rid, Outcome::Abandoned));

        // Phase 2: type "hello ". C2 ignores the Void anchor on each
        // insert (Void anchors don't track). Trailing space seals
        // "hello" at [0,5) as a new Tracking anchor B — the successor.
        for (i, c) in "hello ".chars().enumerate() {
            anchors.apply_insert(i, c);
            line.insert(i, c);
        }
        let _new_aid = anchors.try_register(0, 5, "hello").unwrap();

        // First tick after the retype — observation went from
        // (Void,empty) to (Void,"hello"). stable_since resets.
        let changes = r.tick(200, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty(), "debounce not yet elapsed after retype");

        // Past debounce: "hello" ≠ original "bullon" ≠ candidate
        // "bullion" → record revises from Abandoned → CorrectedToOther.
        let changes = r.tick(350, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    // ---- Position shift vs content edit --------------------------------

    #[test]
    fn position_shift_only_does_not_flip_kept() {
        // Two words on the line: "teh foo". Anchor for "teh" at [0,3).
        // After it resolves Kept, the user types 'x' at the START of
        // the line — "teh" shifts right to [1,4) but its content is
        // unchanged. The Kept resolution must persist.
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // User inserts 'x' at p=0. Anchor for "teh" shifts to [1,4);
        // content at [1,4) is still "teh".
        anchors.apply_insert(0, 'x');
        line.insert(0, 'x');

        // Far beyond the debounce — should remain Kept, with no
        // transition emitted (the resolver short-circuits because
        // computed == record.outcome).
        let changes = r.tick(10_000, anchors.anchors(), &line, &ledger);
        assert!(
            changes.is_empty(),
            "shift-only must not re-resolve a Kept record"
        );
    }

    // ---- Revisability ---------------------------------------------------

    #[test]
    fn kept_record_re_resolves_when_span_is_later_edited() {
        // "teh " resolves Kept. Then user edits the span content to
        // "the" via the same in-place trick used in the
        // pending_resolves_to_corrected_to_suggestion path. Resolver
        // must re-fire and re-resolve to CorrectedToSuggestion.
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Mutate the anchor's span content to "the" via insert-inside
        // / delete-tail (apply_insert at p == end is "after, ignore" —
        // we need the insert strictly inside the span to grow it).
        anchors.apply_insert(1, 'h');
        line.insert(1, 'h');
        anchors.apply_delete(3, line[3]);
        line.remove(3);
        // Anchor is now [0,3) Tracking with content "the".

        // First post-edit tick: observation changed → debounce resets.
        let changes = r.tick(200, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty(), "debounce not yet elapsed");

        // Past the debounce window: Kept → CorrectedToSuggestion.
        let changes = r.tick(350, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToSuggestion)]);
    }

    // ---- Edge cases ------------------------------------------------------

    #[test]
    fn no_candidates_record_with_unchanged_content_resolves_to_kept() {
        // Names like "Soumyo" land on LeaveAlone(NoCandidates) — record
        // has no top_candidate. Untouched text → Kept regardless.
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
    }

    #[test]
    fn no_candidates_record_with_changed_content_resolves_to_corrected_to_other() {
        // Same Soumyo scenario with `top_candidate = None`: any change
        // is CorrectedToOther (ToSuggestion is unreachable).
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "Soumyo").unwrap();
        // Replicate the partial-edit shape: drop the anchor's end so a
        // successor at start=0 can win the lookup.
        anchors.apply_delete(5, 'o');
        anchors.apply_delete(4, 'y');
        let _new = anchors.try_register(0, 6, "Saumyo").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = log_with_candidate(
            &mut ledger,
            leave_alone("Soumyo", LeaveAloneReason::NoCandidates),
            aid,
            None,
            None,
        );
        let line: Vec<char> = "Saumyo ".chars().collect();

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    #[test]
    fn debounce_resets_on_content_edit_then_commit_seals_resolves_to_other() {
        // Two sequential inside-deletes shrink "teh" → "te" → "t". Each
        // observation changes (`Incomplete("te")` ≠ `Incomplete("t")`)
        // so each delete resets the debounce timer. Past the debounce,
        // the record DOES NOT resolve while truncated (the mid-edit
        // gate from the wordl→world fix). Only once the user commits
        // by typing a boundary — sealing "t" as a successor anchor —
        // does the resolver classify CorrectedToOther.
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(0));

        // t=50: delete 'h' at p=2 → anchor [0,2), content "te" (prefix
        // of "teh"). Observation transitions from Resolved("teh") to
        // Incomplete("te") — different observation, timer resets.
        anchors.apply_delete(2, 'h');
        line.remove(2);
        r.tick(50, anchors.anchors(), &line, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(50));

        // t=80: delete 'e' at p=1 → anchor [0,1), content "t" (also a
        // prefix). Incomplete("te") ≠ Incomplete("t") → timer resets.
        anchors.apply_delete(1, 'e');
        line.remove(1);
        r.tick(80, anchors.anchors(), &line, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(80));

        // t=130 — 50ms after last edit, still inside debounce.
        let changes = r.tick(130, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty());

        // t=200 — past debounce, but observation is still Incomplete →
        // no transition. The record stays Pending.
        let changes = r.tick(200, anchors.anchors(), &line, &ledger);
        assert!(
            changes.is_empty(),
            "Incomplete must not fire ToOther on a truncated mid-edit"
        );
        assert_eq!(ledger.get(rid).unwrap().outcome, Outcome::Pending);

        // Commit: user types ' ' which seals "t" as a successor anchor
        // at [0,1). Now there's a Tracking neighbour at the original
        // start — observation flips Incomplete → Resolved("t") via the
        // successor lookup. Timer resets to t=210, then we wait again.
        anchors.apply_insert(1, ' ');
        line.insert(1, ' ');
        let _new_aid = anchors.try_register(0, 1, "t").unwrap();
        r.tick(210, anchors.anchors(), &line, &ledger);

        // Past the post-commit debounce: classify "t" → ToOther.
        let changes = r.tick(320, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        assert!(r.stable_since_ms(aid).is_some());

        anchors.clear();
        line.clear();
        r.tick(1, anchors.anchors(), &line, &ledger);
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

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        let changes = r.tick(200, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty());
        let changes = r.tick(1_000, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty());
    }

    // ---- Pure classifier ------------------------------------------------

    fn resolved(s: &str) -> PostEdit {
        PostEdit::Resolved(s.chars().collect())
    }

    fn incomplete(s: &str) -> PostEdit {
        PostEdit::Incomplete(s.chars().collect())
    }

    #[test]
    fn classify_kept_path() {
        assert_eq!(
            classify(&resolved("teh"), "teh", Some("the")),
            Some(Outcome::Kept)
        );
    }

    #[test]
    fn classify_corrected_to_suggestion_path() {
        assert_eq!(
            classify(&resolved("the"), "teh", Some("the")),
            Some(Outcome::CorrectedToSuggestion)
        );
    }

    #[test]
    fn classify_corrected_to_other_path() {
        assert_eq!(
            classify(&resolved("tax"), "teh", Some("the")),
            Some(Outcome::CorrectedToOther)
        );
    }

    #[test]
    fn classify_abandoned_on_empty() {
        assert_eq!(
            classify(&resolved(""), "teh", Some("the")),
            Some(Outcome::Abandoned)
        );
    }

    #[test]
    fn classify_corrected_to_other_when_no_candidate_present() {
        // top_candidate=None: anything non-empty that doesn't match the
        // original is ToOther — ToSuggestion is unreachable by design.
        assert_eq!(
            classify(&resolved("tax"), "teh", None),
            Some(Outcome::CorrectedToOther)
        );
    }

    #[test]
    fn classify_incomplete_returns_none() {
        // Incomplete state never emits a transition — record stays at
        // its prior outcome until the user commits.
        assert_eq!(classify(&incomplete("wor"), "wordl", Some("world")), None);
        // Even when the truncated prefix matches a suggestion-like
        // shape, Incomplete dominates — we don't classify mid-edit.
        assert_eq!(classify(&incomplete("the"), "the", Some("the")), None);
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
}
