//! Lexicon-learning proposal — Component 5b, Phase 1 of the L4 Observing
//! brief. **Observe-only this phase**: the proposer reads C5a's resolved
//! outcomes and emits a per-word verdict (lane + motor signal + tier +
//! occasions) for the debug panel. It does **NOT** touch
//! [`crate::Lexicon::is_known`] — that wiring lands in Phase 2 after the
//! panel has been watched against real typing.
//!
//! ## What feeds learning
//!
//! Only [`Outcome::Kept`] records contribute. [`Outcome::CorrectedToSuggestion`]
//! / [`Outcome::CorrectedToOther`] feed the motor map later (5c);
//! [`Outcome::Abandoned`] feeds nothing.
//!
//! The proposer **must react to revisable transitions** ([`crate::OutcomeResolver`]'s
//! "latest write wins" contract): a Kept that later flips to Corrected or
//! Abandoned must retract its prior contribution. The internal
//! [`RecordContribution`] cache tracks each ledger record's last
//! observed outcome so a re-resolve can be undone.
//!
//! ## Lanes (which bar the word starts at)
//!
//! * **Fast lane** — `top_candidate.is_none()`. The engine found no
//!   edit-1 neighbour at all; the word is *unknown* in the strongest
//!   sense. Lowest bar to promote, since there's nothing the engine
//!   could plausibly have meant by it.
//! * **Slow lane** — `top_candidate.is_some()`. The engine had a
//!   suggestion the user *rejected* by keeping the original. Bar scales
//!   with the rejected suggestion's `confidence` band: rejecting a HIGH
//!   confidence suggestion is the strongest "user knows what they're
//!   doing" signal, but also the highest cost if we're wrong.
//!
//! ## Motor verdict (held = motor-only)
//!
//! Held means **this token's execution looks slip-like, nothing else**
//! — *not* "the engine had a high-confidence suggestion." String
//! distance to a known word never holds a record on its own: a
//! deliberate novel word that happens to be edit-1 from a frequent
//! word should still promote if the fingers executed cleanly.
//!
//! The proposer reads [`LogRecord::token_motor`] — the
//! candidate-independent per-token motor signal from
//! [`crate::motor_signal`]. It's computed from the dwell slice over
//! the token's own keystrokes, so the **fast lane** (no candidate)
//! has a real verdict instead of falling through to `Unknown` and
//! degenerating the gate to lane + count.
//!
//! | `TokenMotorVerdict`  | proposer reads as | meaning                          |
//! |----------------------|-------------------|----------------------------------|
//! | `Clean`              | `MotorVerdict::Clean` | All char dwells passed the graze threshold — fingers executed deliberately. |
//! | `Slip`               | `MotorVerdict::Slip` | At least one graze-shaped dwell in the span — looks like an uncaught slip. |
//! | `Insufficient`       | `MotorVerdict::Unknown` | Span too short to read — won't promote off it, won't hold off it. |
//! | (record carries `None`) | `MotorVerdict::Unknown` | Defensive — engine should always populate. |
//!
//! See [`crate::motor_signal`] for the (placeholder) graze threshold
//! and the dwell-based heuristic.
//!
//! ## Tiers (display-only this phase)
//!
//! * `Held(reason)` — proposer refuses to promote (motor slip signature,
//!   high-confidence rejection without corroboration, obvious fragment).
//! * `Provisional` — would stop flagging the word but not yet a
//!   correction target.
//! * `Confirmed` — recurs consistently → full citizen / correction anchor.
//!
//! Phase 1 uses *occasions count* as a coarse corroboration proxy. Real
//! corroboration needs distinct sessions / time-bins (5d / persistence);
//! the panel exposes the count so we can see whether the threshold is
//! tight enough on real typing.
//!
//! ## Non-goals (Phase 1)
//!
//! * **No `is_known` writes.** Phase 2.
//! * **No persistence.** In-memory only; cleared on engine restart.
//! * **No fragment surgery.** Obvious fragments (single-char or empty)
//!   are flagged `Held(ObviousFragment)` and excluded from promotion,
//!   but we don't try to reconstruct the parent word — that's 5c.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::lexicon::Lexicon;
use crate::linguistic::{linguistic_signal, LinguisticSignal, ProximityVerdict};
use crate::log::{LogConfidence, LogRecord, Outcome};
use crate::motor_signal::TokenMotorVerdict;

/// Version of the proposal wire shape. Bump on any change to
/// [`LexiconProposal`] / [`Lane`] / [`MotorVerdict`] / [`ProposalTier`].
///
/// v2 — C5b motor-signal fix.
/// v3 — C5b linguistic stack (plausibility / proximity / hold reasons).
/// v4 — Eligibility/Promotion split. Restructured `recompute_tier`
/// into two stages: an eligibility veto (lane- and count-independent;
/// recurrence can never bypass) and a promotion stage (lane sets the
/// occasion bar). Added `norvig_freq` so the eligibility veto can
/// detect near-known + no-web-presence (catches `aduluts`-class
/// typos that the bigram model can't distinguish from real words by
/// shape alone). Near-known gates BOTH lanes now (was fast-only).
pub const LEXICON_PROPOSAL_VERSION: u32 = 4;

// ---- Tunable PLACEHOLDERS — replace with values from real Observing data --

/// Minimum word length to be eligible for promotion. Two-char unknown
/// tokens (`un`, `iv`, `qz`) are overwhelmingly typos / fragments of
/// real words; legitimate 2-char words (`of`, `to`, `is`) are all in
/// SCOWL and never reach the proposer. Strictly less than this →
/// `Held(ObviousFragment)`. Bumped from 2 to 3 after slow-lane
/// leaks (`un` was reaching Confirmed).
const MIN_PROMOTABLE_WORD_LEN: usize = 3;

/// Occasions at which a `Provisional` proposal upgrades to `Confirmed`
/// on the **fast lane** (no candidate exists — engine has no theory
/// of what the user might have meant). Placeholder — real
/// corroboration wants distinct time-bins (5d).
const CONFIRMED_OCCASIONS_THRESHOLD_FAST: u32 = 3;

/// Higher bar for the **slow lane** — the engine already found and
/// suggested a candidate, and the user kept the near-miss anyway.
/// That's more slip-evidence per occasion than the fast lane, not
/// less; we want more independent occasions before confirming.
const CONFIRMED_OCCASIONS_THRESHOLD_SLOW: u32 = 5;

// ---- Public types ---------------------------------------------------------

/// Which promotion path a candidate word is on. The lane is determined
/// per-record by whether the engine had a candidate; on subsequent
/// occasions the *latest* lane wins (a word can move between lanes if
/// the engine's candidate set changes — e.g., the volatility map grew
/// after the first occurrence).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Lane {
    /// `top_candidate.is_none()` — no edit-1 neighbour at all. Lowest
    /// bar; the engine has no theory of what the user might've meant.
    Fast,
    /// `top_candidate.is_some()` — the user kept their text despite a
    /// suggestion. Bar scales with `rejected_confidence`: a HIGH
    /// rejection is the strongest user-vs-engine signal.
    Slow {
        rejected_confidence: LogConfidence,
    },
}

/// Motor signature verdict per record. Same C3 motor signal, reread to
/// decide "clean execution → leans intended" vs "slip-like → probable
/// uncaught slip of a real word, don't learn."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotorVerdict {
    Clean,
    Mixed,
    Slip,
    /// No candidate, no motor signature to compute. Fast-lane records
    /// land here by construction.
    Unknown,
}

/// Why a proposal is held back from promotion. Surfaced on the panel so
/// the builder can see exactly which gate fired. **Motor-only by
/// design**: a hold says "this token's execution looks slip-like" (or
/// is structurally disqualified). String distance to a known word
/// never holds — a deliberate novel word that resembles a frequent
/// one still promotes if the fingers were clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    /// Word length below [`MIN_PROMOTABLE_WORD_LEN`]. Likely a
    /// premature-space boundary fragment (e.g. `Soum` from `Soumyo`);
    /// 5c will reconstruct, 5b just refuses to learn.
    ObviousFragment,
    /// Latest occasion's motor signature is slip-like —
    /// `token_motor.verdict == Slip`.
    SlipSignature,
    /// Plausibility below [`crate::linguistic::PLAUSIBILITY_FLOOR`].
    /// A character-bigram backstop for truly anomalous letter
    /// sequences (`qzqz`-class). Spatial mangles built from common
    /// bigrams pass this and are caught by the proximity gates instead.
    IllFormed,
    /// A high-frequency known word sits within edit distance ≤ 2
    /// (`ProximityVerdict::NearKnownEdit2`). The token is a mangle
    /// of an existing word, not novel vocabulary. Catches `imapc`
    /// (near `impact`), `potentjual` (near `potential`).
    NearKnownWord,
    /// Word splits into 2+ known words (`ProximityVerdict::Segmentable`).
    /// Catches dropped-space merges like `andthe`, `inthe`.
    SegmentableMerge,
    /// Word starts with a high-frequency known prefix and has a
    /// non-empty suffix (`ProximityVerdict::PrefixMerge`). Softer
    /// segmentation signal that catches `themach`-class merges
    /// where the suffix isn't itself a recognized word.
    PrefixMerge,
}

/// Where the proposer thinks the word stands right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProposalTier {
    Held { reason: HoldReason },
    Provisional,
    Confirmed,
}

/// One row in the proposal table — everything the panel needs to render
/// "why would (or wouldn't) this word promote?"
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LexiconProposal {
    /// Case-preserved core text. Lexicon membership is case-insensitive
    /// (see [`crate::Lexicon`]) but the proposer keys per case-preserved
    /// form so the panel renders `"Soumyo"` distinctly from `"soumyo"`.
    pub word: String,
    pub lane: Lane,
    pub motor_verdict: MotorVerdict,
    pub tier: ProposalTier,
    /// Count of `Kept` resolutions for this word. Decremented when a
    /// previously-Kept record re-resolves to something else.
    pub occasions: u32,
    /// Raw motor evidence from the most recent occasion. `None` for
    /// fast-lane records (no candidate → no motor evidence). Surfaced
    /// so the panel can show the value alongside the verdict and the
    /// builder can sanity-check where [`MOTOR_SLIP_THRESHOLD`] sits.
    pub last_motor_evidence: Option<f64>,
    /// `id` of the most recent contributing record. Lets the panel link
    /// a proposal row back to the LOG row, when needed.
    pub last_record_id: u64,
    /// Timestamp (ms since epoch) of the most recent contributing record.
    pub last_seen_ms: u64,
    /// Mean-log10-probability of the word's character bigrams against
    /// the bundled SCOWL distribution. Surfaced on the LEXICON panel
    /// next to the motor verdict so the builder can see the raw
    /// linguistic read.
    pub plausibility: f64,
    /// Proximity verdict: FarFromKnown / NearKnownEdit2 / Segmentable
    /// / PrefixMerge. The verdict drives the gate (held-vs-promote)
    /// and is surfaced in the panel so the builder can see WHY each
    /// non-FarFromKnown verdict fired.
    pub proximity: ProximityVerdict,
    /// Norvig web-corpus frequency for the typed word, looked up
    /// directly (NOT through `is_known`). Zero means "this string
    /// has never been seen on the web" — a strong signal that the
    /// token is a pure typo, not a novel word. Combined with
    /// near-known proximity this is the eligibility-veto signal
    /// that separates `aduluts` (0 web freq → held) from `lol`
    /// (16M web freq → promotable) — neither of which the bigram
    /// plausibility could distinguish.
    pub norvig_freq: u64,
    /// Wire shape version. See [`LEXICON_PROPOSAL_VERSION`].
    pub version: u32,
}

// ---- Internal: per-record contribution cache ------------------------------

/// What the proposer remembers about a single ledger record. Used to
/// undo a contribution when the record re-resolves under C5a's
/// "latest write wins" semantics.
#[derive(Debug, Clone)]
struct RecordContribution {
    /// Word the contribution was credited to (case-preserved).
    word: String,
    /// Outcome we counted. Only `Kept` increments occasions; other
    /// outcomes are tracked here so we don't decrement on a no-op
    /// repeat-tick (e.g. Pending → Pending).
    outcome: Outcome,
}

// ---- The proposer ---------------------------------------------------------

/// In-memory lexicon-learning proposer. Owned by the engine; reset on
/// engine restart by design. Phase 2 persistence will swap the storage
/// behind it.
///
/// Holds a static reference to the shared [`Lexicon`] so the
/// linguistic gate has the dictionary it needs at credit time.
/// Constructed via [`Self::new`] (defaults to `Lexicon::shared`).
pub struct LexiconProposer {
    proposals: HashMap<String, LexiconProposal>,
    record_contributions: HashMap<u64, RecordContribution>,
    lex: &'static Lexicon,
}

impl std::fmt::Debug for LexiconProposer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LexiconProposer")
            .field("proposals", &self.proposals.len())
            .field("record_contributions", &self.record_contributions.len())
            .finish()
    }
}

impl Default for LexiconProposer {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a [`LexiconProposer::note_record`] call. Carries either
/// the updated proposal (caller emits to the panel) or `Skipped`
/// (record didn't affect any proposal, e.g. an obvious fragment in the
/// fast lane with `Pending` outcome).
#[derive(Debug, Clone)]
pub enum ProposalUpdate {
    /// A proposal was inserted, updated, or removed. The variant
    /// carries the **current** state of the proposal (or `None` if the
    /// proposal was retracted because its only contributing record was
    /// undone by a revisable transition).
    Changed(Option<LexiconProposal>, String),
    /// No-op — record outcome didn't change since last time, or never
    /// affected any proposal.
    NoChange,
}

impl LexiconProposer {
    pub fn new() -> Self {
        Self {
            proposals: HashMap::new(),
            record_contributions: HashMap::new(),
            lex: Lexicon::shared(),
        }
    }

    /// All current proposals, ordered by most-recently-touched first.
    /// The engine emits per-proposal events on each change; this is
    /// for tests and for the initial snapshot on panel reconnect.
    pub fn snapshot(&self) -> Vec<LexiconProposal> {
        let mut out: Vec<LexiconProposal> = self.proposals.values().cloned().collect();
        out.sort_by_key(|p| std::cmp::Reverse(p.last_seen_ms));
        out
    }

    pub fn get(&self, word: &str) -> Option<&LexiconProposal> {
        self.proposals.get(word)
    }

    pub fn len(&self) -> usize {
        self.proposals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }

    /// React to a `LogRecord`'s current state. **Idempotent** — calling
    /// twice with the same `(record.id, record.outcome)` is a no-op.
    /// Returns whether the call changed any proposal (and which one),
    /// so the caller can emit a panel event.
    ///
    /// Call sites:
    ///   1. After `ledger.append(...)` for a newly-appended `Pending`
    ///      record. The proposer doesn't credit Pending records, but
    ///      it records the contribution shape so a subsequent
    ///      Pending→Kept transition knows what to credit.
    ///   2. After every `ledger.resolve_outcome(id, ...)` call (i.e.
    ///      every resolver transition). This is where actual Kept
    ///      credits / retractions happen.
    pub fn note_record(&mut self, record: &LogRecord) -> ProposalUpdate {
        let word = record.original_text.clone();

        // Idempotency check: same record id + same outcome since last
        // call → nothing to do. Saves the panel from re-rendering on
        // every redundant tick.
        if let Some(prev) = self.record_contributions.get(&record.id) {
            if prev.outcome == record.outcome && prev.word == word {
                return ProposalUpdate::NoChange;
            }
        }

        // Step 1: undo the previous contribution, if any. Decrement the
        // old word's occasions if we'd credited Kept; remove the
        // contribution. We do this even when the new outcome is also
        // Kept, then re-credit — keeps the math simple if word changed
        // (which shouldn't happen for a given record id, but defensive).
        let prev_word = self
            .record_contributions
            .remove(&record.id)
            .and_then(|c| matches!(c.outcome, Outcome::Kept).then_some(c.word));
        if let Some(pw) = prev_word.as_ref() {
            self.retract_kept_contribution(pw);
        }

        // Step 2: apply the new contribution.
        if matches!(record.outcome, Outcome::Kept) {
            self.credit_kept_contribution(record);
        }

        // Step 3: stash the new contribution shape so the next call
        // can undo correctly.
        self.record_contributions.insert(
            record.id,
            RecordContribution {
                word: word.clone(),
                outcome: record.outcome,
            },
        );

        // Step 4: emit the resulting proposal state (or `None` if
        // retracted — the only contributing record was rolled back).
        let proposal = self.proposals.get(&word).cloned();
        // If both the previous word and the current word ended up
        // with no proposal change, report no change. But for the
        // common case (Kept credited or retracted), we always have a
        // word-keyed event to send.
        ProposalUpdate::Changed(proposal, word)
    }

    /// Forget every proposal AND every per-record contribution. Hooked
    /// to the engine's true line-reset / shutdown paths in case we
    /// ever want a fresh slate (Phase 1 doesn't call this).
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.proposals.clear();
        self.record_contributions.clear();
    }

    // ---- Internal helpers --------------------------------------------

    fn credit_kept_contribution(&mut self, record: &LogRecord) {
        let word = record.original_text.clone();
        let lane = lane_for(record);
        let motor_verdict = motor_verdict_for(record);
        let last_motor_evidence = slip_score_for(record);
        let last_record_id = record.id;
        let last_seen_ms = record.timestamp_ms;
        // Linguistic signal — plausibility + proximity, computed once
        // per credited record. The Norvig-frequency lookup is a
        // separate signal (not derived from the bigram model) — it's
        // what tells `aduluts`-class typos apart from `lol`-class
        // informal real words.
        let LinguisticSignal {
            plausibility,
            well_formed: _,
            proximity,
        } = linguistic_signal(&word, self.lex);
        let norvig_freq = self.lex.frequency(&word);

        let entry = self.proposals.entry(word.clone()).or_insert_with(|| {
            LexiconProposal {
                word: word.clone(),
                lane: lane.clone(),
                motor_verdict,
                tier: ProposalTier::Provisional, // placeholder; recomputed below
                occasions: 0,
                last_motor_evidence,
                last_record_id,
                last_seen_ms,
                plausibility,
                proximity,
                norvig_freq,
                version: LEXICON_PROPOSAL_VERSION,
            }
        });

        entry.occasions += 1;
        entry.lane = lane;
        entry.motor_verdict = motor_verdict;
        entry.last_motor_evidence = last_motor_evidence;
        entry.last_record_id = last_record_id;
        entry.last_seen_ms = last_seen_ms;
        entry.plausibility = plausibility;
        entry.proximity = proximity;
        entry.norvig_freq = norvig_freq;
        entry.tier = recompute_tier(
            &entry.word,
            &entry.lane,
            entry.motor_verdict,
            entry.plausibility,
            entry.proximity,
            entry.norvig_freq,
            entry.occasions,
        );
    }

    fn retract_kept_contribution(&mut self, word: &str) {
        let mut should_remove = false;
        if let Some(p) = self.proposals.get_mut(word) {
            // Saturating: a previously-credited Kept must have a count
            // ≥ 1; the saturating sub is just defensive against a
            // double-undo bug we'd rather not crash on.
            p.occasions = p.occasions.saturating_sub(1);
            if p.occasions == 0 {
                should_remove = true;
            } else {
                p.tier = recompute_tier(
                    &p.word,
                    &p.lane,
                    p.motor_verdict,
                    p.plausibility,
                    p.proximity,
                    p.norvig_freq,
                    p.occasions,
                );
            }
        }
        if should_remove {
            self.proposals.remove(word);
        }
    }
}

// ---- Pure helpers (testable in isolation) ---------------------------------

/// Determine the proposal's lane from a single record. Per-occasion;
/// `LexiconProposer` takes the latest record's lane as the proposal's
/// lane (with the explicit cost that a word can move between lanes
/// across occasions).
fn lane_for(record: &LogRecord) -> Lane {
    match record.top_candidate {
        None => Lane::Fast,
        Some(_) => Lane::Slow {
            rejected_confidence: record.confidence,
        },
    }
}

/// Map [`LogRecord::token_motor`] → [`MotorVerdict`]. The proposer
/// reads the candidate-independent token signal here, not
/// `top_motor_evidence` — see the module doc for why string-distance
/// motor signals don't gate alone. `Mixed` is currently unreachable
/// (the token signal is three-state); kept on the enum for the panel
/// in case a future detector adds a mixed state.
fn motor_verdict_for(record: &LogRecord) -> MotorVerdict {
    match record.token_motor {
        None => MotorVerdict::Unknown,
        Some(s) => match s.verdict {
            TokenMotorVerdict::Clean => MotorVerdict::Clean,
            TokenMotorVerdict::Slip => MotorVerdict::Slip,
            TokenMotorVerdict::Insufficient => MotorVerdict::Unknown,
        },
    }
}

/// Slip-score for panel display — drawn from the token signal. `None`
/// only when the engine couldn't compute one (defensive).
fn slip_score_for(record: &LogRecord) -> Option<f64> {
    record.token_motor.map(|s| s.slip_score)
}

/// Decide the tier from the stacked motor + linguistic gates plus
/// occasion count.
///
/// **Held reasons** (in order of evaluation; first match wins so the
/// reported reason is the *most specific* signal):
///   1. `ObviousFragment` — word too short to reason about.
///   2. `SlipSignature` — motor-clean execution required.
///   3. `IllFormed` — bigram plausibility below the floor.
///   4. `SegmentableMerge` — strictly splits into 2+ known words.
///   5. `PrefixMerge` — high-freq known prefix + non-empty suffix.
///   6. `NearKnownWord` — within edit-2 of a high-freq known word.
///
/// Net: a Kept must clear motor AND linguistic to promote.
/// Recurrence (multiple occasions) does NOT bypass any gate — a
/// systematic recurring mangle still holds. The same gates apply on
/// both lanes; slow-lane proximity is implicit (it has a candidate
/// by definition), but a Slow-lane Kept on an ill-formed token
/// still holds.
/// Two-stage tier decision:
///
/// **Stage 1 — Eligibility (veto).** Lane-independent and
/// count-independent. A failing word is `Held` no matter how many
/// times it recurs — recurrence does NOT bypass the eligibility veto.
/// Vetoes:
///   * `ObviousFragment` — length below the minimum.
///   * `SlipSignature` — motor verdict says slip.
///   * `IllFormed` — plausibility below the floor (a backstop for
///     `qzqz`-class anomalies; mean bigram doesn't separate spatial
///     mangles from real words by shape alone).
///   * `SegmentableMerge` / `PrefixMerge` — strictly splits into
///     known parts.
///   * `NearKnownWord` — the **combined** signal "near a known word
///     AND not seen on the web" (`norvig_freq == 0`). Catches
///     `aduluts` / `imapc` / `youbd` while letting `lol`-class
///     informal real words (Norvig-attested) through. The slow lane
///     gets this gate too — the user keeping a near-miss after the
///     engine suggested the correct word is MORE slip-evidence than
///     fast lane, not less.
///
/// **Stage 2 — Promotion.** Only runs if eligibility passed. Lane
/// sets the occasion bar: fast lane confirms at
/// [`CONFIRMED_OCCASIONS_THRESHOLD_FAST`], slow lane at
/// [`CONFIRMED_OCCASIONS_THRESHOLD_SLOW`] (higher — slow lane
/// carries inherent slip-signal). Below the bar → `Provisional`.
#[allow(clippy::too_many_arguments)]
fn recompute_tier(
    word: &str,
    lane: &Lane,
    motor_verdict: MotorVerdict,
    plausibility: f64,
    proximity: ProximityVerdict,
    norvig_freq: u64,
    occasions: u32,
) -> ProposalTier {
    // ---- Stage 1: Eligibility (veto) ----

    if word.chars().count() < MIN_PROMOTABLE_WORD_LEN {
        return ProposalTier::Held {
            reason: HoldReason::ObviousFragment,
        };
    }
    if matches!(motor_verdict, MotorVerdict::Slip) {
        return ProposalTier::Held {
            reason: HoldReason::SlipSignature,
        };
    }
    if plausibility < crate::linguistic::PLAUSIBILITY_FLOOR {
        return ProposalTier::Held {
            reason: HoldReason::IllFormed,
        };
    }
    match proximity {
        ProximityVerdict::Segmentable => {
            return ProposalTier::Held {
                reason: HoldReason::SegmentableMerge,
            };
        }
        ProximityVerdict::PrefixMerge => {
            return ProposalTier::Held {
                reason: HoldReason::PrefixMerge,
            };
        }
        ProximityVerdict::NearKnownEdit2 => {
            // Near-known alone doesn't veto: informal real words
            // (`lol`, `meh`, etc.) sit near a known word AND have
            // genuine web usage. The combined signal "near-known AND
            // no Norvig presence" is what flags a typo. Both lanes
            // get this gate — the slow lane's existing candidate is
            // additional slip evidence, not a free pass.
            if norvig_freq == 0 {
                return ProposalTier::Held {
                    reason: HoldReason::NearKnownWord,
                };
            }
        }
        ProximityVerdict::FarFromKnown => {}
    }

    // ---- Stage 2: Promotion (eligible) ----

    let threshold = match lane {
        Lane::Fast => CONFIRMED_OCCASIONS_THRESHOLD_FAST,
        Lane::Slow { .. } => CONFIRMED_OCCASIONS_THRESHOLD_SLOW,
    };
    if occasions >= threshold {
        ProposalTier::Confirmed
    } else {
        ProposalTier::Provisional
    }
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{DecisionOutcome, LeaveAloneReason};
    use crate::log::{DecisionLedger, LogConfidence};
    use crate::motor_signal::{TokenMotorSignal, TokenMotorVerdict};
    use crate::score::Confidence;
    use crate::ConfidenceTier;

    /// Build a clean-motor signal for `word.len()` chars.
    fn clean_motor(word: &str) -> TokenMotorSignal {
        TokenMotorSignal {
            verdict: TokenMotorVerdict::Clean,
            slip_score: 0.0,
            graze_count: 0,
            char_count: word.chars().count() as u32,
        }
    }

    /// Build a slip-motor signal (one graze) for `word.len()` chars.
    fn slip_motor(word: &str) -> TokenMotorSignal {
        let n = word.chars().count() as u32;
        TokenMotorSignal {
            verdict: TokenMotorVerdict::Slip,
            slip_score: 1.0 / (n as f64).max(1.0),
            graze_count: 1,
            char_count: n,
        }
    }

    /// Append a `LeaveAlone(NoCandidates)`-shaped record — fast lane —
    /// with the supplied per-token motor signal.
    fn append_fast(
        ledger: &mut DecisionLedger,
        word: &str,
        motor: TokenMotorSignal,
    ) -> u64 {
        ledger.append(
            0,
            DecisionOutcome::LeaveAlone {
                original: word.to_string(),
                reason: LeaveAloneReason::NoCandidates,
            },
            0,
            ConfidenceTier::Eager,
            None,
            None,
            None,
            None,
            Some(motor),
        )
    }

    /// Append a `LeaveAlone(BelowActiveTier)` — slow lane — record.
    /// `motor` is the per-token signal; the legacy
    /// `top_motor_evidence` is still set for diagnostic display but no
    /// longer drives the gate.
    fn append_slow(
        ledger: &mut DecisionLedger,
        word: &str,
        candidate: &str,
        legacy_motor_evidence: f64,
        rejected_conf: Confidence,
        motor: TokenMotorSignal,
    ) -> u64 {
        ledger.append(
            0,
            DecisionOutcome::LeaveAlone {
                original: word.to_string(),
                reason: LeaveAloneReason::BelowActiveTier,
            },
            0,
            ConfidenceTier::Cautious,
            Some(candidate.to_string()),
            Some(0.50),
            Some(legacy_motor_evidence),
            Some(rejected_conf),
            Some(motor),
        )
    }

    fn flip_outcome(ledger: &mut DecisionLedger, id: u64, outcome: Outcome) -> LogRecord {
        assert!(ledger.resolve_outcome(id, outcome));
        ledger.get(id).cloned().unwrap()
    }

    // ---- Held-via-motor (the core C5b contract) ------------------------

    #[test]
    fn no_candidate_kept_with_slip_dwells_is_held() {
        // Same fast-lane word as the Provisional case below, but with a
        // slip-shaped per-token signal injected. Motor signature is the
        // ONLY thing that holds — fast lane and single occasion would
        // otherwise promote.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo", slip_motor("Soumyo"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("Soumyo").unwrap();
        assert_eq!(prop.lane, Lane::Fast);
        assert_eq!(prop.motor_verdict, MotorVerdict::Slip);
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::SlipSignature
            }
        );
        // Slip-score surfaces on the panel so the builder can see WHY.
        assert!(prop.last_motor_evidence.is_some());
    }

    #[test]
    fn no_candidate_kept_with_clean_dwells_promotes_provisional() {
        // Same word, clean motor signal → Provisional. The pair of
        // tests deterministically pins the slip-vs-clean axis.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("Soumyo").unwrap();
        assert_eq!(prop.lane, Lane::Fast);
        assert_eq!(prop.motor_verdict, MotorVerdict::Clean);
        assert_eq!(prop.tier, ProposalTier::Provisional);
        assert_eq!(prop.occasions, 1);
        assert_eq!(prop.last_motor_evidence, Some(0.0));
    }

    #[test]
    fn slow_lane_high_confidence_rejection_promotes_when_motor_is_clean() {
        // The earlier `HighConfidenceRejection` hold is gone — clean
        // motor outweighs how confident the engine was that the user
        // typed a typo. One occasion of a HIGH-conf rejection +
        // clean execution → Provisional (NOT Held).
        let mut ledger = DecisionLedger::new();
        let id = append_slow(
            &mut ledger,
            "foo",
            "the",
            0.2,
            Confidence::High,
            clean_motor("foo"),
        );
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("foo").unwrap();
        assert!(matches!(
            prop.lane,
            Lane::Slow {
                rejected_confidence: LogConfidence::High
            }
        ));
        assert_eq!(prop.motor_verdict, MotorVerdict::Clean);
        assert_eq!(prop.tier, ProposalTier::Provisional);
    }

    #[test]
    fn slip_signature_holds_regardless_of_lane_and_count() {
        // Many Kept occasions can't outweigh a slip signature.
        // Motor-only contract: slip beats everything.
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        for _ in 0..5 {
            let id = append_slow(
                &mut ledger,
                "wordl",
                "world",
                0.85,
                Confidence::Medium,
                slip_motor("wordl"),
            );
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        let prop = p.get("wordl").unwrap();
        assert_eq!(prop.motor_verdict, MotorVerdict::Slip);
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::SlipSignature
            }
        );
        // Slip-score is the per-token signal's, NOT top_motor_evidence.
        assert_eq!(prop.last_motor_evidence, Some(1.0 / 5.0));
    }

    // ---- Fast-lane promotion path --------------------------------------

    #[test]
    fn three_kept_occasions_promote_to_confirmed() {
        // Same word kept three times with clean motor → Confirmed. Pin
        // the placeholder threshold so a future tune doesn't silently
        // change behaviour.
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = append_fast(&mut ledger, "Krutrim", clean_motor("Krutrim"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert_eq!(p.get("Krutrim").unwrap().tier, ProposalTier::Confirmed);
        assert_eq!(p.get("Krutrim").unwrap().occasions, 3);
    }

    // ---- Slow-lane promotion path --------------------------------------

    #[test]
    fn slow_lane_clean_motor_promotes_with_low_rejected_confidence() {
        let mut ledger = DecisionLedger::new();
        let id = append_slow(
            &mut ledger,
            "foo",
            "boo",
            0.2,
            Confidence::Low,
            clean_motor("foo"),
        );
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("foo").unwrap();
        assert_eq!(prop.motor_verdict, MotorVerdict::Clean);
        assert_eq!(prop.tier, ProposalTier::Provisional);
    }

    // ---- Reversibility (revisable C5a transitions) ---------------------

    #[test]
    fn kept_then_corrected_retracts_the_kept_contribution() {
        // Critical: a Kept that later flips to Corrected must un-count.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let mut p = LexiconProposer::new();
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_record(&kept);
        assert!(p.get("Soumyo").is_some());

        // Revisable: user later edits to a correction.
        let corrected = flip_outcome(&mut ledger, id, Outcome::CorrectedToOther);
        p.note_record(&corrected);
        assert!(
            p.get("Soumyo").is_none(),
            "the only contributing record retracted → proposal is dropped"
        );
    }

    #[test]
    fn kept_then_abandoned_retracts_the_kept_contribution() {
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let mut p = LexiconProposer::new();
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_record(&kept);

        let abandoned = flip_outcome(&mut ledger, id, Outcome::Abandoned);
        p.note_record(&abandoned);
        assert!(p.get("Soumyo").is_none());
    }

    #[test]
    fn multiple_records_independently_contribute() {
        // Three distinct records for "Soumyo", all Kept → occasions=3.
        // Retract one → occasions=2.
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        let mut ids = Vec::new();
        for _ in 0..3 {
            let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
            ids.push(id);
        }
        assert_eq!(p.get("Soumyo").unwrap().occasions, 3);

        let undone = flip_outcome(&mut ledger, ids[1], Outcome::CorrectedToOther);
        p.note_record(&undone);
        assert_eq!(p.get("Soumyo").unwrap().occasions, 2);
        assert_eq!(p.get("Soumyo").unwrap().tier, ProposalTier::Provisional);
    }

    #[test]
    fn idempotent_repeat_note_returns_no_change() {
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        let mut p = LexiconProposer::new();
        let first = p.note_record(&kept);
        let second = p.note_record(&kept);
        assert!(matches!(first, ProposalUpdate::Changed(_, _)));
        assert!(matches!(second, ProposalUpdate::NoChange));
        assert_eq!(p.get("Soumyo").unwrap().occasions, 1);
    }

    #[test]
    fn pending_record_does_not_credit_but_can_revise_later() {
        // A note on Pending records carries no Kept credit, but the
        // contribution shape is stashed so the eventual Pending→Kept
        // transition counts cleanly.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let pending = ledger.get(id).cloned().unwrap();
        let mut p = LexiconProposer::new();
        p.note_record(&pending);
        assert!(p.get("Soumyo").is_none());

        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_record(&kept);
        assert_eq!(p.get("Soumyo").unwrap().occasions, 1);
    }

    #[test]
    fn corrected_records_do_not_feed_learning() {
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let mut p = LexiconProposer::new();
        let corrected = flip_outcome(&mut ledger, id, Outcome::CorrectedToSuggestion);
        p.note_record(&corrected);
        assert!(p.get("Soumyo").is_none());
    }

    // ---- Obvious fragments ---------------------------------------------

    #[test]
    fn single_character_word_is_held_as_fragment() {
        // The "Soum"/"yo" premature-space case the brief flagged. We
        // don't reconstruct (that's 5c) but we also don't pretend to
        // learn a single-char "word."
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "a", clean_motor("a"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("a").unwrap();
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::ObviousFragment
            }
        );
        // Still recorded — the panel surfaces it so the builder can
        // see how often it's tripping the filter.
        assert_eq!(prop.occasions, 1);
    }

    // ---- Linguistic gate (calibration corpus) --------------------------
    //
    // The motor gate alone was leaking garbage from natural typing —
    // `themach` / `imapc` / `potentjual` all "clean / 0.00" reached
    // Provisional. These tests deterministically pin the linguistic
    // stack's job: a real novel name promotes; a spatial mangle, a
    // dropped-space merge, and a high-frequency-prefix merge all
    // HELD despite clean motor.

    #[test]
    fn novel_name_with_clean_motor_promotes_provisional() {
        // Soumyo: not in SCOWL, not in seed, not near a high-freq
        // known word, not segmentable. Net: FarFromKnown +
        // well-formed + clean motor → Provisional.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("Soumyo").unwrap();
        assert_eq!(prop.proximity, ProximityVerdict::FarFromKnown);
        assert_eq!(prop.tier, ProposalTier::Provisional);
    }

    #[test]
    fn spatial_mangle_held_near_known_word_despite_clean_motor() {
        // imapc is edit-2 of `impact` (61M-freq). Clean motor doesn't
        // override; held(NearKnownWord).
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "imapc", clean_motor("imapc"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("imapc").unwrap();
        assert_eq!(prop.motor_verdict, MotorVerdict::Clean);
        assert_eq!(prop.proximity, ProximityVerdict::NearKnownEdit2);
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::NearKnownWord
            }
        );
    }

    #[test]
    fn dropped_space_merge_held_segmentable_despite_clean_motor() {
        // andthe: `and`+`the`, both high-freq → strictly segmentable.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "andthe", clean_motor("andthe"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("andthe").unwrap();
        assert_eq!(prop.motor_verdict, MotorVerdict::Clean);
        assert_eq!(prop.proximity, ProximityVerdict::Segmentable);
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::SegmentableMerge
            }
        );
    }

    #[test]
    fn prefix_merge_held_themach_despite_clean_motor() {
        // themach: `the` high-freq prefix + `mach` (not in SCOWL) →
        // prefix-merge.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "themach", clean_motor("themach"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("themach").unwrap();
        assert_eq!(prop.motor_verdict, MotorVerdict::Clean);
        assert_eq!(prop.proximity, ProximityVerdict::PrefixMerge);
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::PrefixMerge
            }
        );
    }

    #[test]
    fn systematic_recurring_mangle_does_not_promote_on_recurrence() {
        // The C5b brief: "a systematic slip recurs too, but it carries
        // its signature every time." Three Kept occasions of `imapc`
        // would otherwise hit Confirmed — but each occasion carries
        // the near-known signature, so each one holds. Recurrence
        // alone does NOT bypass the linguistic gate.
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST + 2 {
            let id = append_fast(&mut ledger, "imapc", clean_motor("imapc"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        let prop = p.get("imapc").unwrap();
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::NearKnownWord
            },
            "5 occasions of imapc must still be held — recurrence alone \
             doesn't bypass the linguistic gate"
        );
    }

    #[test]
    fn slow_lane_near_known_held_when_no_web_freq() {
        // `aduluts` (the user's reported leak): slow lane (candidate
        // "adults"), clean motor, plausibility well above the floor
        // by mean bigram, but ZERO Norvig frequency — never seen on
        // the web. The combined "near-known + no web presence" signal
        // catches it. Slow-lane proximity must hold here.
        let mut ledger = DecisionLedger::new();
        let id = append_slow(
            &mut ledger,
            "aduluts",
            "adults",
            0.5,
            Confidence::High,
            clean_motor("aduluts"),
        );
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("aduluts").unwrap();
        assert_eq!(prop.motor_verdict, MotorVerdict::Clean);
        assert_eq!(prop.proximity, ProximityVerdict::NearKnownEdit2);
        assert_eq!(
            prop.norvig_freq, 0,
            "aduluts must have zero Norvig freq (purely a typo)"
        );
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::NearKnownWord
            }
        );
    }

    #[test]
    fn slow_lane_near_known_promotes_when_word_has_web_freq() {
        // `lol` is the counter-example: slow lane (candidate "lot"),
        // clean motor, near-known — but Norvig freq is 16M (genuine
        // informal use on the web). The combo signal doesn't fire;
        // promotion proceeds normally. This is the "informal real
        // word near a known one" the brief said should still be
        // promotable.
        let mut ledger = DecisionLedger::new();
        let id = append_slow(
            &mut ledger,
            "lol",
            "lot",
            0.4,
            Confidence::Medium,
            clean_motor("lol"),
        );
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("lol").unwrap();
        assert!(
            prop.norvig_freq > 0,
            "lol has documented web frequency (Norvig 16M)"
        );
        // Single occasion → Provisional (slow lane confirmation
        // threshold is higher; 5 occasions needed for Confirmed).
        assert_eq!(prop.tier, ProposalTier::Provisional);
    }

    #[test]
    fn slow_lane_confirmation_requires_more_occasions_than_fast() {
        // Same lol case: three occasions still Provisional on slow
        // lane (would be Confirmed on fast lane). Five occasions
        // confirm.
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = append_slow(
                &mut ledger,
                "lol",
                "lot",
                0.4,
                Confidence::Medium,
                clean_motor("lol"),
            );
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert_eq!(p.get("lol").unwrap().tier, ProposalTier::Provisional);

        for _ in 0..(CONFIRMED_OCCASIONS_THRESHOLD_SLOW - CONFIRMED_OCCASIONS_THRESHOLD_FAST) {
            let id = append_slow(
                &mut ledger,
                "lol",
                "lot",
                0.4,
                Confidence::Medium,
                clean_motor("lol"),
            );
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert_eq!(p.get("lol").unwrap().tier, ProposalTier::Confirmed);
    }

    #[test]
    fn two_char_word_held_as_fragment_regardless_of_lane() {
        // `un` (the user's reported leak): 2 chars. The eligibility
        // veto holds it as a fragment no matter how many times it
        // recurs — count never bypasses eligibility.
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_SLOW + 2 {
            let id = append_slow(
                &mut ledger,
                "un",
                "an",
                0.6,
                Confidence::High,
                clean_motor("un"),
            );
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        let prop = p.get("un").unwrap();
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::ObviousFragment
            },
            "`un` is 2 chars — held as fragment regardless of recurrence count"
        );
    }

    #[test]
    fn slow_lane_segmentable_held_despite_clean_motor() {
        // The user's tightening on the slow lane: an ill-formed or
        // near-known kept-despite-suggestion word shouldn't confirm on
        // recurrence alone. Use `andthe` — a slow-lane record where
        // some candidate exists but the linguistic gate still
        // identifies it as a merge.
        let mut ledger = DecisionLedger::new();
        let id = append_slow(
            &mut ledger,
            "andthe",
            "another", // a plausible candidate
            0.2,
            Confidence::Low,
            clean_motor("andthe"),
        );
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("andthe").unwrap();
        assert_eq!(prop.motor_verdict, MotorVerdict::Clean);
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::SegmentableMerge
            },
            "slow-lane segmentation must hold the same way fast lane does"
        );
    }

    // ---- snapshot ordering ---------------------------------------------

    #[test]
    fn snapshot_orders_by_most_recent_first() {
        let _lex = Lexicon::shared();
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        let id1 = ledger.append(
            10,
            DecisionOutcome::LeaveAlone {
                original: "alpha".to_string(),
                reason: LeaveAloneReason::NoCandidates,
            },
            0,
            ConfidenceTier::Eager,
            None,
            None,
            None,
            None,
            Some(clean_motor("alpha")),
        );
        let kept1 = flip_outcome(&mut ledger, id1, Outcome::Kept);
        p.note_record(&kept1);
        let id2 = ledger.append(
            20,
            DecisionOutcome::LeaveAlone {
                original: "beta".to_string(),
                reason: LeaveAloneReason::NoCandidates,
            },
            0,
            ConfidenceTier::Eager,
            None,
            None,
            None,
            None,
            Some(clean_motor("beta")),
        );
        let kept2 = flip_outcome(&mut ledger, id2, Outcome::Kept);
        p.note_record(&kept2);

        let snap = p.snapshot();
        assert_eq!(snap[0].word, "beta");
        assert_eq!(snap[1].word, "alpha");
    }
}
