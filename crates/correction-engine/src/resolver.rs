//! Outcome resolver — Component 5a of the L4 Observing brief.
//!
//! Watches the live [`crate::AnchorTracker`] state for each Pending
//! [`crate::log::LogRecord`] and resolves the outcome (`Kept`,
//! `CorrectedToSuggestion`, `CorrectedToOther`, `Abandoned`). Still
//! **observe-only** — the resolver writes back through
//! [`crate::log::DecisionLedger::resolve_outcome`] and surfaces the
//! transitions on the panel; it does NOT learn, persist, or inject.
//!
//! ## Resolution policy
//!
//! Two gates must both pass before a record resolves:
//!
//! 1. **Caret moved past the word.** The token is only sealed (and a
//!    record only appended) when a boundary character — space, newline,
//!    punctuation — follows the word. By that point the caret is already
//!    past the original span, so this gate is satisfied **by construction**
//!    when the record is created. Subsequent revisability events
//!    (user goes back, edits, leaves again) re-arm the debounce below;
//!    we don't separately re-check caret position.
//! 2. **Span stable for [`DEFAULT_DEBOUNCE_MS`].** The resolver tracks
//!    the anchor's *state + content* (deliberately NOT its position —
//!    see the position-shift caveat below). Each tick that observes a
//!    change resets the stable-since timestamp. A record only resolves
//!    once the debounce elapses without further change.
//!
//! ## Position shift vs. content edit
//!
//! Critical distinction baked into the observation comparator: editing
//! ELSEWHERE on the line shifts an anchor's `start`/`end` but **does not
//! touch its content** — `line_buf[start..end]` still spells the original
//! word. We deliberately strip `start`/`end` out of the observation so a
//! pure shift never resets the debounce and never re-resolves a record.
//! The same span position re-arming would otherwise produce false
//! "corrected" reads when the user simply typed something else on the
//! line — the C2 anchor's edit-delta already encodes shift vs. resize vs.
//! void, and the resolver leans on that contract.
//!
//! ## Classification
//!
//! Given a stable observation:
//!
//! | Anchor state                | Content read           | Outcome                 |
//! |-----------------------------|------------------------|-------------------------|
//! | `Tracking`                  | `== original_text`     | `Kept`                  |
//! | `Tracking`                  | `== top_candidate`     | `CorrectedToSuggestion` |
//! | `Tracking`                  | something else         | `CorrectedToOther`      |
//! | `Void(Deleted)`             | (no content)           | `Abandoned`             |
//! | `Void(Split)` / `Void(Merge)` | (unreadable)         | `CorrectedToOther`      |
//!
//! `Void(Split)` and `Void(Merge)` are the awkward middle: the original
//! word boundary is gone, so there's no coherent single-word read; but
//! the user clearly didn't keep the text as-is, and we can't claim a
//! match against the suggestion. We collapse them into `CorrectedToOther`
//! as the honest catch-all (see [`crate::log::Outcome::CorrectedToOther`]
//! doc-comment).
//!
//! Match is **case-sensitive exact** between content chars and
//! `original_text` / `top_candidate`. C5a doesn't try to normalise case;
//! the lexicon's case-insensitive lookups happen upstream when the
//! candidate is generated. Refine in C5b if real usage shows we need it.
//!
//! ## Revisability
//!
//! A resolved record re-resolves if the anchor is later touched:
//! observation change → debounce reset → re-classify. The resolver
//! compares the newly computed outcome against the record's CURRENT
//! outcome in the ledger and only emits a transition when they differ.
//! Once the anchor is retired (line reset → `anchors.clear()`), the
//! record's resolver entry is GC'd on the next tick and the last
//! resolution stands forever.

use std::collections::HashMap;

use crate::anchor::{AnchorState, SpanAnchor, VoidReason};
use crate::log::{DecisionLedger, Outcome};

/// Default debounce — short enough to feel live in the debug panel, long
/// enough to swallow normal mid-word pauses (a typist hovering at ~3 cps
/// has ~330 ms between keystrokes). Tune from real usage data.
pub const DEFAULT_DEBOUNCE_MS: u64 = 600;

/// Resolver version. Bump on any change to the resolution policy that
/// downstream Components or the debug panel could observe.
pub const RESOLVER_VERSION: u32 = 1;

/// What the resolver remembers about an anchor at last observation. We
/// deliberately exclude `start` / `end` so a pure position shift (caused
/// by an edit elsewhere on the line) does NOT reset the debounce — see
/// module doc-comment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AnchorObservation {
    state: AnchorState,
    /// Char-slice of `line_buf[start..end]` for a Tracking anchor;
    /// empty for Void anchors (their content can't be reliably read
    /// from the current line — the original boundary is gone).
    content: Vec<char>,
}

#[derive(Debug, Clone)]
struct Observed {
    obs: AnchorObservation,
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
    /// observed that anchor since the last GC). Useful in tests to pin
    /// debounce behaviour.
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

        // Refresh per-anchor observations.
        for anchor in anchors {
            let obs = observe(anchor, line_buf);
            match self.last_seen.get_mut(&anchor.id) {
                Some(stored) if stored.obs == obs => {
                    // Stable — do not reset the timer.
                }
                Some(stored) => {
                    stored.obs = obs;
                    stored.stable_since_ms = now_ms;
                }
                None => {
                    self.last_seen.insert(
                        anchor.id,
                        Observed {
                            obs,
                            stable_since_ms: now_ms,
                        },
                    );
                }
            }
        }

        // Walk the ledger and queue any transitions whose anchor has
        // been stable for at least the debounce window. We compare the
        // newly computed outcome to the ledger's CURRENT outcome so
        // revisited records flip back when their content changes again.
        let mut changes = Vec::new();
        for record in ledger.iter() {
            let Some(anchor) = anchors.iter().find(|a| a.id == record.anchor_id) else {
                continue;
            };
            let Some(stored) = self.last_seen.get(&anchor.id) else {
                continue;
            };
            // Saturating to guard against a non-monotonic `now_ms`
            // (test fixtures, time skew). A negative delta would
            // otherwise wrap into a huge u64 and short-circuit the
            // debounce — keep it pinned at 0 so the debounce holds.
            let elapsed = now_ms.saturating_sub(stored.stable_since_ms);
            if elapsed < self.debounce_ms {
                continue;
            }
            let computed = classify(
                &stored.obs,
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

/// Read the live observation for an anchor. Void anchors have no
/// content read — see module doc-comment for why.
fn observe(anchor: &SpanAnchor, line_buf: &[char]) -> AnchorObservation {
    let content = match anchor.state {
        AnchorState::Tracking => {
            // Defensive bounds-check: a healthy AnchorTracker keeps
            // start/end within line_buf, but if it ever drifts (engine
            // bug) we'd rather see an empty observation than panic.
            if anchor.end <= line_buf.len() && anchor.start <= anchor.end {
                line_buf[anchor.start..anchor.end].to_vec()
            } else {
                Vec::new()
            }
        }
        AnchorState::Void { .. } => Vec::new(),
    };
    AnchorObservation {
        state: anchor.state.clone(),
        content,
    }
}

/// Pure classifier. See the table in the module doc-comment.
fn classify(obs: &AnchorObservation, original: &str, top_candidate: Option<&str>) -> Outcome {
    match obs.state {
        AnchorState::Void {
            reason: VoidReason::Deleted,
        } => Outcome::Abandoned,
        AnchorState::Void {
            reason: VoidReason::Split,
        }
        | AnchorState::Void {
            reason: VoidReason::Merge,
        } => Outcome::CorrectedToOther,
        AnchorState::Tracking => {
            let now: String = obs.content.iter().collect();
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
    }
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchor::AnchorTracker;
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

    /// Build a tiny world: an AnchorTracker holding one Word at the
    /// given span, and a DecisionLedger holding one Pending record
    /// for it. Returns the linkage so a test can drive edits.
    fn world_one_word(
        line_chars: &str,
        word_start: usize,
        word_end: usize,
        decision: DecisionOutcome,
    ) -> (Vec<char>, AnchorTracker, DecisionLedger, u32, u64) {
        let line: Vec<char> = line_chars.chars().collect();
        assert!(word_end <= line.len());
        let core: String = line[word_start..word_end].iter().collect();
        let mut anchors = AnchorTracker::new();
        let anchor_id = anchors.try_register(word_start, word_end, &core).unwrap();
        let mut ledger = DecisionLedger::new();
        let record_id = ledger.append(
            0,
            decision,
            anchor_id,
            ConfidenceTier::Eager,
            Some(Confidence::High),
        );
        (line, anchors, ledger, anchor_id, record_id)
    }

    // ---- Core outcomes --------------------------------------------------

    #[test]
    fn pending_resolves_to_kept_when_word_is_untouched() {
        // Type "teh ", token seals, anchor [0,3) for "teh". User does
        // not touch the word again. After the debounce elapses, the
        // record resolves to Kept.
        let (line, anchors, ledger, _aid, rid) =
            world_one_word("teh ", 0, 3, would_correct("teh", "the", 0.82));

        let mut r = OutcomeResolver::with_debounce_ms(100);
        // First tick at t=0 stamps the stable-since timestamp.
        let changes = r.tick(0, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty(), "no resolution before debounce");
        // Tick again after the debounce window: now → Kept.
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
    }

    #[test]
    fn pending_resolves_to_corrected_to_suggestion_on_content_match() {
        // User backspaces "teh" and retypes "the". Engine-side this
        // involves keystroke replays and tokenizer rebuilds; here we
        // drive the anchor through edits that leave its [start, end)
        // span Tracking with current content "the":
        //
        //   "teh " ── insert 'h' at p=1 ──→ "theh "   (anchor grows to [0,4))
        //   "theh " ── delete tail 'h' at p=3 ──→ "the "  (anchor shrinks to [0,3))
        //
        // The insert-inside / delete-tail trick keeps a single live
        // anchor whose final content is the suggestion. This is the
        // observable state the engine ends up in once a real correction
        // settles, with the dance of tokenizer rebuilds handled
        // upstream (the resolver only inspects the final state at tick).
        let (mut line, mut anchors, mut ledger, _aid, rid) =
            world_one_word("teh ", 0, 3, would_correct("teh", "the", 0.82));

        anchors.apply_insert(1, 'h');
        line.insert(1, 'h');
        anchors.apply_delete(3, line[3]);
        line.remove(3);

        // Sanity-check the world we built: anchor [0,3) Tracking with
        // content "the". If anchor math drifts, fail loudly here so the
        // resolver assertion below isn't blamed for an upstream bug.
        let a = anchors
            .anchors()
            .iter()
            .find(|a| a.original_core == "teh")
            .unwrap();
        assert_eq!((a.start, a.end), (0, 3));
        assert!(matches!(a.state, AnchorState::Tracking));
        let slice: String = line[a.start..a.end].iter().collect();
        assert_eq!(slice, "the");

        // Suppress unused-write warning on ledger (used through &ledger below).
        let _ = &mut ledger;

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToSuggestion)]);
    }

    #[test]
    fn pending_resolves_to_corrected_to_other_when_content_differs() {
        // Anchor over "tax", record says original="teh" / suggested="the".
        // Content matches neither original ("teh") nor suggestion ("the"),
        // so resolution must land on CorrectedToOther.
        let mut ledger = DecisionLedger::new();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let rid = ledger.append(
            0,
            would_correct("teh", "the", 0.82),
            aid,
            ConfidenceTier::Eager,
            Some(Confidence::High),
        );
        let line: Vec<char> = "tax ".chars().collect();

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    #[test]
    fn whole_word_deleted_resolves_to_abandoned() {
        // Type "teh ", then backspace the trailing space and all three
        // letters. Anchor walks to Void(Deleted).
        let mut line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = ledger.append(
            0,
            would_correct("teh", "the", 0.82),
            aid,
            ConfidenceTier::Eager,
            Some(Confidence::High),
        );

        // Delete from the right.
        anchors.apply_delete(3, line[3]); // space; not boundary-merge (no word after)
        line.remove(3);
        anchors.apply_delete(2, line[2]); // 'h'
        line.remove(2);
        anchors.apply_delete(1, line[1]); // 'e'
        line.remove(1);
        anchors.apply_delete(0, line[0]); // 'h' — wait, line[0] now 't'
        line.remove(0);

        // Confirm the anchor voided Deleted.
        let a = anchors.anchors().iter().find(|a| a.id == aid).unwrap();
        assert!(matches!(
            a.state,
            AnchorState::Void {
                reason: VoidReason::Deleted
            }
        ));

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
        let rid = ledger.append(
            0,
            would_correct("teh", "the", 0.82),
            teh,
            ConfidenceTier::Eager,
            Some(Confidence::High),
        );

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        // Apply the resolution back to the ledger so the next tick sees
        // the record's outcome as Kept.
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Now: user inserts 'x' at p=0. Anchor for "teh" shifts to
        // [1,4); content at [1,4) is still "teh".
        anchors.apply_insert(0, 'x');
        line.insert(0, 'x');

        // Tick (well past the debounce). The stored observation for
        // "teh" is (Tracking, content=['t','e','h']); the new
        // observation is the same — debounce timer must NOT reset.
        // No transition because the record is already Kept and the
        // computed outcome is still Kept.
        let changes = r.tick(300, anchors.anchors(), &line, &ledger);
        assert!(
            changes.is_empty(),
            "shift-only must not re-resolve a Kept record"
        );

        // And even much later, still Kept.
        let changes = r.tick(10_000, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty());
    }

    // ---- Revisability ---------------------------------------------------

    #[test]
    fn kept_record_re_resolves_to_corrected_when_span_is_later_edited() {
        // User types "teh ", record resolves Kept after debounce. Then
        // they go back and edit it to "the". The resolver must
        // re-fire and re-resolve to CorrectedToSuggestion.
        let mut line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = ledger.append(
            0,
            would_correct("teh", "the", 0.82),
            aid,
            ConfidenceTier::Eager,
            Some(Confidence::High),
        );

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Now mutate the span content to "the" using the insert-inside /
        // delete-tail trick (see pending_resolves_to_corrected_to_suggestion
        // for why these specific operations and not "delete 'e', insert
        // 'e' at end" — apply_insert at p==end is the "after, ignore"
        // branch, so the anchor wouldn't grow).
        anchors.apply_insert(1, 'h');
        line.insert(1, 'h');
        anchors.apply_delete(3, line[3]);
        line.remove(3);

        // First tick after the edits: observation changed → debounce
        // timer resets at t=200, no resolution yet.
        let changes = r.tick(200, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty(), "debounce not yet elapsed");

        // Past the debounce window from the edit: Kept → CorrectedToSuggestion.
        let changes = r.tick(350, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToSuggestion)]);
    }

    // ---- Edge cases ------------------------------------------------------

    #[test]
    fn no_candidates_record_with_unchanged_content_resolves_to_kept() {
        // Names like "Soumyo" land on LeaveAlone(NoCandidates) — record
        // has no top_candidate. As long as the text stays put, it must
        // resolve to Kept (the engine's leave-alone was correct).
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "Soumyo").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = ledger.append(
            0,
            leave_alone("Soumyo", LeaveAloneReason::NoCandidates),
            aid,
            ConfidenceTier::Eager,
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
        // Same Soumyo scenario, but the user edits it. With no
        // top_candidate, ToSuggestion is unreachable — any change is
        // CorrectedToOther.
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 6, "Soumyo").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = ledger.append(
            0,
            leave_alone("Soumyo", LeaveAloneReason::NoCandidates),
            aid,
            ConfidenceTier::Eager,
            None,
        );
        let line: Vec<char> = "Saumyo ".chars().collect();

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    #[test]
    fn debounce_resets_on_in_span_edit() {
        // Apply content edits before the debounce window completes —
        // the timer must reset on each so we don't snap to an outcome
        // computed from a half-formed edit. Two sequential deletes
        // here (shrinking "teh" → "te" → "t") since each one changes
        // the observation. The final content ("t") matches neither
        // original nor suggestion, so the post-debounce outcome is
        // CorrectedToOther — what matters is the timing.
        let mut line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = ledger.append(
            0,
            would_correct("teh", "the", 0.82),
            aid,
            ConfidenceTier::Eager,
            Some(Confidence::High),
        );

        let mut r = OutcomeResolver::with_debounce_ms(100);
        // First tick stamps stable_since=0 with content "teh".
        r.tick(0, anchors.anchors(), &line, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(0));

        // t=50: user deletes 'h' at p=2. Anchor [0,3) → [0,2), content "te".
        anchors.apply_delete(2, 'h');
        line.remove(2);
        r.tick(50, anchors.anchors(), &line, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(50));

        // t=80: user deletes 'e' at p=1. Anchor [0,2) → [0,1), content "t".
        anchors.apply_delete(1, 'e');
        line.remove(1);
        r.tick(80, anchors.anchors(), &line, &ledger);
        assert_eq!(r.stable_since_ms(aid), Some(80));

        // t=130 — 50ms since last edit; debounce is 100ms, still inside.
        let changes = r.tick(130, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty(), "should still be inside debounce");

        // t=200 — 120ms since last edit, past debounce. Content "t"
        // matches neither original ("teh") nor suggestion ("the") →
        // CorrectedToOther.
        let changes = r.tick(200, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    #[test]
    fn line_reset_garbage_collects_resolver_cache() {
        // After the line resets and anchors.clear() drops the anchor,
        // the resolver's cache entry for it is GC'd on the next tick.
        let mut line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let _rid = ledger.append(
            0,
            would_correct("teh", "the", 0.82),
            aid,
            ConfidenceTier::Eager,
            Some(Confidence::High),
        );

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        assert!(r.stable_since_ms(aid).is_some());

        // Line reset — engine clears anchors AND line_buf.
        anchors.clear();
        line.clear();

        r.tick(1, anchors.anchors(), &line, &ledger);
        assert!(
            r.stable_since_ms(aid).is_none(),
            "cache entry must be GC'd once the anchor is retired"
        );
    }

    #[test]
    fn does_not_re_emit_unchanged_resolution() {
        // After a record resolves to Kept and the ledger is updated,
        // subsequent ticks must NOT keep returning the same transition.
        let line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = ledger.append(
            0,
            would_correct("teh", "the", 0.82),
            aid,
            ConfidenceTier::Eager,
            Some(Confidence::High),
        );

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::Kept)]);
        assert!(ledger.resolve_outcome(rid, Outcome::Kept));

        // Tick again with no further changes — must return empty.
        let changes = r.tick(200, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty());
        let changes = r.tick(1_000, anchors.anchors(), &line, &ledger);
        assert!(changes.is_empty());
    }

    #[test]
    fn split_resolves_to_corrected_to_other() {
        // Type "teh ", then insert a space mid-word → Void(Split). The
        // original boundary is gone; the C5a policy classifies this
        // as CorrectedToOther (see module doc).
        let mut line: Vec<char> = "teh ".chars().collect();
        let mut anchors = AnchorTracker::new();
        let aid = anchors.try_register(0, 3, "teh").unwrap();
        let mut ledger = DecisionLedger::new();
        let rid = ledger.append(
            0,
            would_correct("teh", "the", 0.82),
            aid,
            ConfidenceTier::Eager,
            Some(Confidence::High),
        );

        anchors.apply_insert(1, ' ');
        line.insert(1, ' ');

        let mut r = OutcomeResolver::with_debounce_ms(100);
        r.tick(0, anchors.anchors(), &line, &ledger);
        let changes = r.tick(150, anchors.anchors(), &line, &ledger);
        assert_eq!(changes, vec![(rid, Outcome::CorrectedToOther)]);
    }

    #[test]
    fn classify_pure_function_kept_path() {
        let obs = AnchorObservation {
            state: AnchorState::Tracking,
            content: "teh".chars().collect(),
        };
        assert_eq!(classify(&obs, "teh", Some("the")), Outcome::Kept);
    }

    #[test]
    fn classify_pure_function_corrected_to_suggestion_path() {
        let obs = AnchorObservation {
            state: AnchorState::Tracking,
            content: "the".chars().collect(),
        };
        assert_eq!(
            classify(&obs, "teh", Some("the")),
            Outcome::CorrectedToSuggestion
        );
    }

    #[test]
    fn classify_pure_function_corrected_to_other_path() {
        let obs = AnchorObservation {
            state: AnchorState::Tracking,
            content: "tax".chars().collect(),
        };
        assert_eq!(
            classify(&obs, "teh", Some("the")),
            Outcome::CorrectedToOther
        );
    }

    #[test]
    fn classify_pure_function_void_paths() {
        let obs = AnchorObservation {
            state: AnchorState::Void {
                reason: VoidReason::Deleted,
            },
            content: Vec::new(),
        };
        assert_eq!(classify(&obs, "teh", Some("the")), Outcome::Abandoned);

        let obs = AnchorObservation {
            state: AnchorState::Void {
                reason: VoidReason::Split,
            },
            content: Vec::new(),
        };
        assert_eq!(
            classify(&obs, "teh", Some("the")),
            Outcome::CorrectedToOther
        );

        let obs = AnchorObservation {
            state: AnchorState::Void {
                reason: VoidReason::Merge,
            },
            content: Vec::new(),
        };
        assert_eq!(
            classify(&obs, "teh", Some("the")),
            Outcome::CorrectedToOther
        );
    }
}
