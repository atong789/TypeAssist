//! Layer 4 — correction engine (STUB).
//!
//! Consumes a `VolatilityMap` from Layer 3 plus a lexicon, and proposes corrections
//! at one of three confidence tiers. See CLAUDE.md for invariants.
//!
//! Self-corrections via backspace are *signal*, not failure. The four-state outcome
//! model is intentional — do not collapse to three.

use serde::{Deserialize, Serialize};

pub mod anchor;
pub mod candidates;
pub mod keyboard;
pub mod lexicon;
pub mod score;
pub mod tokenizer;
pub use anchor::{AnchorState, AnchorTracker, AnchorsSnapshot, SpanAnchor, VoidReason};
pub use candidates::{ranked_known_candidates, KnownCandidate, CANDIDATES_VERSION};
pub use lexicon::{Lexicon, LEXICON_VERSION};
pub use score::{
    score_candidates, tier_for, ConfidenceReport, EditType, ScoredCandidate, SCORE_VERSION,
};
pub use tokenizer::{Token, TokenKind, Tokenizer, TOKENIZER_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceTier {
    Gentle,
    Balanced,
    Bold,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceError {
    MissingSpace,
    ExtraSpace,
    ModifierDrift,
}

/// Per-word outcome. All four variants are load-bearing — do not collapse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WordOutcome {
    CleanHit,
    SelfCorrected,
    UncorrectedMiss,
    TwoKeysTogether,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrectionProposal {
    pub original: String,
    pub suggested: String,
    pub tier: ConfidenceTier,
    pub space_error: Option<SpaceError>,
}

pub struct CorrectionEngine {
    _tier: ConfidenceTier,
}

impl CorrectionEngine {
    pub fn new(tier: ConfidenceTier) -> Self {
        Self { _tier: tier }
    }

    pub fn propose(
        &self,
        _word: &str,
        _map: &volatility_map::VolatilityMap,
    ) -> Option<CorrectionProposal> {
        todo!("L4 stub — lexicon-aware spatial correction not implemented yet")
    }

    pub fn record_outcome(&mut self, _word: &str, _outcome: WordOutcome) {
        todo!("L4 stub — outcome recording not implemented yet")
    }
}

/// Walking-skeleton lookup: one hardcoded correction, no real map.
///
/// This is intentionally NOT the L3 volatility map and NOT the lexicon — it
/// exists only to prove the Swift↔Rust thread end to end. See CLAUDE.md
/// "walking skeleton". Replace with `CorrectionEngine::propose` once L2/L3 land.
pub fn skeleton_lookup(word: &str) -> Option<&'static str> {
    match word {
        "tge" => Some("the"),
        _ => None,
    }
}

/// If `key` is a single whitespace word-boundary character, return it.
///
/// Shared by the walking-skeleton binary and the Tauri host so they assemble
/// words identically.
pub fn boundary_char(key: &str) -> Option<char> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    c.is_whitespace().then_some(c)
}

/// A single printable, non-whitespace, non-control character belongs to a word.
pub fn is_word_char(key: &str) -> bool {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => !c.is_control() && !c.is_whitespace(),
        _ => false,
    }
}
