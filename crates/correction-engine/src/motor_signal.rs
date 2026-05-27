//! Per-token motor cleanliness signal — Component 5b's
//! candidate-INDEPENDENT motor read.
//!
//! C3's [`crate::score::motor_evidence_for`] is candidate-dependent: it
//! scores "this typed word looks like a motor slip of [candidate]" for
//! a specific (typed, candidate) pair. That's the right shape when the
//! engine has a correction theory, but for the C5b lexicon proposer
//! the **fast lane** (no candidate at all) has nothing to compute
//! against — every Soumyo-shaped record carried `Unknown` motor verdict
//! and the held gate degenerated to lane + count.
//!
//! This module computes a signal that doesn't need a candidate:
//! **was the token typed cleanly, or with slip-like keystrokes?**
//! Based on the per-char dwells the engine already maintains parallel
//! to `line_buf` (the `line_dwells: Vec<u32>`). Every record gets a
//! verdict — fast lane included.
//!
//! ## Inputs
//!
//! Phase 1 uses **dwell** only: a per-key press-to-release time in
//! milliseconds, available for every sealed character because the C4
//! motor-evidence gate already requires it. Future phases can fold in
//! per-key intervals, adjacency / co-activation, and L2 ghost-key
//! signals attributed to the token's span. The signal type already
//! carries `slip_score` so a richer detector can refine the score
//! without changing the wire shape.
//!
//! ## Slip heuristic — Phase 1
//!
//! A dwell **below [`GRAZE_DWELL_MS`]** is a finger-graze / ghost-tap
//! signature: the key registered but the user barely touched it.
//! That's the simplest, most reliable per-char signal for "this key
//! didn't land deliberately." We mark a token `Slip` if **any** char
//! in the span dwells under the threshold.
//!
//! Tokens shorter than [`MIN_SAMPLE_CHARS`] are `Insufficient` — there
//! isn't enough span to read a pattern; one-char tokens are also gated
//! upstream by the proposer's `ObviousFragment` rule, but we still
//! mark them so the panel renders the right cell.
//!
//! Tokens that pass both gates are `Clean`. The verdict alone drives
//! the proposer's gate; `slip_score` is a numeric companion the debug
//! panel renders so the builder can sanity-check where the threshold
//! actually splits intended vs slip.
//!
//! ## Thresholds — PLACEHOLDERS
//!
//! Pure round numbers; tune from real Observing data and revisit when
//! the per-token signal earns its keep. The doc-comment + the
//! constant are the only places thresholds live.

use serde::{Deserialize, Serialize};

/// Dwell at or below this is a finger-graze / ghost-tap signature.
/// Normal deliberate keystrokes sit ~50-150 ms on this user; sub-30
/// ms presses indicate the key registered but wasn't intentional.
/// **PLACEHOLDER** — re-pin against the user's actual dwell baseline
/// once C5b is observed against real typing.
pub const GRAZE_DWELL_MS: u32 = 30;

/// Minimum span length (chars) for a reliable verdict. A 1-char token
/// could be a real word (e.g. "I") but the proposer's
/// `ObviousFragment` gate already excludes those from promotion;
/// this constant just makes the verdict explicit ("Insufficient") so
/// the panel doesn't display a coin-flip Clean / Slip on too few
/// samples.
pub const MIN_SAMPLE_CHARS: usize = 2;

/// Per-token motor cleanliness verdict — the C5b proposer reads this
/// off [`crate::log::LogRecord::token_motor`] to gate held vs promote.
///
/// Deliberately a three-state shape (not boolean): `Insufficient` is
/// load-bearing so the panel can show "we couldn't tell" instead of
/// pretending to a confident verdict on one keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenMotorVerdict {
    /// All char dwells passed the graze threshold and the span had
    /// enough characters to evaluate. The fingers executed deliberately.
    Clean,
    /// At least one char in the span dwells below [`GRAZE_DWELL_MS`].
    /// The C5b proposer holds the proposal regardless of lane / count.
    Slip,
    /// Span too short to read (`< MIN_SAMPLE_CHARS`) or the line_dwells
    /// slice was unavailable / corrupted. The proposer treats this as
    /// "no motor signal" — won't promote off it, won't hold off it.
    Insufficient,
}

/// Full per-token motor signal attached to every loggable record. The
/// verdict is what drives the proposer's gate; the score + counts are
/// for panel display and for diagnostic regression tests.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TokenMotorSignal {
    pub verdict: TokenMotorVerdict,
    /// Fraction of chars in the span whose dwell fell at or below
    /// [`GRAZE_DWELL_MS`]. `0.0` for a clean token; `1.0` if every
    /// char in the span was a graze. Surfaced on the LEXICON panel
    /// as the `m·ev` column so the builder can see the raw read.
    pub slip_score: f64,
    /// Number of graze-shaped chars in the span. Mostly diagnostic;
    /// counted separately from `slip_score` so the panel can show
    /// "1 / 6" style ratios in future without recomputing.
    pub graze_count: u32,
    /// Span length in chars at measurement time. Records the sample
    /// size the verdict was decided from.
    pub char_count: u32,
}

impl TokenMotorSignal {
    /// Insufficient-with-no-samples sentinel, used when callers can't
    /// produce a real signal (e.g. a future paste-detector path where
    /// the dwell buffer is unavailable for the token's span).
    pub fn insufficient() -> Self {
        Self {
            verdict: TokenMotorVerdict::Insufficient,
            slip_score: 0.0,
            graze_count: 0,
            char_count: 0,
        }
    }
}

/// Compute the per-token motor signal from the slice of per-char
/// dwells inside the token's span. Caller passes
/// `line_dwells[start..end]` (engine.rs maintains the buffer in
/// lock-step with `line_buf`).
///
/// Pure function — no I/O, no shared state. Easy to drive from tests
/// by constructing a small `&[u32]` slice.
pub fn measure_token_motor(span_dwells: &[u32]) -> TokenMotorSignal {
    let char_count = span_dwells.len() as u32;
    if span_dwells.len() < MIN_SAMPLE_CHARS {
        return TokenMotorSignal {
            verdict: TokenMotorVerdict::Insufficient,
            slip_score: 0.0,
            graze_count: 0,
            char_count,
        };
    }
    let graze_count = span_dwells.iter().filter(|&&d| d <= GRAZE_DWELL_MS).count() as u32;
    let slip_score = graze_count as f64 / span_dwells.len() as f64;
    let verdict = if graze_count > 0 {
        TokenMotorVerdict::Slip
    } else {
        TokenMotorVerdict::Clean
    };
    TokenMotorSignal {
        verdict,
        slip_score,
        graze_count,
        char_count,
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_span_is_insufficient() {
        let s = measure_token_motor(&[]);
        assert_eq!(s.verdict, TokenMotorVerdict::Insufficient);
        assert_eq!(s.char_count, 0);
    }

    #[test]
    fn single_char_span_is_insufficient() {
        let s = measure_token_motor(&[100]);
        assert_eq!(s.verdict, TokenMotorVerdict::Insufficient);
        assert_eq!(s.char_count, 1);
    }

    #[test]
    fn all_clean_dwells_resolve_clean() {
        let s = measure_token_motor(&[80, 100, 95, 110, 90, 75]);
        assert_eq!(s.verdict, TokenMotorVerdict::Clean);
        assert_eq!(s.graze_count, 0);
        assert!(
            (s.slip_score - 0.0).abs() < 1e-9,
            "clean span must score 0.0, got {}",
            s.slip_score
        );
        assert_eq!(s.char_count, 6);
    }

    #[test]
    fn one_graze_dwell_resolves_slip() {
        // 'S','o','u','m','y','o' — middle 'm' was a graze.
        let s = measure_token_motor(&[80, 90, 100, 15, 95, 85]);
        assert_eq!(s.verdict, TokenMotorVerdict::Slip);
        assert_eq!(s.graze_count, 1);
        assert!((s.slip_score - (1.0 / 6.0)).abs() < 1e-9);
    }

    #[test]
    fn dwell_exactly_at_threshold_counts_as_graze() {
        // Threshold is inclusive — `dwell <= GRAZE_DWELL_MS` matches.
        // Pinned so a future "off-by-one" refactor doesn't silently
        // shift the slip boundary.
        let s = measure_token_motor(&[100, GRAZE_DWELL_MS, 100]);
        assert_eq!(s.verdict, TokenMotorVerdict::Slip);
        assert_eq!(s.graze_count, 1);
    }

    #[test]
    fn dwell_just_above_threshold_is_clean() {
        let s = measure_token_motor(&[100, GRAZE_DWELL_MS + 1, 100]);
        assert_eq!(s.verdict, TokenMotorVerdict::Clean);
        assert_eq!(s.graze_count, 0);
    }

    #[test]
    fn all_graze_dwells_score_one() {
        let s = measure_token_motor(&[10, 5, 20, 15]);
        assert_eq!(s.verdict, TokenMotorVerdict::Slip);
        assert_eq!(s.graze_count, 4);
        assert!((s.slip_score - 1.0).abs() < 1e-9);
    }
}
