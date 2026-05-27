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
//! ## Motor verdict (modulates the bar, both lanes)
//!
//! The proposer reads [`LogRecord::top_motor_evidence`] — the same `[0,1]`
//! "this typed word is a plausible motor slip of the top candidate"
//! score C3 uses to justify a correction. The reading is intentionally
//! re-used; we don't invent a new motor signal.
//!
//! | `top_motor_evidence` | verdict        | meaning                          |
//! |----------------------|----------------|----------------------------------|
//! | `≥ 0.6`              | `Slip`         | Typed word fits a known slip shape (high prior the user slipped). |
//! | `≤ 0.4`              | `Clean`        | No slip signature — fingers executed deliberately. |
//! | `> 0.4 && < 0.6`     | `Mixed`        | Inconclusive. Lean clean for promotion (cautious). |
//! | `None`               | `Unknown`      | No candidate to compute slip-shape against. Fast-lane default. |
//!
//! Thresholds are **PLACEHOLDERS** — chosen to bracket the C3 floors
//! (`CONFIDENCE_LOW_FLOOR = 0.25`, `CONFIDENCE_MEDIUM_FLOOR = 0.5`)
//! conservatively. Tune from real Observing data; the panel surfaces the
//! raw score so we can see what threshold actually splits intended-vs-slip.
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

use crate::log::{LogConfidence, LogRecord, Outcome};

/// Version of the proposal wire shape. Bump on any change to
/// [`LexiconProposal`] / [`Lane`] / [`MotorVerdict`] / [`ProposalTier`].
pub const LEXICON_PROPOSAL_VERSION: u32 = 1;

// ---- Tunable PLACEHOLDERS — replace with values from real Observing data --

/// `top_motor_evidence >= this` → motor verdict `Slip`. Brackets the
/// medium-confidence floor where C3 would consider a correction
/// plausible — a slip-like Kept under that bar should hold for review.
const MOTOR_SLIP_THRESHOLD: f64 = 0.6;
/// `top_motor_evidence <= this` → motor verdict `Clean`. Below the
/// engine's lowest correction floor; the fingers didn't fit any
/// slip-shape the candidate set offered.
const MOTOR_CLEAN_THRESHOLD: f64 = 0.4;

/// Minimum word length to be eligible for promotion. Single-letter
/// tokens are routinely premature-space boundary fragments (the
/// `Soum`/`yo` case the C5b brief flagged for C5c) and shouldn't
/// learn. Strictly less than this → `Held(ObviousFragment)`.
const MIN_PROMOTABLE_WORD_LEN: usize = 2;

/// Occasions at which a `Provisional` proposal upgrades to `Confirmed`.
/// Placeholder — real corroboration wants distinct time-bins (5d).
const CONFIRMED_OCCASIONS_THRESHOLD: u32 = 3;

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
/// the builder can see exactly which gate fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    /// Word length below [`MIN_PROMOTABLE_WORD_LEN`]. Likely a
    /// premature-space boundary fragment (e.g. `Soum` from `Soumyo`);
    /// 5c will reconstruct, 5b just refuses to learn.
    ObviousFragment,
    /// Latest occasion's motor signature is slip-like.
    SlipSignature,
    /// Slow lane + rejected a HIGH-confidence suggestion + corroboration
    /// hasn't accumulated. Even clean execution shouldn't promote here
    /// after only one or two occurrences — wait for more.
    HighConfidenceRejection,
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
#[derive(Debug, Default)]
pub struct LexiconProposer {
    proposals: HashMap<String, LexiconProposal>,
    record_contributions: HashMap<u64, RecordContribution>,
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
        Self::default()
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
        let last_motor_evidence = record.top_motor_evidence;
        let last_record_id = record.id;
        let last_seen_ms = record.timestamp_ms;

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
                version: LEXICON_PROPOSAL_VERSION,
            }
        });

        entry.occasions += 1;
        entry.lane = lane;
        entry.motor_verdict = motor_verdict;
        entry.last_motor_evidence = last_motor_evidence;
        entry.last_record_id = last_record_id;
        entry.last_seen_ms = last_seen_ms;
        entry.tier = recompute_tier(&entry.word, &entry.lane, entry.motor_verdict, entry.occasions);
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
                p.tier = recompute_tier(&p.word, &p.lane, p.motor_verdict, p.occasions);
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

/// Map `top_motor_evidence` → [`MotorVerdict`] using the placeholder
/// thresholds at the top of this file.
fn motor_verdict_for(record: &LogRecord) -> MotorVerdict {
    match record.top_motor_evidence {
        None => MotorVerdict::Unknown,
        Some(m) if m >= MOTOR_SLIP_THRESHOLD => MotorVerdict::Slip,
        Some(m) if m <= MOTOR_CLEAN_THRESHOLD => MotorVerdict::Clean,
        Some(_) => MotorVerdict::Mixed,
    }
}

/// Decide the tier given lane + motor verdict + occasions. Pure
/// function — the proposer state is just bookkeeping around this.
fn recompute_tier(
    word: &str,
    lane: &Lane,
    motor_verdict: MotorVerdict,
    occasions: u32,
) -> ProposalTier {
    // Gate 1: obvious fragments — single-character tokens are
    // overwhelmingly premature-space breaks; we refuse to learn them.
    // (Empty would also fall here, but the tokenizer rejects empty
    // cores upstream.)
    if word.chars().count() < MIN_PROMOTABLE_WORD_LEN {
        return ProposalTier::Held {
            reason: HoldReason::ObviousFragment,
        };
    }

    // Gate 2: slip signature. Clean execution can outweigh a
    // high-confidence suggestion ("if the fingers did it cleanly, he
    // meant it"); a slip signature blocks regardless of lane or count.
    if matches!(motor_verdict, MotorVerdict::Slip) {
        return ProposalTier::Held {
            reason: HoldReason::SlipSignature,
        };
    }

    // Gate 3: slow lane with high-confidence rejection needs
    // corroboration — even clean execution shouldn't promote a
    // single rejection of a HIGH-confidence suggestion.
    if let Lane::Slow {
        rejected_confidence: LogConfidence::High,
    } = lane
    {
        if occasions < CONFIRMED_OCCASIONS_THRESHOLD {
            return ProposalTier::Held {
                reason: HoldReason::HighConfidenceRejection,
            };
        }
    }

    // Gate 4: enough occasions → Confirmed; otherwise Provisional.
    if occasions >= CONFIRMED_OCCASIONS_THRESHOLD {
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
    use crate::score::Confidence;
    use crate::ConfidenceTier;

    /// Append a `LeaveAlone(NoCandidates)`-shaped record — fast lane.
    fn append_fast(ledger: &mut DecisionLedger, word: &str) -> u64 {
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
        )
    }

    /// Append a `LeaveAlone(BelowActiveTier)`-shaped record — slow lane
    /// (engine had a candidate, mode declined, user kept the original).
    fn append_slow(
        ledger: &mut DecisionLedger,
        word: &str,
        candidate: &str,
        motor: f64,
        rejected_conf: Confidence,
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
            Some(motor),
            Some(rejected_conf),
        )
    }

    fn flip_outcome(ledger: &mut DecisionLedger, id: u64, outcome: Outcome) -> LogRecord {
        assert!(ledger.resolve_outcome(id, outcome));
        ledger.get(id).cloned().unwrap()
    }

    // ---- Fast lane ------------------------------------------------------

    #[test]
    fn no_candidate_kept_lands_provisional_clean_fast_lane() {
        // Soumyo: name, no edit-1 neighbour, user kept it. Fast lane,
        // motor verdict Unknown (no candidate to compute slip-shape).
        // Single occasion → Provisional.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo");
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        let upd = p.note_record(&kept);
        let prop = p.get("Soumyo").unwrap();
        assert_eq!(prop.lane, Lane::Fast);
        assert_eq!(prop.motor_verdict, MotorVerdict::Unknown);
        assert_eq!(prop.tier, ProposalTier::Provisional);
        assert_eq!(prop.occasions, 1);
        assert!(prop.last_motor_evidence.is_none());
        assert!(matches!(upd, ProposalUpdate::Changed(Some(_), _)));
    }

    #[test]
    fn three_kept_occasions_promote_to_confirmed() {
        // Same word kept three times → Confirmed. Pin the placeholder
        // threshold so a future tune doesn't silently change behaviour
        // without updating the test.
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD {
            let id = append_fast(&mut ledger, "Krutrim");
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert_eq!(p.get("Krutrim").unwrap().tier, ProposalTier::Confirmed);
        assert_eq!(p.get("Krutrim").unwrap().occasions, 3);
    }

    // ---- Slow lane ------------------------------------------------------

    #[test]
    fn slow_lane_clean_motor_promotes_when_low_rejected_confidence() {
        // User kept "foo" even though engine had a LOW-confidence
        // suggestion "boo". Clean motor (0.2) → not Held(Slip); LOW
        // rejection → not Held(HighConfidenceRejection); single
        // occasion → Provisional.
        let mut ledger = DecisionLedger::new();
        let id = append_slow(&mut ledger, "foo", "boo", 0.2, Confidence::Low);
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("foo").unwrap();
        assert!(matches!(prop.lane, Lane::Slow { rejected_confidence: LogConfidence::Low }));
        assert_eq!(prop.motor_verdict, MotorVerdict::Clean);
        assert_eq!(prop.tier, ProposalTier::Provisional);
    }

    #[test]
    fn slow_lane_high_confidence_rejection_holds_until_corroborated() {
        // Rejecting a HIGH-confidence suggestion needs corroboration.
        // Even with clean motor, the first Kept holds.
        let mut ledger = DecisionLedger::new();
        let id = append_slow(&mut ledger, "foo", "the", 0.2, Confidence::High);
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("foo").unwrap();
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::HighConfidenceRejection
            }
        );

        // Two more occasions → corroborated → Confirmed (jumps past
        // Provisional because the HighConfidenceRejection gate clears
        // exactly when CONFIRMED_OCCASIONS_THRESHOLD lands).
        for _ in 0..2 {
            let id = append_slow(&mut ledger, "foo", "the", 0.2, Confidence::High);
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert_eq!(p.get("foo").unwrap().tier, ProposalTier::Confirmed);
    }

    #[test]
    fn slip_signature_holds_even_with_clean_history() {
        // Motor evidence above the slip threshold → Held(SlipSignature)
        // regardless of lane or occasion count.
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        for _ in 0..5 {
            let id = append_slow(&mut ledger, "wordl", "world", 0.85, Confidence::Medium);
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
    }

    // ---- Reversibility (revisable C5a transitions) ---------------------

    #[test]
    fn kept_then_corrected_retracts_the_kept_contribution() {
        // Critical: a Kept that later flips to Corrected must un-count.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "Soumyo");
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
        let id = append_fast(&mut ledger, "Soumyo");
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
            let id = append_fast(&mut ledger, "Soumyo");
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
        let id = append_fast(&mut ledger, "Soumyo");
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
        let id = append_fast(&mut ledger, "Soumyo");
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
        let id = append_fast(&mut ledger, "Soumyo");
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
        let id = append_fast(&mut ledger, "a");
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

    // ---- snapshot ordering ---------------------------------------------

    #[test]
    fn snapshot_orders_by_most_recent_first() {
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        // First word at t=10.
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
        );
        let kept1 = flip_outcome(&mut ledger, id1, Outcome::Kept);
        p.note_record(&kept1);
        // Second word at t=20.
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
        );
        let kept2 = flip_outcome(&mut ledger, id2, Outcome::Kept);
        p.note_record(&kept2);

        let snap = p.snapshot();
        assert_eq!(snap[0].word, "beta");
        assert_eq!(snap[1].word, "alpha");
    }
}
