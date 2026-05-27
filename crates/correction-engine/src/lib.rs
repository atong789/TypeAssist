//! Layer 4 — correction engine.
//!
//! Consumes the tokenizer's sealed Word tokens (Component 1), the L4
//! lexicon (Component 3a), edit-1 candidate generation (3b), the
//! per-candidate confidence score (3c-1), and the active-tier decision
//! policy (3c-2) to **propose** a correction. Still observe-only — the
//! engine decides but does not inject.
//!
//! See CLAUDE.md for the four-state outcome model (`CleanHit`,
//! `SelfCorrected`, `UncorrectedMiss`, `TwoKeysTogether`) and the three
//! space-error types. Outcome resolution + space handling are Component 5
//! and 3c-3 respectively — both still stubbed.

use serde::{Deserialize, Serialize};
use volatility_map::VolatilityMap;

pub mod anchor;
pub mod candidates;
pub mod decision;
pub mod keyboard;
pub mod lexicon;
pub mod lexicon_proposal;
pub mod log;
pub mod resolver;
pub mod score;
pub mod tokenizer;
pub use anchor::{AnchorState, AnchorTracker, AnchorsSnapshot, SpanAnchor, VoidReason};
pub use candidates::{ranked_known_candidates, KnownCandidate, CANDIDATES_VERSION};
pub use decision::{
    decide, mode_min_confidence, DecisionOutcome, LeaveAloneReason, ACTIVE_TIER,
    DECISION_VERSION, SEPARATION_MARGIN,
};
pub use lexicon::{Lexicon, LEXICON_VERSION};
pub use lexicon_proposal::{
    HoldReason, Lane, LexiconProposal, LexiconProposer, MotorVerdict, ProposalTier,
    ProposalUpdate, LEXICON_PROPOSAL_VERSION,
};
pub use log::{
    has_motor_evidence, should_log, DecisionLedger, LogConfidence, LogRecord, Outcome,
    DEFAULT_LEDGER_CAPACITY, LOG_VERSION,
};
pub use resolver::{OutcomeResolver, DEFAULT_DEBOUNCE_MS, RESOLVER_VERSION};
pub use score::{
    confidence_for, score_candidates, Confidence, ConfidenceReport, EditType, ScoredCandidate,
    CONFIDENCE_HIGH_FLOOR, CONFIDENCE_LOW_FLOOR, CONFIDENCE_MEDIUM_FLOOR, SCORE_VERSION,
};
pub use tokenizer::{Token, TokenKind, Tokenizer, TOKENIZER_VERSION};

/// **Engine MODE.** Names how aggressive the user wants the engine to be.
/// The mode gates whether a candidate's confidence is high enough to fire
/// a correction — **not** a label for any specific candidate.
///
/// * `Cautious` — strictest threshold. Engine acts only on High-confidence
///   candidates. Quiet-by-default.
/// * `Balanced` — middle threshold. Acts on Medium-or-higher confidence.
/// * `Eager` — laxest threshold. Acts on Low-or-higher confidence; most
///   frequent corrections.
///
/// Modes and candidate-confidences use **separate vocabularies** — see
/// [`crate::score::Confidence`] for the per-candidate label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceTier {
    Cautious,
    Balanced,
    Eager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceError {
    MissingSpace,
    ExtraSpace,
    ModifierDrift,
}

/// Per-word outcome. All four variants are load-bearing — do not collapse.
/// **Reserved for Component 5** (outcome resolution).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordOutcome {
    CleanHit,
    SelfCorrected,
    UncorrectedMiss,
    TwoKeysTogether,
}

/// A specific correction proposal. Currently a thin packaging of the
/// `WouldCorrect` arm of [`DecisionOutcome`]; richer shape (e.g.
/// space-error attachment, paste handling) lands with the matching
/// components.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrectionProposal {
    pub original: String,
    pub suggested: String,
    pub tier: ConfidenceTier,
    pub space_error: Option<SpaceError>,
}

/// Top-level library handle. Composes lexicon + candidates + scoring +
/// decision into a single `propose` call — what an external L1 host
/// would use if it didn't want to wire the stages itself. The Tauri
/// engine instead calls the lower-level modules directly so it can
/// emit panel events at each stage.
pub struct CorrectionEngine {
    active_tier: ConfidenceTier,
    lexicon: &'static Lexicon,
}

/// Cap on candidates the library considers when deciding. The Tauri
/// panel uses its own (lower) limit for the per-line display; this is
/// the headroom the decision policy needs to compute the separation
/// check (top vs runner-up).
const DECISION_CANDIDATE_LIMIT: usize = 5;

impl CorrectionEngine {
    pub fn new(active_tier: ConfidenceTier) -> Self {
        Self {
            active_tier,
            lexicon: Lexicon::shared(),
        }
    }

    /// The active tier the policy is gating on. The Tauri host exposes
    /// this via [`ACTIVE_TIER`] today; per-user setting comes later.
    pub fn active_tier(&self) -> ConfidenceTier {
        self.active_tier
    }

    /// Apply the full pipeline to one Word token's core text and return
    /// the decision. **Observe-only** — the caller renders / logs the
    /// outcome but does not inject (injection is Component 3c-3).
    pub fn propose(&self, word: &str, map: &VolatilityMap) -> DecisionOutcome {
        let is_known = self.lexicon.is_known(word);
        let candidates = ranked_known_candidates(word, self.lexicon, DECISION_CANDIDATE_LIMIT);
        let report = score_candidates(word, &candidates, map);
        decide(word, is_known, &report, self.active_tier)
    }

    pub fn record_outcome(&mut self, _word: &str, _outcome: WordOutcome) {
        todo!("Component 5 — outcome resolution not implemented yet")
    }
}

/// A single printable, non-whitespace, non-control character belongs to a word.
/// Used by the Tauri host to filter the raw `key` field on input events
/// down to characters the tokenizer can stream.
pub fn is_word_char(key: &str) -> bool {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => !c.is_control() && !c.is_whitespace(),
        _ => false,
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use volatility_map::{ProfileContext, TimeOfDay};

    fn empty_map() -> VolatilityMap {
        VolatilityMap::empty(
            0,
            ProfileContext {
                time_of_day: TimeOfDay::Morning,
                session_fatigue: 0.0,
            },
        )
    }

    #[test]
    fn engine_propose_leaves_known_word_alone() {
        let engine = CorrectionEngine::new(ConfidenceTier::Eager);
        let out = engine.propose("the", &empty_map());
        match out {
            DecisionOutcome::LeaveAlone { reason, .. } => {
                assert_eq!(reason, LeaveAloneReason::Known);
            }
            other => panic!("expected LeaveAlone(Known), got {other:?}"),
        }
    }

    #[test]
    fn engine_propose_returns_no_candidates_for_nonsense() {
        let engine = CorrectionEngine::new(ConfidenceTier::Eager);
        let out = engine.propose("qzxjvk", &empty_map());
        match out {
            DecisionOutcome::LeaveAlone { reason, .. } => {
                assert_eq!(reason, LeaveAloneReason::NoCandidates);
            }
            other => panic!("expected LeaveAlone(NoCandidates), got {other:?}"),
        }
    }

    #[test]
    fn engine_propose_returns_correction_when_winner_is_unambiguous() {
        // `recieve` → `receive` (transposition, ~10M Norvig count) clearly
        // beats `relieve` (replace c→l, far-apart sub, ~few M). Top score
        // ~0.47, runner-up ~0.09 — well above SEPARATION_MARGIN.
        let engine = CorrectionEngine::new(ConfidenceTier::Eager);
        let out = engine.propose("recieve", &empty_map());
        match out {
            DecisionOutcome::WouldCorrect { suggested, .. } => {
                assert_eq!(suggested, "receive");
            }
            other => panic!("expected WouldCorrect(receive), got {other:?}"),
        }
    }

    #[test]
    fn engine_propose_returns_ambiguous_for_near_tie() {
        // `teh` is a known near-tie under the placeholder scoring:
        //   - `ten` via h→n adjacent fat-finger sub  ≈ 0.72
        //   - `the` via h↔e transposition            ≈ 0.70
        // Below SEPARATION_MARGIN (0.10) → Ambiguous, exactly the
        // behaviour the brief asked for ("don't confidently pick a
        // near-tie"). Pinned so a scorer tuning change can't silently
        // start picking a confident loser.
        let engine = CorrectionEngine::new(ConfidenceTier::Eager);
        let out = engine.propose("teh", &empty_map());
        match out {
            DecisionOutcome::LeaveAlone { reason, .. } => {
                assert_eq!(reason, LeaveAloneReason::Ambiguous);
            }
            other => panic!("expected LeaveAlone(Ambiguous), got {other:?}"),
        }
    }

    #[test]
    fn engine_propose_at_cautious_active_leaves_unambiguous_winner_alone_too() {
        // `recieve` → `receive` score ~0.47 — clears the Low floor (0.25)
        // and would fire under Eager mode, but below the Medium (0.50)
        // and High (0.75) floors. At Cautious mode (strictest) the
        // decision is BelowActiveTier. Flipping `ACTIVE_TIER` is the
        // verification surface exposed by Component 3c-3.
        let engine = CorrectionEngine::new(ConfidenceTier::Cautious);
        let out = engine.propose("recieve", &empty_map());
        match out {
            DecisionOutcome::LeaveAlone { reason, .. } => {
                assert_eq!(reason, LeaveAloneReason::BelowActiveTier);
            }
            other => panic!("expected LeaveAlone(BelowActiveTier), got {other:?}"),
        }
    }
}
