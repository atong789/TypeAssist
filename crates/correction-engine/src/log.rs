//! Decision ledger — Component 4 of the L4 brief.
//!
//! An **in-memory, observe-only** rolling log of every L4 decision made on
//! an UNKNOWN Word token. Each record carries enough context for Component
//! 5 (outcome resolution) to come back later and tell the difference
//! between five states:
//!   * `Pending` — outcome not yet observed; the initial state.
//!   * `Kept` — the engine left the word alone and the user kept it.
//!   * `CorrectedToSuggestion` — the user backspaced and arrived at the
//!     engine's `top_candidate`.
//!   * `CorrectedToOther` — the user backspaced and arrived at something
//!     else (engine was wrong about both *whether* and *what* to suggest).
//!   * `Abandoned` — the user deleted the whole word.
//!
//! **Privacy and scope.** Process-memory only; capped at
//! [`DEFAULT_LEDGER_CAPACITY`] most-recent records; cleared on restart.
//! No disk writes, no telemetry. Known-word leave-alones are excluded as
//! noise: if the user typed something the lexicon already approves of,
//! the engine has nothing to learn. Unknown words *with no candidate set*
//! ([`LeaveAloneReason::NoCandidates`]) are deliberately included — names
//! like "Soumyo" land here, and C5 needs them to attribute a `Kept`.
//!
//! **Motor-evidence gate.** A C4 guard against capturing pasted /
//! auto-filled / synthetic text. Pasted characters arrive without
//! press/release timing → `dwell_ms == 0` per char. The engine maintains
//! a per-line dwell buffer (`line_dwells`) parallel to `line_buf`; a
//! token's evidence is "real" iff every char in its `[start, end)` span
//! had a non-zero dwell. See [`has_motor_evidence`]. The full
//! paste-detector (multi-char `key` arms, clipboard signals, IME) is
//! Component 5.
//!
//! ## What lives where
//!
//! * [`LogRecord`] — one row per decision. Carries the full
//!   [`DecisionOutcome`] (no enum duplication), the candidate's
//!   confidence band, the [`SpanAnchor`] id (the bridge C5 uses to
//!   resolve outcomes), the active tier at decision time, and an
//!   [`Outcome`] slot initialised to [`Outcome::Pending`].
//! * [`DecisionLedger`] — the bounded rolling store. `append` writes
//!   Pending records (the only state C4 writes); `resolve_outcome` is
//!   the C5 hook (defined here, not called from C4).
//! * [`should_log`] — the upstream gate (UNKNOWN-word + motor evidence).
//!   Pure function so it's testable without the engine harness.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::decision::{DecisionOutcome, LeaveAloneReason};
use crate::score::Confidence;
use crate::ConfidenceTier;

/// Version of the log record shape. Bump on any change to [`LogRecord`]
/// or the [`Outcome`] / [`LogConfidence`] enums.
///
/// v2 — Component 5a adds [`Outcome::Abandoned`] for the whole-word-delete
/// terminal state.
///
/// v3 — Component 5b first cut adds `top_motor_evidence` (C3's
/// candidate-dependent motor score) so the proposer could read motor
/// info without re-fetching the score report.
///
/// v4 — Component 5b fix: `top_motor_evidence` was blank for the
/// fast lane (no candidate → no edit-shape to score). Adds
/// `token_motor`, a candidate-INDEPENDENT per-token motor signal
/// computed from `line_dwells`. Every loggable record now carries a
/// motor verdict; the proposer reads `token_motor.verdict` rather
/// than `top_motor_evidence` to decide hold vs promote.
pub const LOG_VERSION: u32 = 4;

/// **PLACEHOLDER capacity.** A few hundred records — enough to span a
/// typical writing session without growing unbounded. Tune from real
/// Observing data once C5 is in.
pub const DEFAULT_LEDGER_CAPACITY: usize = 256;

/// Confidence band recorded alongside the decision.
///
/// Mirrors [`crate::score::Confidence`] but adds [`LogConfidence::BelowFloor`]
/// so the ledger can record decisions where the top candidate's score
/// didn't clear the lowest floor *and* the `LeaveAlone(NoCandidates)`
/// case (no top candidate at all). The precise reason lives in
/// [`LogRecord::decision`]; this band is the bucketised glanceable view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogConfidence {
    High,
    Medium,
    Low,
    /// Top score below [`crate::CONFIDENCE_LOW_FLOOR`], or no candidate at all.
    BelowFloor,
}

impl LogConfidence {
    /// Map an optional per-candidate [`Confidence`] (which is `None` when
    /// the top score didn't clear the floor) to a 4-state band.
    pub fn from_optional(c: Option<Confidence>) -> Self {
        match c {
            Some(Confidence::High) => LogConfidence::High,
            Some(Confidence::Medium) => LogConfidence::Medium,
            Some(Confidence::Low) => LogConfidence::Low,
            None => LogConfidence::BelowFloor,
        }
    }
}

/// Per-decision outcome slot. All five states are load-bearing — Component
/// 5's [`crate::resolver::OutcomeResolver`] watches the [`SpanAnchor`] for
/// the user's subsequent edits and transitions through these. **C4 only
/// ever writes [`Outcome::Pending`];** transitions are Component 5's job.
///
/// Resolution is **revisable**: a record may flip back and forth (e.g.
/// `Kept` → `CorrectedToOther` if the user later edits the span) until the
/// anchor is retired by a line reset, at which point the last resolution
/// stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// Not yet observed. Initial state.
    Pending,
    /// User kept the typed text as-is — the anchor's text-now still
    /// matches `original_text`. The engine's leave-alone was correct;
    /// a would-correct that ended here means the user disagreed.
    Kept,
    /// User self-corrected to the engine's `top_candidate`. The strongest
    /// possible positive learning signal.
    CorrectedToSuggestion,
    /// User self-corrected, but to something other than the suggestion.
    /// Engine was wrong about *what* to suggest. Also catches the
    /// `Void(Split)` / `Void(Merge)` cases where the original word
    /// boundary is gone and there's no reliable single-word read-back —
    /// the user clearly didn't keep it as-is, but the post-edit text
    /// can't be matched against the suggestion.
    CorrectedToOther,
    /// User deleted the whole word (anchor voided with `Deleted`). No
    /// content remains at the original span. The engine learns nothing
    /// about *what* to suggest, only that the user walked away.
    Abandoned,
}

/// One row in the ledger — the full structured record of a single
/// L4 decision on an unknown Word token.
///
/// `decision` is the unmodified [`DecisionOutcome`] from
/// [`crate::decision::decide`]; we do **not** duplicate the enum. Fields
/// like `top_candidate`, `top_score`, and `confidence` are convenience
/// projections so the panel doesn't have to pattern-match on every row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    /// Monotonic per [`DecisionLedger`]. Used by [`DecisionLedger::resolve_outcome`]
    /// to find the record C5 wants to update.
    pub id: u64,
    /// Wall-clock timestamp (ms since UNIX epoch). The engine fills this
    /// at append time; the ledger itself is timestamp-agnostic.
    pub timestamp_ms: u64,
    /// The word the user actually typed (case-preserved core from the
    /// tokenizer). The same string lives inside `decision.original()`;
    /// copied to the top level for cheap rendering.
    pub original_text: String,
    /// Full [`DecisionOutcome`] — reuse, don't duplicate. Carries either
    /// the `WouldCorrect` arm (with `suggested` + confidence + score) or
    /// the `LeaveAlone` arm (with `reason`).
    pub decision: DecisionOutcome,
    /// Engine's top candidate text — the strongest edit-1 neighbour the
    /// score pass turned up, **regardless of whether the mode acted on
    /// it**. Present whenever `score_candidates` produced at least one
    /// candidate; `None` only for `LeaveAlone(NoCandidates)`. The
    /// resolver matches the user's post-edit text against this string
    /// to attribute `CorrectedToSuggestion` — crucially, that includes
    /// the `LeaveAlone(BelowActiveTier)` arm where the mode declined to
    /// suggest but the candidate was right anyway.
    pub top_candidate: Option<String>,
    /// Engine's top candidate raw score. Same nullability as
    /// `top_candidate`: present whenever a candidate existed, absent
    /// only when none did. C5 uses this to weight learning later.
    pub top_score: Option<f64>,
    /// **Motor evidence** of the top candidate's edit shape (sub / trans
    /// / ins / del) — the same `[0, 1]` value C3 uses to justify a
    /// correction. High = the typed word is a plausible motor slip of
    /// a real word; low = no slip-shape match against the candidate.
    /// Same nullability as `top_candidate`. **Diagnostic** for the
    /// debug panel; the C5b proposer reads [`Self::token_motor`]
    /// instead (the candidate-independent signal) so the fast lane
    /// has a verdict too.
    pub top_motor_evidence: Option<f64>,
    /// **Per-token motor cleanliness** computed from the keystrokes
    /// themselves — `line_dwells[start..end]`. Candidate-independent,
    /// present on every loggable record (fast lane included). The
    /// C5b proposer reads `token_motor.verdict` to decide
    /// hold-as-slip vs promote-as-clean; the proposer no longer
    /// consults `top_motor_evidence` for the gate.
    ///
    /// `None` only when the engine couldn't read the dwell slice for
    /// the token's span (defensive — should not happen in normal
    /// flow). A 1-char token still gets `Some(_)` with verdict
    /// `Insufficient` so the panel renders the right cell.
    pub token_motor: Option<crate::motor_signal::TokenMotorSignal>,
    /// 4-state confidence band. See [`LogConfidence`]. Always present —
    /// `BelowFloor` covers both "below floor" and "no candidate".
    pub confidence: LogConfidence,
    /// The Component-2 [`crate::SpanAnchor`] id this decision was made
    /// on. C5 watches this anchor's text-now and void state to resolve
    /// the outcome. Required — if the anchor wasn't cleanly available
    /// at decision time, the engine doesn't append (and tells the
    /// builder rather than hacking a link).
    pub anchor_id: u32,
    /// Active engine tier ([`ConfidenceTier`]) at the moment of
    /// decision. Recorded so the C5 attribution survives later tier
    /// changes (Cautious one minute ago, Eager now — the historical
    /// decision was made under the older bar).
    pub active_tier: ConfidenceTier,
    /// Outcome slot. C4 always writes [`Outcome::Pending`]; C5 calls
    /// [`DecisionLedger::resolve_outcome`] to transition.
    pub outcome: Outcome,
    /// Shape version — bump [`LOG_VERSION`] alongside any change to
    /// [`LogRecord`] / [`Outcome`] / [`LogConfidence`].
    pub log_version: u32,
}

/// Bounded rolling decision ledger.
///
/// Owned by the engine; capacity is fixed at construction. Once full,
/// the oldest record is evicted to make room — by design, no disk
/// fallback. Records are append-only with one exception: C5 may call
/// [`Self::resolve_outcome`] to transition the outcome slot.
pub struct DecisionLedger {
    records: VecDeque<LogRecord>,
    next_id: u64,
    capacity: usize,
}

impl Default for DecisionLedger {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_LEDGER_CAPACITY)
    }
}

impl DecisionLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "ledger capacity must be > 0");
        Self {
            records: VecDeque::with_capacity(capacity),
            next_id: 0,
            capacity,
        }
    }

    /// Maximum records the ledger will retain. The (capacity + 1)-th
    /// `append` evicts the oldest.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Iterate records oldest → newest. The debug panel renders this
    /// order with newest at the bottom (matches FEED).
    pub fn iter(&self) -> impl Iterator<Item = &LogRecord> {
        self.records.iter()
    }

    /// Append a Pending record built from the engine's decision context.
    /// Returns the new record's `id` so the caller can correlate it
    /// later (e.g. the engine emits the wire event and could hand the
    /// id off to C5).
    ///
    /// **Always writes [`Outcome::Pending`]** — the C4 contract. The
    /// caller is responsible for the upstream gating ([`should_log`] +
    /// [`has_motor_evidence`]); this method does not re-check them.
    ///
    /// `top_candidate`, `top_score`, and `top_motor_evidence` are
    /// sourced from the engine's **score report**. `token_motor` is
    /// sourced from the **per-char dwell slice** of the token's span
    /// — a candidate-independent read that's present on every
    /// loggable record (fast lane included).
    ///
    /// * C5a's resolver: classifies `CorrectedToSuggestion` when the
    ///   user lands on a candidate Cautious wouldn't have suggested
    ///   (reads `top_candidate`).
    /// * C5b's lexicon proposer: gates promotion of Kept words by
    ///   `token_motor.verdict` — clean execution promotes, slip
    ///   execution holds. `top_motor_evidence` is no longer the gate;
    ///   it's kept on the record as a diagnostic.
    ///
    /// Pass `None` for `top_*` whenever the score report had no
    /// candidates. `token_motor` should always be `Some(_)`; a `None`
    /// indicates a bug in the engine's dwell-tracking (defensive).
    #[allow(clippy::too_many_arguments)]
    pub fn append(
        &mut self,
        timestamp_ms: u64,
        decision: DecisionOutcome,
        anchor_id: u32,
        active_tier: ConfidenceTier,
        top_candidate: Option<String>,
        top_score: Option<f64>,
        top_motor_evidence: Option<f64>,
        top_confidence: Option<Confidence>,
        token_motor: Option<crate::motor_signal::TokenMotorSignal>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;

        let original_text = decision.original().to_string();
        let confidence = LogConfidence::from_optional(top_confidence);

        let record = LogRecord {
            id,
            timestamp_ms,
            original_text,
            decision,
            top_candidate,
            top_score,
            top_motor_evidence,
            token_motor,
            confidence,
            anchor_id,
            active_tier,
            outcome: Outcome::Pending,
            log_version: LOG_VERSION,
        };

        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(record);
        id
    }

    /// Transition the outcome slot of record `id`. Called by Component 5
    /// ([`crate::resolver::OutcomeResolver::tick`]) as the user's edits
    /// reveal the outcome; **revisable** — may fire more than once on
    /// the same record, latest write wins. Returns `true` if the record
    /// was found (and updated), `false` if `id` has already been evicted
    /// or never existed.
    ///
    /// O(n) scan — the ledger is small and resolution events are
    /// infrequent (one per real user correction). If the ledger ever
    /// outgrows that, switch to a (id → index) sidecar map.
    pub fn resolve_outcome(&mut self, id: u64, outcome: Outcome) -> bool {
        for r in self.records.iter_mut() {
            if r.id == id {
                r.outcome = outcome;
                return true;
            }
        }
        false
    }

    /// Locate a record by id (read-only). Useful for tests and for any
    /// future C5 read path that wants to inspect the slot before
    /// transitioning.
    pub fn get(&self, id: u64) -> Option<&LogRecord> {
        self.records.iter().find(|r| r.id == id)
    }
}

/// Upstream gate: decide whether a sealed Word decision should be
/// appended to the ledger.
///
/// Two gates, both must pass:
///   1. **UNKNOWN-word filter.** Skip `LeaveAlone(Known)` — a known word
///      is protected by design and there's nothing to learn from leaving
///      it alone. **Include** `LeaveAlone(NoCandidates)` so unknown names
///      ("Soumyo", "Krutrim") are captured for C5 to attribute `Kept`.
///   2. **Motor-evidence gate.** `has_motor` must be `true` — pasted /
///      auto-filled / synthetic text has ~zero motor evidence (dwell=0)
///      and shouldn't reach the ledger. See [`has_motor_evidence`].
///
/// Pure function — no I/O, easy to unit-test the gates in isolation.
pub fn should_log(decision: &DecisionOutcome, has_motor: bool) -> bool {
    if !has_motor {
        return false;
    }
    if let DecisionOutcome::LeaveAlone {
        reason: LeaveAloneReason::Known,
        ..
    } = decision
    {
        return false;
    }
    true
}

/// True iff every character in `[start, end)` of the per-char dwell
/// buffer carries real motor evidence (non-zero dwell).
///
/// The engine maintains `line_dwells: Vec<u32>` in lock-step with its
/// `line_buf: Vec<char>` — every typed character contributes its
/// `dwell_ms` from the L2 `Key` event; backspace removes both in
/// parallel; line-reset clears both. Pasted / auto-filled / synthetic
/// characters arrive with `dwell_ms == 0` (no press/release timing) and
/// fail this check by construction.
///
/// **Empty slice → `false`** (no chars = no evidence). Out-of-range
/// slice → `false` (defensive; the engine should never feed one).
pub fn has_motor_evidence(line_dwells: &[u32], start: usize, end: usize) -> bool {
    if end <= start || end > line_dwells.len() {
        return false;
    }
    line_dwells[start..end].iter().all(|&d| d > 0)
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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

    // ---- LogConfidence mapping ------------------------------------------

    #[test]
    fn confidence_band_maps_from_optional_confidence() {
        assert_eq!(
            LogConfidence::from_optional(Some(Confidence::High)),
            LogConfidence::High
        );
        assert_eq!(
            LogConfidence::from_optional(Some(Confidence::Medium)),
            LogConfidence::Medium
        );
        assert_eq!(
            LogConfidence::from_optional(Some(Confidence::Low)),
            LogConfidence::Low
        );
        assert_eq!(
            LogConfidence::from_optional(None),
            LogConfidence::BelowFloor
        );
    }

    // ---- should_log gate -------------------------------------------------

    #[test]
    fn should_log_skips_known_word_leave_alone() {
        let d = leave_alone("the", LeaveAloneReason::Known);
        assert!(!should_log(&d, true));
    }

    #[test]
    fn should_log_keeps_no_candidates_leave_alone() {
        // Names like "Soumyo" land here — C5 needs to attribute a Kept.
        let d = leave_alone("Soumyo", LeaveAloneReason::NoCandidates);
        assert!(should_log(&d, true));
    }

    #[test]
    fn should_log_keeps_below_floor_leave_alone() {
        let d = leave_alone("lold", LeaveAloneReason::BelowFloor);
        assert!(should_log(&d, true));
    }

    #[test]
    fn should_log_keeps_below_active_tier_leave_alone() {
        let d = leave_alone("recieve", LeaveAloneReason::BelowActiveTier);
        assert!(should_log(&d, true));
    }

    #[test]
    fn should_log_keeps_ambiguous_leave_alone() {
        let d = leave_alone("wodl", LeaveAloneReason::Ambiguous);
        assert!(should_log(&d, true));
    }

    #[test]
    fn should_log_keeps_would_correct() {
        let d = would_correct("teh", "the", 0.82);
        assert!(should_log(&d, true));
    }

    #[test]
    fn should_log_rejects_when_motor_evidence_missing() {
        // Even strong WouldCorrect decisions are rejected without motor
        // evidence — pasted text never reaches the ledger.
        let d = would_correct("teh", "the", 0.82);
        assert!(!should_log(&d, false));
        // Same for unknown names with no candidates.
        let d = leave_alone("Soumyo", LeaveAloneReason::NoCandidates);
        assert!(!should_log(&d, false));
    }

    // ---- has_motor_evidence ---------------------------------------------

    #[test]
    fn motor_evidence_present_when_all_dwells_nonzero() {
        let dwells = vec![80, 95, 110, 60, 70];
        assert!(has_motor_evidence(&dwells, 0, 5));
        assert!(has_motor_evidence(&dwells, 1, 4));
    }

    #[test]
    fn motor_evidence_absent_when_any_dwell_zero() {
        // One zero-dwell char inside the span — paste-like.
        let dwells = vec![80, 0, 110, 60, 70];
        assert!(!has_motor_evidence(&dwells, 0, 5));
        // The zero is outside this sub-span, so the sub-span still has motor.
        assert!(has_motor_evidence(&dwells, 2, 5));
    }

    #[test]
    fn motor_evidence_false_for_empty_or_out_of_range_span() {
        let dwells = vec![80, 95, 110];
        // Empty span — no chars contributed, no evidence.
        assert!(!has_motor_evidence(&dwells, 2, 2));
        // Out of range — engine bug guard, fail closed.
        assert!(!has_motor_evidence(&dwells, 0, 99));
        // Inverted span — same.
        assert!(!has_motor_evidence(&dwells, 3, 1));
    }

    #[test]
    fn motor_evidence_false_when_all_dwells_zero() {
        // The pure-paste case: every char arrived without press/release timing.
        let dwells = vec![0, 0, 0, 0];
        assert!(!has_motor_evidence(&dwells, 0, 4));
    }

    // ---- DecisionLedger basics ------------------------------------------

    /// Convenience for tests: append a `WouldCorrect`-shaped record and
    /// also populate the matching candidate/score on the record (mirrors
    /// what engine.rs does — sourced from the score report, not the
    /// decision arm). Motor evidence defaults to 0.5 (a neutral
    /// placeholder for tests that don't exercise the motor signal).
    #[allow(clippy::too_many_arguments)]
    fn append_would_correct(
        ledger: &mut DecisionLedger,
        timestamp_ms: u64,
        original: &str,
        suggested: &str,
        score: f64,
        anchor_id: u32,
        tier: ConfidenceTier,
        conf: Confidence,
    ) -> u64 {
        ledger.append(
            timestamp_ms,
            would_correct(original, suggested, score),
            anchor_id,
            tier,
            Some(suggested.to_string()),
            Some(score),
            Some(0.5),
            Some(conf),
            // Test default: a 3-char clean span. Tests that want
            // specific motor verdicts construct the signal explicitly.
            Some(crate::motor_signal::TokenMotorSignal {
                verdict: crate::motor_signal::TokenMotorVerdict::Clean,
                slip_score: 0.0,
                graze_count: 0,
                char_count: 3,
            }),
        )
    }

    #[test]
    fn append_initialises_outcome_to_pending() {
        let mut ledger = DecisionLedger::new();
        let id = append_would_correct(
            &mut ledger,
            1_000,
            "teh",
            "the",
            0.82,
            42,
            ConfidenceTier::Cautious,
            Confidence::High,
        );
        let rec = ledger.get(id).unwrap();
        assert_eq!(rec.outcome, Outcome::Pending);
        assert_eq!(rec.anchor_id, 42);
        assert_eq!(rec.active_tier, ConfidenceTier::Cautious);
        assert_eq!(rec.timestamp_ms, 1_000);
        assert_eq!(rec.original_text, "teh");
        assert_eq!(rec.top_candidate.as_deref(), Some("the"));
        assert_eq!(rec.top_score, Some(0.82));
        assert_eq!(rec.top_motor_evidence, Some(0.5));
        assert_eq!(rec.confidence, LogConfidence::High);
        assert_eq!(rec.log_version, LOG_VERSION);
    }

    #[test]
    fn append_for_leave_alone_no_candidates_sets_below_floor_band() {
        let mut ledger = DecisionLedger::new();
        let d = leave_alone("Soumyo", LeaveAloneReason::NoCandidates);
        // NoCandidates is the only arm where the score report had
        // nothing — `top_candidate` / `top_score` are legitimately
        // `None` here.
        let id = ledger.append(
            2_000,
            d,
            7,
            ConfidenceTier::Balanced,
            None,
            None,
            None,
            None,
            None,
        );
        let rec = ledger.get(id).unwrap();
        assert!(rec.top_candidate.is_none());
        assert!(rec.top_score.is_none());
        assert!(rec.top_motor_evidence.is_none());
        // token_motor is independent of candidate availability — for
        // a NoCandidates record we pass None defensively, the engine
        // path actually populates it from line_dwells.
        assert!(rec.token_motor.is_none());
        assert_eq!(rec.confidence, LogConfidence::BelowFloor);
    }

    #[test]
    fn append_for_leave_alone_with_candidate_keeps_candidate() {
        // C5a load-bearing: a `LeaveAlone(BelowActiveTier)` decision
        // **still carries the candidate** so the resolver can attribute
        // `CorrectedToSuggestion` when the user lands on a suggestion
        // the mode wouldn't have fired. This is the change that fixes
        // the "bullon → bullion" misclassification: the engine knows
        // the candidate, only the mode declined to act.
        let mut ledger = DecisionLedger::new();
        let d = leave_alone("bullon", LeaveAloneReason::BelowActiveTier);
        let id = ledger.append(
            0,
            d,
            1,
            ConfidenceTier::Cautious,
            Some("bullion".to_string()),
            Some(0.47),
            Some(0.55),
            Some(Confidence::Medium),
            None,
        );
        let rec = ledger.get(id).unwrap();
        assert_eq!(rec.top_candidate.as_deref(), Some("bullion"));
        assert_eq!(rec.top_score, Some(0.47));
        assert_eq!(rec.confidence, LogConfidence::Medium);
        // Decision arm is still `LeaveAlone` — the candidate is a
        // *score-report* field, orthogonal to the engine's action.
        assert!(matches!(rec.decision, DecisionOutcome::LeaveAlone { .. }));
    }

    #[test]
    fn ids_are_monotonic_across_appends() {
        let mut ledger = DecisionLedger::new();
        let id0 = append_would_correct(
            &mut ledger, 0, "a", "an", 0.6, 1, ConfidenceTier::Eager, Confidence::Medium,
        );
        let id1 = append_would_correct(
            &mut ledger, 0, "b", "be", 0.6, 2, ConfidenceTier::Eager, Confidence::Medium,
        );
        let id2 = append_would_correct(
            &mut ledger, 0, "c", "cat", 0.6, 3, ConfidenceTier::Eager, Confidence::Medium,
        );
        assert_eq!((id0, id1, id2), (0, 1, 2));
    }

    // ---- Bounded capacity ------------------------------------------------

    #[test]
    fn capacity_evicts_oldest_when_full() {
        let mut ledger = DecisionLedger::with_capacity(3);
        let id0 = append_would_correct(
            &mut ledger, 0, "a", "an", 0.6, 1, ConfidenceTier::Eager, Confidence::Medium,
        );
        let id1 = append_would_correct(
            &mut ledger, 0, "b", "be", 0.6, 2, ConfidenceTier::Eager, Confidence::Medium,
        );
        let id2 = append_would_correct(
            &mut ledger, 0, "c", "cat", 0.6, 3, ConfidenceTier::Eager, Confidence::Medium,
        );
        // Capacity full; the next append should evict id0.
        let id3 = append_would_correct(
            &mut ledger, 0, "d", "do", 0.6, 4, ConfidenceTier::Eager, Confidence::Medium,
        );
        assert_eq!(ledger.len(), 3);
        assert!(ledger.get(id0).is_none(), "oldest record should be evicted");
        assert!(ledger.get(id1).is_some());
        assert!(ledger.get(id2).is_some());
        assert!(ledger.get(id3).is_some());
        // IDs still monotonic across evictions.
        assert_eq!(id3, 3);
    }

    #[test]
    fn iter_yields_oldest_first() {
        let mut ledger = DecisionLedger::with_capacity(3);
        for i in 0..3 {
            append_would_correct(
                &mut ledger,
                i,
                &format!("w{i}"),
                "x",
                0.6,
                i as u32,
                ConfidenceTier::Eager,
                Confidence::Medium,
            );
        }
        let timestamps: Vec<u64> = ledger.iter().map(|r| r.timestamp_ms).collect();
        assert_eq!(timestamps, vec![0, 1, 2]);
    }

    // ---- resolve_outcome (C5 hook) --------------------------------------

    #[test]
    fn resolve_outcome_updates_existing_record() {
        let mut ledger = DecisionLedger::new();
        let id = append_would_correct(
            &mut ledger,
            0,
            "teh",
            "the",
            0.82,
            1,
            ConfidenceTier::Cautious,
            Confidence::High,
        );
        assert_eq!(ledger.get(id).unwrap().outcome, Outcome::Pending);

        let ok = ledger.resolve_outcome(id, Outcome::CorrectedToSuggestion);
        assert!(ok);
        assert_eq!(
            ledger.get(id).unwrap().outcome,
            Outcome::CorrectedToSuggestion
        );
    }

    #[test]
    fn resolve_outcome_returns_false_for_unknown_id() {
        let mut ledger = DecisionLedger::new();
        append_would_correct(
            &mut ledger,
            0,
            "teh",
            "the",
            0.82,
            1,
            ConfidenceTier::Cautious,
            Confidence::High,
        );
        assert!(!ledger.resolve_outcome(9999, Outcome::Kept));
    }

    #[test]
    fn resolve_outcome_returns_false_for_evicted_id() {
        // Capacity 2 — the third append evicts id0; resolve_outcome on it fails.
        let mut ledger = DecisionLedger::with_capacity(2);
        let id0 = append_would_correct(
            &mut ledger, 0, "a", "an", 0.6, 1, ConfidenceTier::Eager, Confidence::Medium,
        );
        append_would_correct(
            &mut ledger, 0, "b", "be", 0.6, 2, ConfidenceTier::Eager, Confidence::Medium,
        );
        append_would_correct(
            &mut ledger, 0, "c", "cat", 0.6, 3, ConfidenceTier::Eager, Confidence::Medium,
        );
        assert!(!ledger.resolve_outcome(id0, Outcome::Kept));
    }
}
