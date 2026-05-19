//! Layer 4 — correction engine (STUB).
//!
//! Consumes a `VolatilityMap` from Layer 3 plus a lexicon, and proposes corrections
//! at one of three confidence tiers. See CLAUDE.md for invariants.
//!
//! Self-corrections via backspace are *signal*, not failure. The four-state outcome
//! model is intentional — do not collapse to three.

use serde::{Deserialize, Serialize};

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
