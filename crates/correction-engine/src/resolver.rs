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
//! Given the post-edit text computed from the successor lookup:
//!
//! | post-edit text                                 | Outcome                 |
//! |------------------------------------------------|-------------------------|
//! | empty (no successor + original Void)           | `Abandoned`             |
//! | == `record.original_text`                       | `Kept`                  |
//! | == `record.top_candidate`                       | `CorrectedToSuggestion` |
//! | any other non-empty word                        | `CorrectedToOther`      |
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
pub const RESOLVER_VERSION: u32 = 2;

/// What the resolver remembers about an anchor at last observation.
/// Deliberately keyed on `(state, post_edit_text)` — position is NOT
/// part of the stability tuple, so a pure shift (insert/delete
/// elsewhere on the line) doesn't reset the debounce.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    state: AnchorState,
    post_edit_text: Vec<char>,
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

            let post_edit_text = compute_post_edit_text(record, original, anchors, line_buf);
            let obs = Observation {
                state: original.state.clone(),
                post_edit_text,
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
            let computed = classify(
                &obs.post_edit_text,
                &record.original_text,
                record.top_candidate.as_deref(),
            );
            if record.outcome != computed {
                changes.push((record.id, computed));
            }
        }
        changes
    }
}

/// Compute the text the user has left at this record's position.
///
/// Algorithm:
///   1. If the original anchor is `Tracking` AND its content slice
///      equals `record.original_text` → return that content (Kept happy
///      path; short-circuits before any successor lookup).
///   2. Otherwise look for a **successor** — a `Tracking` anchor whose
///      `start` equals the original's `start`, with the largest `end`
///      (most recent / longest seal at this position), excluding the
///      original itself by id. If found, return that anchor's content
///      slice.
///   3. Otherwise, if the original is still `Tracking` (so it has a
///      live span), fall back to its content slice — captures the
///      partial-shrink-no-replacement case where the user mid-edited
///      and walked away with a truncated prefix.
///   4. Otherwise (original is `Void`, no successor at its frozen
///      position) → empty, which maps to `Abandoned`.
fn compute_post_edit_text(
    record: &crate::log::LogRecord,
    original: &SpanAnchor,
    anchors: &[SpanAnchor],
    line_buf: &[char],
) -> Vec<char> {
    if matches!(original.state, AnchorState::Tracking) {
        let content = slice_chars(line_buf, original.start, original.end);
        let orig_chars: Vec<char> = record.original_text.chars().collect();
        if content == orig_chars {
            return content;
        }
        // Content drifted on a Tracking anchor — try a successor first,
        // fall back to the (possibly truncated) live content if there
        // isn't one.
        if let Some(succ) = find_successor(original.id, original.start, anchors, line_buf) {
            return succ;
        }
        return content;
    }

    // Void path: anchor.start is frozen at the void position. An
    // immediate retype seals a new token at this position.
    find_successor(original.id, original.start, anchors, line_buf).unwrap_or_default()
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

/// Pure classifier. See the table in the module doc-comment.
fn classify(post_edit_text: &[char], original: &str, top_candidate: Option<&str>) -> Outcome {
    if post_edit_text.is_empty() {
        return Outcome::Abandoned;
    }
    let now: String = post_edit_text.iter().collect();
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
    fn debounce_resets_on_content_edit() {
        // Two sequential deletes — each one changes the observation
        // (different post_edit_text), each one must reset the timer.
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

        // t=50: delete 'h' at p=2 → anchor [0,2), content "te".
        anchors.apply_delete(2, 'h');
        line.remove(2);
        r.tick(50, anchors.anchors(), &line, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(50));

        // t=80: delete 'e' at p=1 → anchor [0,1), content "t".
        anchors.apply_delete(1, 'e');
        line.remove(1);
        r.tick(80, anchors.anchors(), &line, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(80));

        // t=130 — 50ms after last edit, still inside debounce.
        let changes = r.tick(130, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty());

        // t=200 — past debounce. "t" ≠ original "teh", ≠ candidate
        // "the". No other anchor at start=0 → fallback to truncated
        // original content "t" → ToOther.
        let changes = r.tick(200, anchors.anchors(), &line, &ledger);
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

    #[test]
    fn classify_kept_path() {
        let text: Vec<char> = "teh".chars().collect();
        assert_eq!(classify(&text, "teh", Some("the")), Outcome::Kept);
    }

    #[test]
    fn classify_corrected_to_suggestion_path() {
        let text: Vec<char> = "the".chars().collect();
        assert_eq!(
            classify(&text, "teh", Some("the")),
            Outcome::CorrectedToSuggestion
        );
    }

    #[test]
    fn classify_corrected_to_other_path() {
        let text: Vec<char> = "tax".chars().collect();
        assert_eq!(
            classify(&text, "teh", Some("the")),
            Outcome::CorrectedToOther
        );
    }

    #[test]
    fn classify_abandoned_on_empty() {
        let text: Vec<char> = Vec::new();
        assert_eq!(classify(&text, "teh", Some("the")), Outcome::Abandoned);
    }

    #[test]
    fn classify_corrected_to_other_when_no_candidate_present() {
        // top_candidate=None: anything non-empty that doesn't match the
        // original is ToOther — ToSuggestion is unreachable by design.
        let text: Vec<char> = "tax".chars().collect();
        assert_eq!(classify(&text, "teh", None), Outcome::CorrectedToOther);
    }
}
