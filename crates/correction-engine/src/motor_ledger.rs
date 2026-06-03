//! Component 5c — **motor ledger**: the motor map's own record stream.
//!
//! The C5b [`crate::log::DecisionLedger`] only admits *unknown* words (the
//! lexicon proposer's concern, gated by [`crate::log::should_log`]). The
//! motor map needs the opposite: **every** real keystroke is motor data, so
//! a known word typed cleanly is exactly the `Kept` signal we want. Routing
//! the motor map through the decision ledger starved it — a fluent typist
//! produced almost no observations (the "sealed → verdict" cliff the capture
//! funnel exposed; CLAUDE.md Principle #8).
//!
//! So the motor map gets its own **lean** ledger: one record per sealed,
//! motor-evidenced Word/Acronym (known *and* unknown), carrying only what the
//! shared [`crate::resolver::OutcomeResolver`] needs to assign a verdict —
//! `anchor_id` + `original_text` — plus the resolvable `outcome` slot. No
//! candidate, no score, no decision arm: for motor purposes a correction is a
//! slip regardless of whether it matched the engine's suggestion, so
//! [`MotorRecord`]'s `top_candidate` is always `None` and every correction
//! resolves to `CorrectedToOther`.
//!
//! The resolver is shared (one verdict state machine, two ledgers). The
//! decision ledger feeds the lexicon proposer; this one feeds the motor map.
//! Unknown words appear in both — different consumers, different needs.

use std::collections::VecDeque;

use crate::log::Outcome;
use crate::resolver::ResolvableRecord;

/// Default capacity. Records are resolved within seconds (idle verdicts),
/// so the ring only needs to outrun the in-flight (unresolved) window;
/// 512 covers a long burst with wide margin. Evicting *resolved* records is
/// harmless — they've already been observed.
pub const DEFAULT_MOTOR_LEDGER_CAPACITY: usize = 512;

/// One lean motor record: a sealed, motor-evidenced word the resolver will
/// assign a verdict to, for the motor map to observe.
#[derive(Debug, Clone)]
pub struct MotorRecord {
    /// Monotonic per [`MotorLedger`]. Independent of the decision ledger's
    /// id space — an unknown word has a record in *both* ledgers with
    /// different ids.
    pub id: u64,
    /// The C2 [`crate::SpanAnchor`] id this word sealed at — the resolver's
    /// re-association key.
    pub anchor_id: u32,
    /// The word as typed (case-preserved core from the tokenizer).
    pub original_text: String,
    /// Wall-clock seal time (ms since epoch).
    pub timestamp_ms: u64,
    /// Resolver outcome slot. Starts [`Outcome::Pending`]; the resolver
    /// transitions it. Revisable, like the decision ledger.
    pub outcome: Outcome,
}

impl ResolvableRecord for MotorRecord {
    fn record_id(&self) -> u64 {
        self.id
    }
    fn anchor_id(&self) -> u32 {
        self.anchor_id
    }
    fn original_text(&self) -> &str {
        &self.original_text
    }
    /// Always `None` — the motor map is candidate-agnostic, so every
    /// correction reads as a `CorrectedToOther` slip (never
    /// `CorrectedToSuggestion`, which is purely a lexicon judgement).
    fn top_candidate(&self) -> Option<&str> {
        None
    }
    fn current_outcome(&self) -> Outcome {
        self.outcome
    }
}

/// Bounded rolling motor ledger — same shape as [`crate::log::DecisionLedger`]
/// but lean. Owned by the engine; cleared on restart (in-memory only).
#[derive(Debug)]
pub struct MotorLedger {
    records: VecDeque<MotorRecord>,
    next_id: u64,
    capacity: usize,
}

impl Default for MotorLedger {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_MOTOR_LEDGER_CAPACITY)
    }
}

impl MotorLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "motor ledger capacity must be > 0");
        Self {
            records: VecDeque::with_capacity(capacity),
            next_id: 0,
            capacity,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Append a `Pending` record for a freshly sealed word. Returns the new
    /// id. Evicts the oldest record once full.
    pub fn append(&mut self, timestamp_ms: u64, anchor_id: u32, original_text: String) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(MotorRecord {
            id,
            anchor_id,
            original_text,
            timestamp_ms,
            outcome: Outcome::Pending,
        });
        id
    }

    /// Transition the outcome slot of record `id`. Revisable; latest write
    /// wins. Returns `true` if the record was found. O(n) scan — the ledger
    /// is small and resolutions are infrequent.
    pub fn resolve_outcome(&mut self, id: u64, outcome: Outcome) -> bool {
        for r in self.records.iter_mut() {
            if r.id == id {
                r.outcome = outcome;
                return true;
            }
        }
        false
    }

    /// Iterate records oldest → newest (the resolver's input).
    pub fn iter(&self) -> impl Iterator<Item = &MotorRecord> {
        self.records.iter()
    }

    pub fn get(&self, id: u64) -> Option<&MotorRecord> {
        self.records.iter().find(|r| r.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_assigns_monotonic_ids_and_starts_pending() {
        let mut l = MotorLedger::new();
        let a = l.append(0, 10, "morning".into());
        let b = l.append(0, 11, "light".into());
        assert_eq!((a, b), (0, 1));
        assert_eq!(l.get(a).unwrap().outcome, Outcome::Pending);
        assert_eq!(l.get(a).unwrap().original_text, "morning");
    }

    #[test]
    fn resolve_outcome_transitions_and_reports_found() {
        let mut l = MotorLedger::new();
        let id = l.append(0, 10, "morning".into());
        assert!(l.resolve_outcome(id, Outcome::Kept));
        assert_eq!(l.get(id).unwrap().outcome, Outcome::Kept);
        assert!(!l.resolve_outcome(999, Outcome::Kept));
    }

    #[test]
    fn evicts_oldest_when_full() {
        let mut l = MotorLedger::with_capacity(2);
        let a = l.append(0, 1, "a".into());
        l.append(0, 2, "b".into());
        l.append(0, 3, "c".into()); // evicts `a`
        assert!(l.get(a).is_none());
        assert_eq!(l.len(), 2);
    }

    #[test]
    fn motor_record_is_candidate_agnostic() {
        let r = MotorRecord {
            id: 0,
            anchor_id: 1,
            original_text: "teh".into(),
            timestamp_ms: 0,
            outcome: Outcome::Pending,
        };
        // A motor record never carries a candidate → corrections resolve as
        // CorrectedToOther (slips), never CorrectedToSuggestion.
        assert_eq!(r.top_candidate(), None);
    }
}
