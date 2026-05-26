//! Decision policy — Component 3c-2 + 3c-3 of the L4 brief.
//!
//! Turns a [`ConfidenceReport`] from [`crate::score`] into an actionable
//! outcome: either we **would** propose a correction (with the candidate's
//! [`Confidence`] and score), or we leave the word alone (with a reason).
//! This pass is still **observe-only**: callers may render the outcome
//! but must NOT inject anything yet (applying corrections is a later
//! slice — wiring is out of scope for 3c-3, only the policy lives here).
//!
//! ## Two separate vocabularies
//!
//! * **Mode** ([`crate::ConfidenceTier`]) — how aggressive the engine
//!   should be. `Cautious` (strict, fires only on High confidence),
//!   `Balanced` (Medium+), `Eager` (Low+).
//! * **Confidence** ([`crate::Confidence`]) — how strong the *evidence*
//!   is for a specific candidate. `High` / `Medium` / `Low` / none.
//!
//! A candidate has confidence; a mode chooses what confidence to act on.
//! [`DecisionOutcome::WouldCorrect`] carries the candidate's confidence —
//! never a mode name. The active mode is reported alongside the outcome
//! (the engine attaches it to the wire payload) so the panel can show
//! both vocabularies without conflating them.
//!
//! ## Inputs
//!
//! * `original` — the word the user actually typed (case-preserved core
//!   from the tokenizer).
//! * `is_known` — `Lexicon::is_known(original)`. Known words are
//!   protected outside of this module; we accept the bool as input so
//!   the decision logic doesn't have to re-do the lookup.
//! * `report` — the per-candidate [`ConfidenceReport`]. Already
//!   score-sorted by [`crate::score::score_candidates`].
//! * `active_tier` — the engine's mode. The decision fires only when the
//!   top candidate's confidence is *at least* what the mode requires.
//!
//! ## Outcomes
//!
//! See [`DecisionOutcome`]. The five leave-alone reasons together cover
//! the entire "do nothing" surface so the FEED never has to guess *why*
//! the engine stayed quiet.
//!
//! ## Versioning
//!
//! Bump [`DECISION_VERSION`] on any change to the policy or the
//! serialized outcome shape (e.g. the 3c-3 rename of `tier` →
//! `confidence` in `WouldCorrect`).

use serde::{Deserialize, Serialize};

use crate::score::{confidence_for, Confidence, ConfidenceReport};
use crate::ConfidenceTier;

/// Bumped to 2 in 3c-3: `WouldCorrect.tier: ConfidenceTier` (mode name)
/// → `WouldCorrect.confidence: Confidence` (per-candidate label).
pub const DECISION_VERSION: u32 = 2;

/// **EASILY-FLIPPED CONSTANT.** Which mode the decision policy treats as
/// active. **Quiet-by-default.** Flip during verification by changing
/// this constant; later this becomes a per-user setting backed by the
/// discrete card selector in the UI (CLAUDE.md: no sliders).
pub const ACTIVE_TIER: ConfidenceTier = ConfidenceTier::Cautious;

/// Minimum gap between the top and runner-up scores for a confident pick.
/// Below this, the decision is treated as ambiguous (e.g. `wodl` could go
/// either to `word` or `world` — don't confidently pick one). Placeholder;
/// tune from real Observing data.
pub const SEPARATION_MARGIN: f64 = 0.10;

/// What the engine would do for a single sealed Word token. Observe-only
/// — `WouldCorrect` is a *proposal*, not an injection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionOutcome {
    /// The engine would propose `suggested` for `original` because the
    /// candidate's `confidence` meets the active mode's bar. `score` is
    /// the raw motor×lexicon product; `runner_up_score` is the next-best
    /// for context (so a panel can render the separation).
    WouldCorrect {
        original: String,
        suggested: String,
        confidence: Confidence,
        score: f64,
        runner_up_score: Option<f64>,
    },
    /// Engine leaves the word as the user typed it. The reason is
    /// load-bearing — the FEED renders it so the builder can tell why.
    LeaveAlone {
        original: String,
        reason: LeaveAloneReason,
    },
}

impl DecisionOutcome {
    pub fn original(&self) -> &str {
        match self {
            DecisionOutcome::WouldCorrect { original, .. } => original,
            DecisionOutcome::LeaveAlone { original, .. } => original,
        }
    }
}

/// Why the engine left a word alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaveAloneReason {
    /// Word is in the clean spelling dictionary — protected by design.
    Known,
    /// Word is unknown but has no clean-dict edit-1 neighbour to suggest.
    NoCandidates,
    /// Top candidate's score is below even [`crate::CONFIDENCE_LOW_FLOOR`]
    /// — no mode would fire.
    BelowFloor,
    /// Top candidate's confidence clears the floor but doesn't meet the
    /// active mode's bar. Flip [`ACTIVE_TIER`] to see this reason change.
    BelowActiveTier,
    /// Top two candidates are within [`SEPARATION_MARGIN`] of each other
    /// — too close to confidently pick the leader (e.g. `world` vs `word`).
    Ambiguous,
}

/// Apply the decision policy to one word + its scoring report.
///
/// Pure function — no I/O, no global state, no side effects. The engine
/// composes this with the tokenizer / lexicon / candidate / scoring
/// stages; library callers can go through [`crate::CorrectionEngine`].
pub fn decide(
    original: &str,
    is_known: bool,
    report: &ConfidenceReport,
    active_tier: ConfidenceTier,
) -> DecisionOutcome {
    if is_known {
        return DecisionOutcome::LeaveAlone {
            original: original.to_string(),
            reason: LeaveAloneReason::Known,
        };
    }
    if report.scored.is_empty() {
        return DecisionOutcome::LeaveAlone {
            original: original.to_string(),
            reason: LeaveAloneReason::NoCandidates,
        };
    }
    let top = &report.scored[0];
    let runner = report.scored.get(1);

    let confidence = match confidence_for(top.score) {
        Some(c) => c,
        None => {
            return DecisionOutcome::LeaveAlone {
                original: original.to_string(),
                reason: LeaveAloneReason::BelowFloor,
            };
        }
    };

    if !confidence_meets(confidence, mode_min_confidence(active_tier)) {
        return DecisionOutcome::LeaveAlone {
            original: original.to_string(),
            reason: LeaveAloneReason::BelowActiveTier,
        };
    }

    // Separation check — runner-up too close = ambiguous. Only checked
    // when a runner-up exists; a single-candidate top is unambiguous by
    // construction.
    if let Some(runner) = runner {
        if (top.score - runner.score).abs() < SEPARATION_MARGIN {
            return DecisionOutcome::LeaveAlone {
                original: original.to_string(),
                reason: LeaveAloneReason::Ambiguous,
            };
        }
    }

    DecisionOutcome::WouldCorrect {
        original: original.to_string(),
        suggested: top.word.clone(),
        confidence,
        score: top.score,
        runner_up_score: runner.map(|r| r.score),
    }
}

/// Minimum candidate confidence the given mode requires to fire.
///
/// * `Cautious` → `High` (strictest)
/// * `Balanced` → `Medium`
/// * `Eager` → `Low` (laxest)
pub fn mode_min_confidence(mode: ConfidenceTier) -> Confidence {
    match mode {
        ConfidenceTier::Cautious => Confidence::High,
        ConfidenceTier::Balanced => Confidence::Medium,
        ConfidenceTier::Eager => Confidence::Low,
    }
}

/// True iff the candidate's `confidence` is at least the mode's required
/// `min` confidence. Used to gate firing on the active mode.
///
/// Ordering (weakest → strongest): `Low < Medium < High`.
fn confidence_meets(confidence: Confidence, min: Confidence) -> bool {
    confidence_rank(confidence) >= confidence_rank(min)
}

fn confidence_rank(c: Confidence) -> u8 {
    match c {
        Confidence::Low => 1,
        Confidence::Medium => 2,
        Confidence::High => 3,
    }
}

// Sanity at compile time.
const _: () = assert!(DECISION_VERSION >= 1);

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::{EditType, ScoredCandidate, SCORE_VERSION};

    /// Build a report with the given candidates and the scores baked in.
    fn report_with(scored: Vec<ScoredCandidate>) -> ConfidenceReport {
        let top_score = scored.first().map(|s| s.score);
        let top_confidence = top_score.and_then(confidence_for);
        ConfidenceReport {
            original: "x".to_string(),
            scored,
            top_score,
            top_confidence,
            score_version: SCORE_VERSION,
        }
    }

    fn cand(word: &str, score: f64) -> ScoredCandidate {
        ScoredCandidate {
            word: word.to_string(),
            frequency: 1,
            edit_type: EditType::Substitution,
            lexicon_evidence: score, // values irrelevant for decision tests
            motor_evidence: 1.0,
            score,
        }
    }

    fn assert_leave_alone(out: &DecisionOutcome, expected: LeaveAloneReason) {
        match out {
            DecisionOutcome::LeaveAlone { reason, .. } => assert_eq!(*reason, expected),
            other => panic!("expected LeaveAlone({expected:?}), got {other:?}"),
        }
    }

    // ---- Known short-circuit -------------------------------------------

    #[test]
    fn known_word_leaves_alone_with_known_reason() {
        // Even with a strong candidate set, a known word is always
        // protected — the gate is is_known, not the score.
        let report = report_with(vec![cand("the", 0.95)]);
        let out = decide("the", true, &report, ConfidenceTier::Eager);
        assert_leave_alone(&out, LeaveAloneReason::Known);
    }

    // ---- No candidates --------------------------------------------------

    #[test]
    fn no_candidates_leaves_alone() {
        let out = decide("asdf", false, &report_with(vec![]), ConfidenceTier::Eager);
        assert_leave_alone(&out, LeaveAloneReason::NoCandidates);
    }

    // ---- Below floor ---------------------------------------------------

    #[test]
    fn score_below_lowest_floor_leaves_alone() {
        // Far-substitution motor*lexicon often lands below 0.25.
        let report = report_with(vec![cand("sold", 0.12)]);
        let out = decide("lold", false, &report, ConfidenceTier::Eager);
        assert_leave_alone(&out, LeaveAloneReason::BelowFloor);
    }

    // ---- Below active tier ----------------------------------------------

    #[test]
    fn low_confidence_does_not_fire_when_mode_is_balanced() {
        // Score 0.30 → Low confidence. Balanced mode requires Medium+.
        let report = report_with(vec![cand("the", 0.30)]);
        let out = decide("teh", false, &report, ConfidenceTier::Balanced);
        assert_leave_alone(&out, LeaveAloneReason::BelowActiveTier);
    }

    #[test]
    fn medium_confidence_does_not_fire_when_mode_is_cautious() {
        // Score 0.55 → Medium confidence. Cautious mode requires High.
        let report = report_with(vec![cand("the", 0.55)]);
        let out = decide("teh", false, &report, ConfidenceTier::Cautious);
        assert_leave_alone(&out, LeaveAloneReason::BelowActiveTier);
    }

    // ---- Ambiguous ------------------------------------------------------

    #[test]
    fn top_two_within_separation_margin_are_ambiguous() {
        // world vs word — both plausible neighbours of `wodl`.
        let report = report_with(vec![cand("world", 0.60), cand("word", 0.55)]);
        let out = decide("wodl", false, &report, ConfidenceTier::Eager);
        assert_leave_alone(&out, LeaveAloneReason::Ambiguous);
    }

    #[test]
    fn clear_separation_does_not_trip_ambiguous() {
        let report = report_with(vec![cand("the", 0.70), cand("ten", 0.40)]);
        let out = decide("teh", false, &report, ConfidenceTier::Eager);
        match out {
            DecisionOutcome::WouldCorrect { suggested, .. } => assert_eq!(suggested, "the"),
            other => panic!("expected WouldCorrect, got {other:?}"),
        }
    }

    // ---- Would-correct ---------------------------------------------------

    #[test]
    fn would_correct_at_eager_mode_for_low_confidence_score() {
        let report = report_with(vec![cand("the", 0.30)]);
        let out = decide("teh", false, &report, ConfidenceTier::Eager);
        match out {
            DecisionOutcome::WouldCorrect {
                original,
                suggested,
                confidence,
                score,
                runner_up_score,
            } => {
                assert_eq!(original, "teh");
                assert_eq!(suggested, "the");
                assert_eq!(confidence, Confidence::Low);
                assert!((score - 0.30).abs() < 1e-9);
                assert_eq!(runner_up_score, None);
            }
            other => panic!("expected WouldCorrect, got {other:?}"),
        }
    }

    #[test]
    fn would_correct_includes_runner_up_score_when_present() {
        let report = report_with(vec![cand("the", 0.70), cand("ten", 0.40)]);
        let out = decide("teh", false, &report, ConfidenceTier::Eager);
        match out {
            DecisionOutcome::WouldCorrect {
                runner_up_score, ..
            } => {
                assert_eq!(runner_up_score, Some(0.40));
            }
            other => panic!("expected WouldCorrect, got {other:?}"),
        }
    }

    #[test]
    fn would_correct_fires_at_balanced_mode_when_score_clears_medium() {
        let report = report_with(vec![cand("the", 0.62)]);
        let out = decide("teh", false, &report, ConfidenceTier::Balanced);
        match out {
            DecisionOutcome::WouldCorrect { confidence, .. } => {
                assert_eq!(confidence, Confidence::Medium);
            }
            other => panic!("expected WouldCorrect, got {other:?}"),
        }
    }

    #[test]
    fn would_correct_fires_at_cautious_mode_when_score_clears_high() {
        let report = report_with(vec![cand("the", 0.82)]);
        let out = decide("teh", false, &report, ConfidenceTier::Cautious);
        match out {
            DecisionOutcome::WouldCorrect { confidence, .. } => {
                assert_eq!(confidence, Confidence::High);
            }
            other => panic!("expected WouldCorrect, got {other:?}"),
        }
    }

    // ---- mode_min_confidence / confidence_meets ------------------------

    #[test]
    fn mode_min_confidence_maps_strictness_correctly() {
        assert_eq!(mode_min_confidence(ConfidenceTier::Cautious), Confidence::High);
        assert_eq!(
            mode_min_confidence(ConfidenceTier::Balanced),
            Confidence::Medium
        );
        assert_eq!(mode_min_confidence(ConfidenceTier::Eager), Confidence::Low);
    }

    #[test]
    fn confidence_meets_orders_low_lt_medium_lt_high() {
        // attained = Low satisfies required = Low only.
        assert!(confidence_meets(Confidence::Low, Confidence::Low));
        assert!(!confidence_meets(Confidence::Low, Confidence::Medium));
        assert!(!confidence_meets(Confidence::Low, Confidence::High));
        // attained = Medium satisfies Low and Medium.
        assert!(confidence_meets(Confidence::Medium, Confidence::Low));
        assert!(confidence_meets(Confidence::Medium, Confidence::Medium));
        assert!(!confidence_meets(Confidence::Medium, Confidence::High));
        // attained = High satisfies everything.
        assert!(confidence_meets(Confidence::High, Confidence::Low));
        assert!(confidence_meets(Confidence::High, Confidence::Medium));
        assert!(confidence_meets(Confidence::High, Confidence::High));
    }

    // ---- Original is preserved -----------------------------------------

    #[test]
    fn original_is_preserved_in_every_outcome() {
        let report = report_with(vec![cand("the", 0.30)]);
        assert_eq!(
            decide("teh", false, &report, ConfidenceTier::Eager).original(),
            "teh"
        );
        assert_eq!(
            decide("teh", true, &report, ConfidenceTier::Eager).original(),
            "teh"
        );
        assert_eq!(
            decide("teh", false, &report_with(vec![]), ConfidenceTier::Eager).original(),
            "teh"
        );
    }
}
