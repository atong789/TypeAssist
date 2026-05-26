//! Decision policy — Component 3c-2 of the L4 brief.
//!
//! Turns a [`ConfidenceReport`] from [`crate::score`] into an actionable
//! outcome: either we **would** propose a correction (with tier and score),
//! or we leave the word alone (with a reason). This pass is still
//! **observe-only**: callers may render the outcome but must NOT inject
//! anything yet (applying corrections is Component 3c-3).
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
//! * `active_tier` — gate; the decision fires only when the top
//!   candidate's tier reaches **at least** this tier. See
//!   [`ACTIVE_TIER`].
//!
//! ## Outcomes
//!
//! See [`DecisionOutcome`] for the full enum. The five leave-alone
//! reasons together cover the entire "do nothing" surface so the FEED
//! never has to guess *why* the engine stayed quiet.
//!
//! ## Versioning
//!
//! Bump [`DECISION_VERSION`] on any change to the policy or to the
//! serialized outcome shape. Mirrors the other versioned modules.

use serde::{Deserialize, Serialize};

use crate::score::{tier_for, ConfidenceReport};
use crate::ConfidenceTier;

pub const DECISION_VERSION: u32 = 1;

/// **EASILY-FLIPPED CONSTANT.** Which tier the decision policy treats as
/// active during cold-start verification. Set to the conservative default
/// per the brief — flip during verification by changing this constant.
/// Later this becomes a per-user setting backed by the discrete card
/// selector in the UI (CLAUDE.md: no sliders).
pub const ACTIVE_TIER: ConfidenceTier = ConfidenceTier::Gentle;

/// Minimum gap between the top and runner-up scores for a confident pick.
/// Below this, the decision is treated as ambiguous (e.g. `wodl` could go
/// either to `word` or `world` — don't confidently pick one). Placeholder;
/// tune from real Observing data.
pub const SEPARATION_MARGIN: f64 = 0.10;

/// What the engine would do for a single sealed Word token. Observe-only
/// this pass — `WouldCorrect` is a *proposal*, not an injection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionOutcome {
    /// The engine would propose `suggested` for `original` at tier
    /// `tier`. `score` is the top-candidate score; `runner_up_score` is
    /// the next-best for context (so a panel can render the separation).
    WouldCorrect {
        original: String,
        suggested: String,
        tier: ConfidenceTier,
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
    /// Top candidate's score is below the lowest tier floor — even the
    /// most permissive tier wouldn't fire.
    BelowFloor,
    /// Top candidate clears the floor but doesn't reach `active_tier`.
    /// Flip [`ACTIVE_TIER`] to see this reason change.
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

    let top_tier = match tier_for(top.score) {
        Some(t) => t,
        None => {
            return DecisionOutcome::LeaveAlone {
                original: original.to_string(),
                reason: LeaveAloneReason::BelowFloor,
            };
        }
    };

    if !tier_meets(top_tier, active_tier) {
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
        tier: top_tier,
        score: top.score,
        runner_up_score: runner.map(|r| r.score),
    }
}

/// True iff `attained` is at least as strong as `required`. Used to
/// gate the decision on the active tier.
///
/// Ordering (weakest → strongest threshold): `Gentle < Balanced < Bold`.
fn tier_meets(attained: ConfidenceTier, required: ConfidenceTier) -> bool {
    tier_rank(attained) >= tier_rank(required)
}

fn tier_rank(t: ConfidenceTier) -> u8 {
    match t {
        ConfidenceTier::Gentle => 1,
        ConfidenceTier::Balanced => 2,
        ConfidenceTier::Bold => 3,
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
        let top_tier = top_score.and_then(tier_for);
        ConfidenceReport {
            original: "x".to_string(),
            scored,
            top_score,
            top_tier,
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
        let out = decide("the", true, &report, ConfidenceTier::Gentle);
        assert_leave_alone(&out, LeaveAloneReason::Known);
    }

    // ---- No candidates --------------------------------------------------

    #[test]
    fn no_candidates_leaves_alone() {
        let out = decide("asdf", false, &report_with(vec![]), ConfidenceTier::Gentle);
        assert_leave_alone(&out, LeaveAloneReason::NoCandidates);
    }

    // ---- Below floor ---------------------------------------------------

    #[test]
    fn score_below_gentle_floor_leaves_alone() {
        // Far-substitution motor*lexicon often lands below 0.25.
        let report = report_with(vec![cand("sold", 0.12)]);
        let out = decide("lold", false, &report, ConfidenceTier::Gentle);
        assert_leave_alone(&out, LeaveAloneReason::BelowFloor);
    }

    // ---- Below active tier ----------------------------------------------

    #[test]
    fn gentle_score_does_not_fire_when_active_is_balanced() {
        // Score 0.30 → Gentle tier. Active=Balanced means we need 0.50+.
        let report = report_with(vec![cand("the", 0.30)]);
        let out = decide("teh", false, &report, ConfidenceTier::Balanced);
        assert_leave_alone(&out, LeaveAloneReason::BelowActiveTier);
    }

    #[test]
    fn balanced_score_does_not_fire_when_active_is_bold() {
        let report = report_with(vec![cand("the", 0.55)]);
        let out = decide("teh", false, &report, ConfidenceTier::Bold);
        assert_leave_alone(&out, LeaveAloneReason::BelowActiveTier);
    }

    // ---- Ambiguous ------------------------------------------------------

    #[test]
    fn top_two_within_separation_margin_are_ambiguous() {
        // world vs word — both plausible neighbours of `wodl`.
        let report = report_with(vec![cand("world", 0.60), cand("word", 0.55)]);
        let out = decide("wodl", false, &report, ConfidenceTier::Gentle);
        assert_leave_alone(&out, LeaveAloneReason::Ambiguous);
    }

    #[test]
    fn clear_separation_does_not_trip_ambiguous() {
        let report = report_with(vec![cand("the", 0.70), cand("ten", 0.40)]);
        let out = decide("teh", false, &report, ConfidenceTier::Gentle);
        match out {
            DecisionOutcome::WouldCorrect { suggested, .. } => assert_eq!(suggested, "the"),
            other => panic!("expected WouldCorrect, got {other:?}"),
        }
    }

    // ---- Would-correct ---------------------------------------------------

    #[test]
    fn would_correct_at_gentle_active_for_gentle_score() {
        let report = report_with(vec![cand("the", 0.30)]);
        let out = decide("teh", false, &report, ConfidenceTier::Gentle);
        match out {
            DecisionOutcome::WouldCorrect {
                original,
                suggested,
                tier,
                score,
                runner_up_score,
            } => {
                assert_eq!(original, "teh");
                assert_eq!(suggested, "the");
                assert_eq!(tier, ConfidenceTier::Gentle);
                assert!((score - 0.30).abs() < 1e-9);
                assert_eq!(runner_up_score, None);
            }
            other => panic!("expected WouldCorrect, got {other:?}"),
        }
    }

    #[test]
    fn would_correct_includes_runner_up_score_when_present() {
        let report = report_with(vec![cand("the", 0.70), cand("ten", 0.40)]);
        let out = decide("teh", false, &report, ConfidenceTier::Gentle);
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
    fn would_correct_fires_at_balanced_active_when_score_clears_balanced() {
        let report = report_with(vec![cand("the", 0.62)]);
        let out = decide("teh", false, &report, ConfidenceTier::Balanced);
        match out {
            DecisionOutcome::WouldCorrect { tier, .. } => {
                assert_eq!(tier, ConfidenceTier::Balanced);
            }
            other => panic!("expected WouldCorrect, got {other:?}"),
        }
    }

    #[test]
    fn would_correct_fires_at_bold_active_when_score_clears_bold() {
        let report = report_with(vec![cand("the", 0.82)]);
        let out = decide("teh", false, &report, ConfidenceTier::Bold);
        match out {
            DecisionOutcome::WouldCorrect { tier, .. } => {
                assert_eq!(tier, ConfidenceTier::Bold);
            }
            other => panic!("expected WouldCorrect, got {other:?}"),
        }
    }

    // ---- tier_meets ----------------------------------------------------

    #[test]
    fn tier_meets_orders_gentle_lt_balanced_lt_bold() {
        // attained = Gentle satisfies required = Gentle only.
        assert!(tier_meets(ConfidenceTier::Gentle, ConfidenceTier::Gentle));
        assert!(!tier_meets(ConfidenceTier::Gentle, ConfidenceTier::Balanced));
        assert!(!tier_meets(ConfidenceTier::Gentle, ConfidenceTier::Bold));
        // attained = Balanced satisfies Gentle and Balanced.
        assert!(tier_meets(ConfidenceTier::Balanced, ConfidenceTier::Gentle));
        assert!(tier_meets(ConfidenceTier::Balanced, ConfidenceTier::Balanced));
        assert!(!tier_meets(ConfidenceTier::Balanced, ConfidenceTier::Bold));
        // attained = Bold satisfies everything.
        assert!(tier_meets(ConfidenceTier::Bold, ConfidenceTier::Gentle));
        assert!(tier_meets(ConfidenceTier::Bold, ConfidenceTier::Balanced));
        assert!(tier_meets(ConfidenceTier::Bold, ConfidenceTier::Bold));
    }

    // ---- Original is preserved -----------------------------------------

    #[test]
    fn original_is_preserved_in_every_outcome() {
        let report = report_with(vec![cand("the", 0.30)]);
        assert_eq!(
            decide("teh", false, &report, ConfidenceTier::Gentle).original(),
            "teh"
        );
        assert_eq!(
            decide("teh", true, &report, ConfidenceTier::Gentle).original(),
            "teh"
        );
        assert_eq!(
            decide("teh", false, &report_with(vec![]), ConfidenceTier::Gentle).original(),
            "teh"
        );
    }
}
