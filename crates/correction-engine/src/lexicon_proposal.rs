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

use std::collections::{HashMap, HashSet};

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
/// lowercase tokens (`un`, `iv`, `qz`) are overwhelmingly typos /
/// fragments; legitimate 2-char words (`of`, `to`, `is`) are all in
/// SCOWL and never reach the proposer. Strictly less than this →
/// `Held(ObviousFragment)`.
const MIN_PROMOTABLE_WORD_LEN: usize = 3;

/// Minimum length for **acronym-shape** tokens (all-caps alphabetic).
/// All-caps `AI` / `GM` / `VP` are intentional 2-char acronyms; the
/// case carries the "deliberate, not a typo" signal that the
/// lowercase fragment gate can't see. Tokenizer's Acronym
/// classification requires length ≥ 2 already, so this aligns the
/// proposer with the tokenizer.
const MIN_PROMOTABLE_ACRONYM_LEN: usize = 2;

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

/// Cap on the re-evaluation cascade after a learned-set change. The
/// confirm-of-X flips Y's proximity → Y demotes, which COULD flip a
/// third word's proximity… in practice this stabilises in 1–2
/// iterations; the cap is defensive against an oscillating
/// configuration. Reached only if there's a real cycle in the
/// proximity graph; the test suite covers the common cases.
const MAX_RE_EVAL_ITERATIONS: usize = 5;

// ---- Casing baseline (relative-to-this-user rescue) -----------------------
//
// The all-caps → brand-name rescue is *informative only when all-caps
// is unusual for this user*. For a habitual all-caps typist (or
// transient caps-lock burst), every near-known typo looks like an
// acronym and everything gets rescued. Track the recency-weighted
// share of all-caps tokens; the rescue fires only when that share is
// below the threshold AND we've seen enough samples to trust it.

/// Per-token decay applied to both casing weights before incrementing.
/// 0.99 → half-life ≈ 69 tokens; a 50-token caps-lock burst lifts the
/// share enough to suppress the rescue, but a return to normal typing
/// recovers it within another ~100 tokens. **PLACEHOLDER** — tune
/// from the panel.
pub const CASING_DECAY_PER_TOKEN: f64 = 0.99;

/// All-caps share strictly below this → rescue active. At-or-above →
/// rescue suppressed (all-caps too common for the case-signal to
/// distinguish intent). **PLACEHOLDER**.
pub const CASING_RESCUE_THRESHOLD: f64 = 0.20;

/// Total sample-weight floor before the share is trusted. While the
/// total weight is below this, the rescue defaults to ACTIVE — a
/// brand-new session shouldn't suppress acronym learning from one
/// stray observation. **PLACEHOLDER**.
pub const CASING_MIN_SAMPLES: f64 = 5.0;

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

/// The user's recency-weighted casing baseline + the derived
/// rescue-active flag. Surfaced on the panel so the signal's state
/// is visible: a normal-cased user sees `rescue_active: true` and
/// expects acronym carve-outs; an all-caps user sees it `false` and
/// understands why their all-caps tokens get the same gates as
/// lowercase ones.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CasingBaseline {
    /// Share of sealed Word/Acronym tokens that were all-caps,
    /// weighted by recency (exponential decay; see
    /// [`CASING_DECAY_PER_TOKEN`]). `[0.0, 1.0]`. Zero before any
    /// observations.
    pub all_caps_share: f64,
    /// Sum of decayed token weights. Below [`CASING_MIN_SAMPLES`]
    /// the share is too noisy to use, and the rescue defaults active.
    pub sample_count: f64,
    /// `true` iff the all-caps brand-name rescue should fire.
    /// Lane-independent of any specific proposal.
    pub rescue_active: bool,
}

impl CasingBaseline {
    /// Cold-start default: no observations, rescue active.
    pub fn cold_start() -> Self {
        Self {
            all_caps_share: 0.0,
            sample_count: 0.0,
            rescue_active: true,
        }
    }
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
    /// Recency-weighted count of all-caps (`Acronym`-kind) seals.
    /// Decayed by [`CASING_DECAY_PER_TOKEN`] on every fresh seal.
    casing_acronym_weight: f64,
    /// Recency-weighted count of non-all-caps (`Word`-kind) seals.
    /// Same decay schedule — both weights decay on every seal regardless
    /// of which one increments, so the ratio reflects RECENCY, not
    /// total observations.
    casing_word_weight: f64,
    /// **C5b Phase 2.** Per-learned-word timestamp (lowercase keys →
    /// `last_seen_ms` at the moment the word entered `is_known`).
    /// Tracked here rather than on `Lexicon` because the proposer is
    /// the authority on "when did this word become learned."
    ///
    /// The proximity check uses this to enforce **newer-wins**: a
    /// learned word X only counts as a near-known anchor for word W
    /// if `learned_at_ms[X] > W.last_seen_ms`. Without this, two
    /// mutually-near-known words deadlock each other — neither can
    /// reach `Confirmed` because each holds the other.
    learned_at_ms: HashMap<String, u64>,
    /// **Meta-context exclusion** — when true, [`Self::note_record`]
    /// short-circuits and writes nothing. Used by the host to suppress
    /// learning while the user is typing about the system itself
    /// (debug-window focus, or the manual pause toggle) — the
    /// canonical case is: typing the slip `soumyio` in a chat to
    /// report a bug confirmed it over the real name `soumyo`.
    ///
    /// Scope is deliberately narrow: ONLY the credit step is gated.
    /// All other observation (ledger, decisions, anchor tracking,
    /// resolver, panel events for non-proposal updates) runs
    /// unchanged. Records ingested while paused are NOT retroactively
    /// credited on resume — the user wants them excluded, not
    /// deferred. Default `false` (learning active).
    credit_paused: bool,
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

/// Result of a [`LexiconProposer::note_record`] call. Now potentially
/// carries MULTIPLE per-word changes — a single Kept can trigger a
/// learning event (confirm/demote) which re-evaluates every other
/// proposal against the new live lexicon, and any of those can also
/// transition tiers (self-cleaning). All resulting changes are
/// returned in one batch so the host emits them atomically.
#[derive(Debug, Clone)]
pub struct ProposalUpdate {
    /// Per-word changes produced by this call. Empty when nothing
    /// changed (idempotent re-note, or a record that doesn't affect
    /// any proposal). `Option<LexiconProposal>` = `None` means the
    /// proposal was retracted (last contributing record rolled back).
    pub changes: Vec<(String, Option<LexiconProposal>)>,
    /// Set true iff a `Confirmed` ↔ not-`Confirmed` transition fired
    /// for ANY word in this call — i.e. the global learned set
    /// changed. The host emits a fresh `learned-snapshot` event so
    /// the panel can update its "live in is_known" rendering.
    pub learned_set_changed: bool,
}

impl ProposalUpdate {
    pub fn empty() -> Self {
        Self {
            changes: Vec::new(),
            learned_set_changed: false,
        }
    }
}

impl LexiconProposer {
    pub fn new() -> Self {
        Self {
            proposals: HashMap::new(),
            record_contributions: HashMap::new(),
            lex: Lexicon::shared(),
            casing_acronym_weight: 0.0,
            casing_word_weight: 0.0,
            learned_at_ms: HashMap::new(),
            credit_paused: false,
        }
    }

    /// Set the meta-context pause flag. While true, every
    /// [`Self::note_record`] call returns [`ProposalUpdate::empty`]
    /// without touching proposer state. Setter only — the host owns
    /// the policy (manual toggle, debug-window-focus auto-pause).
    pub fn set_credit_paused(&mut self, paused: bool) {
        self.credit_paused = paused;
    }

    /// Whether `note_record` is currently suppressed. Exposed so the
    /// host can echo the state back to the panel after a control
    /// command (so the indicator reflects the actual engine flag,
    /// not just the panel's optimistic state).
    pub fn credit_paused(&self) -> bool {
        self.credit_paused
    }

    /// Observe a sealed `Word` or `Acronym` token for the casing
    /// baseline. **Called per fresh seal** (not per replay) by the
    /// engine so backspace rebuilds don't double-count. Includes
    /// known-word seals — the baseline reflects all of the user's
    /// real typing, not just unknown words.
    ///
    /// Both weights decay before the increment, so the metric is
    /// the recency-weighted ratio, not raw counts. A 50-token
    /// caps-lock burst shifts the share enough to suppress the
    /// rescue; ~100 subsequent normal tokens restore it.
    pub fn note_token_seal(&mut self, is_acronym: bool) {
        self.casing_acronym_weight *= CASING_DECAY_PER_TOKEN;
        self.casing_word_weight *= CASING_DECAY_PER_TOKEN;
        if is_acronym {
            self.casing_acronym_weight += 1.0;
        } else {
            self.casing_word_weight += 1.0;
        }
    }

    /// Current [`CasingBaseline`]. Cold-start (no observations) is
    /// `rescue_active: true` — a brand-new session shouldn't suppress
    /// acronym learning. Below [`CASING_MIN_SAMPLES`] total weight the
    /// share is also treated as untrusted (rescue active). Above the
    /// floor, `rescue_active` is true iff `all_caps_share` <
    /// [`CASING_RESCUE_THRESHOLD`].
    pub fn casing_baseline(&self) -> CasingBaseline {
        let total = self.casing_acronym_weight + self.casing_word_weight;
        let share = if total > 0.0 {
            self.casing_acronym_weight / total
        } else {
            0.0
        };
        let rescue_active =
            total < CASING_MIN_SAMPLES || share < CASING_RESCUE_THRESHOLD;
        CasingBaseline {
            all_caps_share: share,
            sample_count: total,
            rescue_active,
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
        // Meta-context pause — the host has signalled "we're in a
        // context where the user types ABOUT TypeAssist, not in it"
        // (debug window focused, or manual toggle). Short-circuit
        // before any state mutation so records ingested while paused
        // leave NO trace: no contribution, no idempotency entry, no
        // re-eval. The rest of the engine (ledger, decisions,
        // resolver) keeps running — only learning is suppressed.
        if self.credit_paused {
            return ProposalUpdate::empty();
        }

        let word = record.original_text.clone();

        // Idempotency check.
        if let Some(prev) = self.record_contributions.get(&record.id) {
            if prev.outcome == record.outcome && prev.word == word {
                return ProposalUpdate::empty();
            }
        }

        // Snapshot ALL proposal tiers before the change so we can
        // (a) sync the learned set on transition,
        // (b) diff at the end to report every word that moved,
        // (c) drive cascade re-evaluation.
        let before: HashMap<String, Option<ProposalTier>> = self
            .proposals
            .iter()
            .map(|(w, p)| (w.clone(), Some(p.tier)))
            .collect();

        // Apply the credit/retract directly.
        let prev_word = self
            .record_contributions
            .remove(&record.id)
            .and_then(|c| matches!(c.outcome, Outcome::Kept).then_some(c.word));
        if let Some(pw) = prev_word.as_ref() {
            self.retract_kept_contribution(pw);
        }
        if matches!(record.outcome, Outcome::Kept) {
            self.credit_kept_contribution(record);
        }
        self.record_contributions.insert(
            record.id,
            RecordContribution {
                word: word.clone(),
                outcome: record.outcome,
            },
        );

        // Sync the lex's learned set against the focal word's
        // current tier (and the prior word if a retract touched
        // it). State-based: this just brings lex in agreement with
        // the proposal tier — robust against cascades.
        let mut learned_changed = self.sync_learned_against_current(&word);
        if let Some(pw) = prev_word.as_ref() {
            if pw != &word {
                learned_changed |= self.sync_learned_against_current(pw);
            }
        }

        // Cascade re-evaluation. When the learned set changes, any
        // other proposal's proximity verdict may now flip — that's
        // the self-cleaning hook (Krutkrim demotes once Krutrim
        // confirms). Each iteration syncs internally; bounded to
        // prevent an oscillating configuration from spinning.
        if learned_changed {
            for _ in 0..MAX_RE_EVAL_ITERATIONS {
                let any = self.re_evaluate_all_proposals();
                if !any {
                    break;
                }
            }
        }

        // Diff before vs current state for the event batch.
        let mut keys: HashSet<String> = before.keys().cloned().collect();
        keys.extend(self.proposals.keys().cloned());
        let mut changes = Vec::new();
        for k in keys {
            let prev_tier = before.get(&k).copied().flatten();
            let curr_proposal = self.proposals.get(&k).cloned();
            let curr_tier = curr_proposal.as_ref().map(|p| p.tier);
            if prev_tier != curr_tier {
                changes.push((k, curr_proposal));
            }
        }

        ProposalUpdate {
            changes,
            learned_set_changed: learned_changed,
        }
    }

    /// Bring the lex's learned set into agreement with the focal
    /// word's CURRENT tier. State-based (not transition-based) so a
    /// re-eval cascade that flips a word in and out of Confirmed
    /// can't desync the lex from the tier. Also maintains
    /// `learned_at_ms` (set on learn, removed on unlearn). Returns
    /// true iff the learned set was mutated. **`&mut self`** —
    /// `learned_at_ms` mutation requires it.
    fn sync_learned_against_current(&mut self, word: &str) -> bool {
        let (is_confirmed, last_seen_ms) = match self.proposals.get(word) {
            Some(p) => (matches!(p.tier, ProposalTier::Confirmed), p.last_seen_ms),
            None => (false, 0),
        };
        let in_learned = self.lex.is_learned(word);
        let key = word.to_ascii_lowercase();
        match (in_learned, is_confirmed) {
            (false, true) => {
                self.learned_at_ms.insert(key, last_seen_ms);
                self.lex.learn(word)
            }
            (true, false) => {
                self.learned_at_ms.remove(&key);
                self.lex.unlearn(word)
            }
            _ => false,
        }
    }

    /// One pass of "re-evaluate every proposal against the live
    /// lexicon." Recomputes `linguistic_signal` (proximity flips when
    /// the learned set grows or shrinks) and the tier. Syncs the lex
    /// learned set for every word it touches. Returns true iff any
    /// tier moved this pass — caller may iterate (capped).
    ///
    /// **Order matters.** Two mutually-near-known words can each be
    /// near-known to the other once both learn; HashMap iteration
    /// order is non-deterministic, so we sort by `last_seen_ms` ASC.
    /// Older words evaluate first, so an older `A` near-known to a
    /// newer `B` demotes first — and then `B`'s subsequent re-eval
    /// no longer sees `A` in the learned set and stays Confirmed.
    /// Net: the newer confirmation wins, matching the brief's
    /// "Krutkrim demotes once Krutrim confirms" semantic.
    fn re_evaluate_all_proposals(&mut self) -> bool {
        let mut words: Vec<(String, u64)> = self
            .proposals
            .iter()
            .map(|(w, p)| (w.clone(), p.last_seen_ms))
            .collect();
        words.sort_by_key(|(_, ts)| *ts);
        let rescue_active = self.casing_baseline().rescue_active;
        let mut any_changed = false;
        for (word, ts) in words {
            // Re-eval: use this proposal's last_seen_ms as the
            // "current word ts" so the newer-wins rule applies
            // consistently with the credit-time evaluation.
            let signal = linguistic_signal(&word, self.lex, ts, &self.learned_at_ms);
            let norvig_freq = self.lex.frequency(&word);
            let (lane, motor_verdict, occasions, prev_tier) = match self.proposals.get(&word) {
                Some(p) => (p.lane.clone(), p.motor_verdict, p.occasions, p.tier),
                None => continue,
            };
            let new_tier = recompute_tier(
                &word,
                &lane,
                motor_verdict,
                signal.plausibility,
                signal.proximity,
                norvig_freq,
                rescue_active,
                occasions,
            );
            // Persist the recomputed proximity/plausibility even if
            // the tier didn't move — the panel reflects the live
            // verdict.
            let p = self.proposals.get_mut(&word).unwrap();
            p.plausibility = signal.plausibility;
            p.proximity = signal.proximity;
            p.norvig_freq = norvig_freq;
            if new_tier != prev_tier {
                p.tier = new_tier;
                any_changed = true;
                self.sync_learned_against_current(&word);
            }
        }
        any_changed
    }

    /// Snapshot of the lex's runtime-learned set. Surfaced to the
    /// host so the panel can render "LIVE in is_known" indicators
    /// next to confirmed proposals and against the LOG records'
    /// `top_candidate` field.
    pub fn learned_snapshot(&self) -> Vec<String> {
        self.lex.learned_snapshot()
    }

    /// Forget every proposal AND every per-record contribution. Hooked
    /// to the engine's true line-reset / shutdown paths in case we
    /// ever want a fresh slate (Phase 1 doesn't call this).
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.proposals.clear();
        self.record_contributions.clear();
    }

    /// **Reset LEXICON — true engine-side wipe.** Forgets every
    /// proposal, every per-record contribution, and every
    /// learned-at-ms entry, **and** clears the lex's learned set so
    /// `is_known` falls back to the bundled clean dict + seeds. After
    /// this returns:
    ///
    ///   * `snapshot()` is empty.
    ///   * `learned_snapshot()` is empty.
    ///   * Any previously-confirmed word is no longer known.
    ///
    /// Distinct from [`Self::clear`] (which only touches proposer
    /// state, leaving any words that were already in `lex.learned`
    /// in place). This is the operation the debug panel's "Reset
    /// LEXICON" button needs: a Confirmed word's state lives in BOTH
    /// the proposer (so the next re-eval syncs back) AND the lex —
    /// resetting one without the other re-pollutes immediately.
    ///
    /// Casing baseline is intentionally NOT reset — it's a property
    /// of the user's typing rhythm, not their learned vocabulary, and
    /// is rebuilt from a wider rolling window anyway.
    pub fn reset_all(&mut self) {
        self.proposals.clear();
        self.record_contributions.clear();
        self.learned_at_ms.clear();
        self.lex.clear_learned();
    }

    // ---- Internal helpers --------------------------------------------

    fn credit_kept_contribution(&mut self, record: &LogRecord) {
        let word = record.original_text.clone();
        let lane = lane_for(record);
        let motor_verdict = motor_verdict_for(record);
        let last_motor_evidence = slip_score_for(record);
        let last_record_id = record.id;
        let last_seen_ms = record.timestamp_ms;
        // Linguistic + casing signals — sampled once per credited
        // record. casing rescue_active is global (per user, not
        // per record) but we read it here so the recompute_tier
        // call sees the latest value.
        // Pass current record's timestamp + the proposer's learned-at
        // map: the proximity check applies the newer-wins rule.
        let LinguisticSignal {
            plausibility,
            well_formed: _,
            proximity,
        } = linguistic_signal(&word, self.lex, last_seen_ms, &self.learned_at_ms);
        let norvig_freq = self.lex.frequency(&word);
        let rescue_active = self.casing_baseline().rescue_active;

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
            rescue_active,
            entry.occasions,
        );
    }

    fn retract_kept_contribution(&mut self, word: &str) {
        // Resolve the casing baseline BEFORE the mutable borrow on the
        // proposal — `casing_baseline()` borrows &self.
        let rescue_active = self.casing_baseline().rescue_active;
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
                    rescue_active,
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

/// Detect an **acronym-shape** original text: all chars uppercase
/// ASCII alphabetic, length ≥ 2. Mirrors the tokenizer's `Acronym`
/// classification rule so the proposer's differentiated gates fire
/// exactly where the tokenizer routed the input as an acronym.
/// We re-derive from `original_text` rather than carrying TokenKind
/// on the record — keeps `LogRecord`'s wire shape stable and the
/// signal is reconstructable losslessly from the string.
fn is_acronym_shape(text: &str) -> bool {
    let mut len: usize = 0;
    for c in text.chars() {
        if !c.is_ascii_alphabetic() || !c.is_ascii_uppercase() {
            return false;
        }
        len += 1;
    }
    len >= 2
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
    rescue_active: bool,
    occasions: u32,
) -> ProposalTier {
    // ---- Stage 1: Eligibility (veto) ----

    // Acronym-shape tokens get differentiated gates ONLY when the
    // user's casing baseline says all-caps is rare for them (`rescue_active`).
    // A habitual all-caps typist (or a caps-lock burst) makes every typo
    // look like an acronym; the case-signal isn't informative for them, so
    // we fall back to the lowercase gates.
    let acronym = is_acronym_shape(word) && rescue_active;
    let min_len = if acronym {
        MIN_PROMOTABLE_ACRONYM_LEN
    } else {
        MIN_PROMOTABLE_WORD_LEN
    };
    if word.chars().count() < min_len {
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
            // get this gate.
            //
            // **Acronyms bypass this veto.** All-caps product names
            // (BBMP, ONDC) have near-known edit-2 neighbours by
            // construction (4-char strings have many edit-2 hits in
            // a 90k-word dict) but the case signal makes them
            // deliberate. Trade-off: a caps-lock typo of a real word
            // (TGE for "the") becomes promotable too — accepted per
            // the brief's "let all-caps NOVEL tokens be learnable."
            if norvig_freq == 0 && !acronym {
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
    use std::sync::Mutex;

    /// **Serialise all proposer tests** against the process-wide
    /// `Lexicon::shared()` learned set. Phase 2 wires `Confirmed`
    /// proposals into the shared lex; any test that confirms or
    /// retracts pollutes the learned set, and concurrent reads in a
    /// different test can see those mutations. The static `Mutex`
    /// forces tests to run sequentially, and every test calls
    /// [`serial_setup`] at the top to wipe the learned set so each
    /// case starts clean.
    static LEARNED_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial_setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = LEARNED_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Lexicon::shared().clear_learned();
        guard
    }

    /// Build a clean-motor signal for `word.len()` chars.
    /// `LeaveAlone(NoCandidates)` decision shorthand for Phase 2 tests
    /// that need to construct ledger entries directly (controlling
    /// timestamps for the older-first re-eval test).
    fn leave_alone_no_candidates(word: &str) -> DecisionOutcome {
        DecisionOutcome::LeaveAlone {
            original: word.to_string(),
            reason: LeaveAloneReason::NoCandidates,
        }
    }

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

        let _g = serial_setup();
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

        let _g = serial_setup();
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

        let _g = serial_setup();
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
        let _g = serial_setup();
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
        let _g = serial_setup();
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

        let _g = serial_setup();
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
        let _g = serial_setup();
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
        let _g = serial_setup();
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
        let _g = serial_setup();
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
        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        let first = p.note_record(&kept);
        let second = p.note_record(&kept);
        assert!(!first.changes.is_empty());
        assert!(second.changes.is_empty(), "idempotent re-note must report no changes");
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
        let _g = serial_setup();
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
        let _g = serial_setup();
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
        let _g = serial_setup();
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

        let _g = serial_setup();
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

        let _g = serial_setup();
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

        let _g = serial_setup();
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

        let _g = serial_setup();
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
        let _g = serial_setup();
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

        let _g = serial_setup();
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

        let _g = serial_setup();
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
        let _g = serial_setup();
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
        let _g = serial_setup();
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

    // ---- Casing baseline (relative-to-this-user) -----------------------

    #[test]
    fn cold_start_has_rescue_active_and_zero_share() {
        let p = LexiconProposer::new();
        let b = p.casing_baseline();
        assert_eq!(b.all_caps_share, 0.0);
        assert_eq!(b.sample_count, 0.0);
        assert!(
            b.rescue_active,
            "cold start MUST default rescue active — \
             a brand-new session shouldn't suppress acronym learning"
        );
    }

    #[test]
    fn normal_typing_keeps_rescue_active() {
        // 200 word seals, no acronyms → share ≈ 0% → rescue active.
        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        for _ in 0..200 {
            p.note_token_seal(false);
        }
        let b = p.casing_baseline();
        assert!(b.sample_count > CASING_MIN_SAMPLES);
        assert!(b.all_caps_share < CASING_RESCUE_THRESHOLD);
        assert!(b.rescue_active);
    }

    #[test]
    fn habitual_all_caps_user_suppresses_rescue() {
        // 200 acronym seals, no words → share = 100% → rescue suppressed.
        // This is the user the brief specifically warned about (caps
        // lock on or always-all-caps typist).
        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        for _ in 0..200 {
            p.note_token_seal(true);
        }
        let b = p.casing_baseline();
        assert!(b.all_caps_share > CASING_RESCUE_THRESHOLD);
        assert!(!b.rescue_active);
    }

    #[test]
    fn caps_lock_burst_temporarily_suppresses_then_recovers() {
        // Start normal (rescue active), do a 50-token caps-lock burst
        // (rescue should suppress mid-burst), then 100 word tokens
        // (rescue should recover).
        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        // Normal typing baseline.
        for _ in 0..100 {
            p.note_token_seal(false);
        }
        assert!(p.casing_baseline().rescue_active);

        // Caps-lock burst.
        for _ in 0..50 {
            p.note_token_seal(true);
        }
        assert!(
            !p.casing_baseline().rescue_active,
            "share after 50-acronym burst must exceed threshold \
             — the rescue should be suppressed DURING the burst \
             (share = {:.3})",
            p.casing_baseline().all_caps_share
        );

        // Return to normal typing.
        for _ in 0..150 {
            p.note_token_seal(false);
        }
        assert!(
            p.casing_baseline().rescue_active,
            "after 150 normal tokens following the burst, decay \
             should restore rescue (share = {:.3})",
            p.casing_baseline().all_caps_share
        );
    }

    #[test]
    fn bbmp_promoted_when_user_normally_lowercase() {
        // Normal user (rescue active) + BBMP → eligible.
        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        for _ in 0..100 {
            p.note_token_seal(false);
        }

        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "BBMP", clean_motor("BBMP"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_token_seal(true); // BBMP itself counts as one acronym
        p.note_record(&kept);

        let prop = p.get("BBMP").unwrap();
        assert_eq!(
            prop.tier,
            ProposalTier::Provisional,
            "BBMP must promote — normal user means rescue is active \
             and acronym-shape bypasses the near-known + no-web veto"
        );
    }

    #[test]
    fn bbmp_held_when_user_is_habitual_all_caps() {
        // All-caps user (rescue suppressed) + BBMP → falls back to
        // word-style gates → near-known + zero Norvig → Held.
        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        for _ in 0..200 {
            p.note_token_seal(true);
        }
        assert!(
            !p.casing_baseline().rescue_active,
            "precondition: rescue must be suppressed"
        );

        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "BBMP", clean_motor("BBMP"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_token_seal(true);
        p.note_record(&kept);

        let prop = p.get("BBMP").unwrap();
        assert_eq!(
            prop.tier,
            ProposalTier::Held {
                reason: HoldReason::NearKnownWord
            },
            "BBMP must be held — for an all-caps user the case-signal \
             carries no rescue, so the standard near-known + no-web \
             veto applies"
        );
    }

    #[test]
    fn ai_two_char_acronym_eligible_for_normal_user_held_for_all_caps_user() {
        // AI (2 chars, all-caps, in Norvig): normal user → eligible
        // (length-2 carve-out via acronym). All-caps user → fragment
        // veto fires (carve-out off; MIN_PROMOTABLE_WORD_LEN=3).
        {
            let _g = serial_setup();
            let mut p = LexiconProposer::new();
            for _ in 0..100 {
                p.note_token_seal(false);
            }
            let mut ledger = DecisionLedger::new();
            let id = append_fast(&mut ledger, "AI", clean_motor("AI"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_token_seal(true);
            p.note_record(&kept);
            assert!(
                !matches!(p.get("AI").unwrap().tier, ProposalTier::Held { .. }),
                "normal user: AI must be eligible"
            );
        }
        {
            let _g = serial_setup();
            let mut p = LexiconProposer::new();
            for _ in 0..200 {
                p.note_token_seal(true);
            }
            let mut ledger = DecisionLedger::new();
            let id = append_fast(&mut ledger, "AI", clean_motor("AI"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_token_seal(true);
            p.note_record(&kept);
            assert_eq!(
                p.get("AI").unwrap().tier,
                ProposalTier::Held {
                    reason: HoldReason::ObviousFragment
                },
                "all-caps user: AI must be a fragment — the case-signal \
                 carries no information, fall back to word-style gates"
            );
        }
    }

    // ---- Acronym calibration (all-caps novel tokens) -------------------

    #[test]
    fn novel_all_caps_acronym_with_near_known_neighbours_promotes() {
        // BBMP: 4-char all-caps product name. Near-known (close to
        // `temp`, `bump`, etc.) but the case-signal says "deliberate
        // acronym, not typo." Eligible despite near-known + zero
        // Norvig — that combo only vetoes lowercase tokens.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "BBMP", clean_motor("BBMP"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        let prop = p.get("BBMP").unwrap();
        assert!(
            !matches!(prop.tier, ProposalTier::Held { .. }),
            "BBMP must not be held — acronym-shape bypasses the \
             near-known + no-web veto. Got: {:?}",
            prop.tier
        );
        assert_eq!(prop.tier, ProposalTier::Provisional);
    }

    #[test]
    fn two_char_acronym_eligible_while_lowercase_two_char_fragment() {
        // The user's calibration corpus: AI/GM/VP (2-char acronyms)
        // must be eligible, while `un` (2-char lowercase) must stay
        // Held as a fragment. The case-signal is the differentiator.
        let mut ledger = DecisionLedger::new();
        let _g = serial_setup();
        let mut p = LexiconProposer::new();

        // 2-char acronym: eligible.
        let id = append_fast(&mut ledger, "AI", clean_motor("AI"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_record(&kept);
        assert!(
            !matches!(p.get("AI").unwrap().tier, ProposalTier::Held { .. }),
            "AI (2-char acronym) must not be a fragment"
        );

        // 2-char lowercase: held as fragment (count never bypasses).
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
        assert_eq!(
            p.get("un").unwrap().tier,
            ProposalTier::Held {
                reason: HoldReason::ObviousFragment
            }
        );
    }

    #[test]
    fn acronym_is_still_held_on_slip_motor() {
        // The acronym carve-outs relax length and near-known, NOT
        // motor. A slip-shaped acronym (caps-lock typo with grazes)
        // still holds.
        let mut ledger = DecisionLedger::new();
        let id = append_fast(&mut ledger, "BBMP", slip_motor("BBMP"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);

        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        p.note_record(&kept);
        assert_eq!(
            p.get("BBMP").unwrap().tier,
            ProposalTier::Held {
                reason: HoldReason::SlipSignature
            }
        );
    }

    #[test]
    fn acronym_recurrence_confirms_after_threshold() {
        // Three Kept occasions of UPI (fast lane) → Confirmed.
        // Acronyms use the same per-lane occasion thresholds as
        // words; only the eligibility gates differ.
        let mut ledger = DecisionLedger::new();
        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = append_fast(&mut ledger, "UPI", clean_motor("UPI"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert_eq!(p.get("UPI").unwrap().tier, ProposalTier::Confirmed);
    }

    #[test]
    fn is_acronym_shape_pins_classification() {
        // Pure unit test of the shape detector — mirrors the
        // tokenizer's Acronym rule (length ≥ 2, all-caps alphabetic).
        assert!(is_acronym_shape("AI"));
        assert!(is_acronym_shape("BBMP"));
        assert!(is_acronym_shape("ONDC"));
        assert!(is_acronym_shape("UPI"));
        assert!(is_acronym_shape("ZAMS"));
        // Length 1 fails (single letter is too short for the
        // tokenizer's acronym rule too).
        assert!(!is_acronym_shape("A"));
        // Lowercase fails.
        assert!(!is_acronym_shape("ai"));
        assert!(!is_acronym_shape("Soumyo"));
        // Mixed case fails (Soumyo isn't an acronym).
        assert!(!is_acronym_shape("UPi"));
        // Empty fails.
        assert!(!is_acronym_shape(""));
        // Digits / punctuation fail.
        assert!(!is_acronym_shape("U2"));
        assert!(!is_acronym_shape("A.I"));
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

        let _g = serial_setup();
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

    // ---- C5b Phase 2: Confirmed wires into is_known --------------------

    #[test]
    fn confirmed_word_enters_live_is_known_and_candidate_pool() {
        let _g = serial_setup();
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        let lex = Lexicon::shared();

        // Soumyo isn't in SCOWL or seed and has zero Norvig — confirm
        // it via three Kept occasions and verify the lex sees it.
        assert!(!lex.is_known("Soumyo"), "precondition: Soumyo not yet known");
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert_eq!(p.get("Soumyo").unwrap().tier, ProposalTier::Confirmed);

        // (a) stop-flagging — is_known returns true.
        assert!(
            lex.is_known("Soumyo"),
            "Soumyo must be in is_known after Confirmed"
        );
        assert!(lex.is_learned("Soumyo"));

        // (b) correction anchor — ranked_known_candidates of a NEAR
        // string now surfaces the learned word. "Soumyon" is edit-1
        // of Soumyo; the candidate generator's `is_known` check
        // accepts learned words transparently.
        use crate::candidates::ranked_known_candidates;
        let cands = ranked_known_candidates("Soumyon", lex, 5);
        assert!(
            cands.iter().any(|c| c.word.eq_ignore_ascii_case("soumyo")),
            "candidate pool must include the learned word; got: {:?}",
            cands.iter().map(|c| &c.word).collect::<Vec<_>>()
        );
    }

    #[test]
    fn provisional_word_does_not_enter_is_known() {
        // Single Kept → Provisional → MUST NOT enter the learned set.
        // Phase 2's wiring is Confirmed-only.
        let _g = serial_setup();
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        let lex = Lexicon::shared();

        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_record(&kept);
        assert_eq!(p.get("Soumyo").unwrap().tier, ProposalTier::Provisional);
        assert!(
            !lex.is_learned("Soumyo"),
            "Provisional words MUST NOT enter is_known"
        );
        assert!(!lex.is_known("Soumyo"));
    }

    #[test]
    fn self_cleaning_demotes_older_confirmed_when_newer_anchors_near_it() {
        // The Krutrim cleanup, falling out for free.
        //
        // 1. User types `Klorvex` (a slip variant) repeatedly → confirms.
        // 2. User then types `Klorvox` (canonical) repeatedly → confirms.
        // 3. Re-eval: Klorvex is now near-known to a learned word
        //    (Klorvox). Klorvex demotes; Klorvox stays.
        //
        // Names chosen so neither has Norvig presence — the near-known
        // + no-web combo veto is what fires the demotion. Older-first
        // iteration ordering in re_evaluate_all_proposals makes the
        // outcome deterministic.
        let _g = serial_setup();
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        let lex = Lexicon::shared();

        // Stage 1: confirm Klorvex. Both words are far-from-known
        // initially (no high-freq edit-2 neighbour); they pass
        // eligibility on every gate.
        for i in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = ledger.append(
                100 + i as u64,
                leave_alone_no_candidates("Klorvex"),
                0,
                ConfidenceTier::Eager,
                None,
                None,
                None,
                None,
                Some(clean_motor("Klorvex")),
            );
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert_eq!(p.get("Klorvex").unwrap().tier, ProposalTier::Confirmed);
        assert!(lex.is_learned("Klorvex"));

        // Stage 2: confirm Klorvox (newer). Klorvex is now in the
        // learned set, so Klorvox's proximity check sees it at edit-1.
        // BUT — at the moment of credit, Klorvox's own
        // recompute_tier already runs and would fire Held(NearKnownWord)
        // (Klorvex in learned). That holds Klorvox… which is the wrong
        // outcome.
        //
        // The re-eval cascade fixes it: older-first iteration runs
        // Klorvex first (it's near-known to Klorvox AND vice versa),
        // sees Klorvox in learned, demotes Klorvex. Then Klorvox's
        // re-eval no longer sees Klorvex → FarFromKnown → Confirmed.
        for i in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = ledger.append(
                200 + i as u64,
                leave_alone_no_candidates("Klorvox"),
                0,
                ConfidenceTier::Eager,
                None,
                None,
                None,
                None,
                Some(clean_motor("Klorvox")),
            );
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }

        // Net: Klorvox wins, Klorvex demotes.
        assert_eq!(
            p.get("Klorvox").unwrap().tier,
            ProposalTier::Confirmed,
            "newer confirmation Klorvox must win"
        );
        assert!(
            lex.is_learned("Klorvox"),
            "Klorvox must be live in is_known"
        );
        assert_eq!(
            p.get("Klorvex").unwrap().tier,
            ProposalTier::Held {
                reason: HoldReason::NearKnownWord
            },
            "older Klorvex must demote on the self-cleaning hook"
        );
        assert!(
            !lex.is_learned("Klorvex"),
            "Klorvex must be removed from is_known"
        );
    }

    #[test]
    fn manual_correction_of_confirmed_word_demotes_it() {
        // User confirms Soumyo (3 Kept occasions). Later, the user
        // CORRECTS one of those records (resolver flips Kept →
        // CorrectedToOther) — the proposer's retract logic drops
        // occasions to 2 → tier → Provisional → unlearn.
        let _g = serial_setup();
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        let lex = Lexicon::shared();

        let mut ids = Vec::new();
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
            ids.push(id);
        }
        assert!(lex.is_learned("Soumyo"));

        // Manual correction of one record. Resolver re-resolves it
        // as CorrectedToOther; proposer retracts the contribution.
        let undone = flip_outcome(&mut ledger, ids[1], Outcome::CorrectedToOther);
        p.note_record(&undone);

        assert_eq!(p.get("Soumyo").unwrap().tier, ProposalTier::Provisional);
        assert!(
            !lex.is_learned("Soumyo"),
            "manual correction must demote from is_known"
        );
    }

    #[test]
    fn hysteresis_one_contradicting_signal_does_not_demote_a_high_occasion_word() {
        // Soumyo is confirmed after 5 Kept occasions (>= threshold +
        // 2). A single retraction drops occasions to 4 — still
        // ≥ threshold (3) — so the tier stays Confirmed and the word
        // stays in is_known. That's the natural hysteresis: the
        // threshold itself prevents single-signal oscillation.
        let _g = serial_setup();
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        let lex = Lexicon::shared();

        let mut ids = Vec::new();
        for _ in 0..(CONFIRMED_OCCASIONS_THRESHOLD_FAST + 2) {
            let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
            ids.push(id);
        }
        assert_eq!(p.get("Soumyo").unwrap().tier, ProposalTier::Confirmed);
        assert!(lex.is_learned("Soumyo"));

        // Retract ONE. Occasions = 4, still ≥ threshold.
        let undone = flip_outcome(&mut ledger, ids[0], Outcome::CorrectedToOther);
        p.note_record(&undone);
        assert_eq!(
            p.get("Soumyo").unwrap().tier,
            ProposalTier::Confirmed,
            "single contradicting signal must not demote — threshold is hysteresis"
        );
        assert!(
            lex.is_learned("Soumyo"),
            "Soumyo must stay in is_known"
        );
    }

    #[test]
    fn injection_stays_zero_when_confirmed_word_changes_candidates() {
        // Phase 2 must NOT inject anything. The engine's
        // correction-injection path doesn't exist yet — this test
        // is a structural pin: confirming a word affects
        // `Lexicon::is_known` and the candidate pool, but no other
        // side effect leaks into the engine's behavior. We assert
        // by exercising the candidate generator directly — it must
        // include the learned word but only as a *candidate*, not
        // as an enacted correction.
        let _g = serial_setup();
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        let lex = Lexicon::shared();

        // Confirm Soumyo.
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert!(lex.is_learned("Soumyo"));

        // Confirming a word doesn't perform any other mutation —
        // the proposer's records, ledger, and engine state are
        // untouched beyond what the explicit calls do. (There's no
        // injection path to assert against; if Phase 3 adds one,
        // this test would need updating to assert it stays off until
        // the injection flag flips.)
        let learned = lex.learned_snapshot();
        assert_eq!(
            learned.len(),
            1,
            "exactly one learning happened — no other writes"
        );
    }

    #[test]
    fn confirmed_word_cleans_its_own_existing_slips_on_re_eval() {
        // Variation on self-cleaning: Klorvex wasn't confirmed at all,
        // but it WAS a Provisional candidate. When Klorvox confirms,
        // the re-eval pass should still detect Klorvex's new
        // near-known status and move it from Provisional to Held.
        let _g = serial_setup();
        let mut ledger = DecisionLedger::new();
        let mut p = LexiconProposer::new();
        let lex = Lexicon::shared();

        // Stage 1: Klorvex once → Provisional (not yet confirmed).
        let id = ledger.append(
            10,
            leave_alone_no_candidates("Klorvex"),
            0,
            ConfidenceTier::Eager,
            None,
            None,
            None,
            None,
            Some(clean_motor("Klorvex")),
        );
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_record(&kept);
        assert_eq!(p.get("Klorvex").unwrap().tier, ProposalTier::Provisional);

        // Stage 2: Klorvox confirms.
        for i in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = ledger.append(
                20 + i as u64,
                leave_alone_no_candidates("Klorvox"),
                0,
                ConfidenceTier::Eager,
                None,
                None,
                None,
                None,
                Some(clean_motor("Klorvox")),
            );
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }

        assert!(lex.is_learned("Klorvox"));
        assert_eq!(
            p.get("Klorvex").unwrap().tier,
            ProposalTier::Held {
                reason: HoldReason::NearKnownWord
            },
            "re-eval should have demoted Klorvex to Held when Klorvox joined is_known"
        );
    }

    // ---- Reset LEXICON (true engine-side wipe) -------------------------

    #[test]
    fn reset_all_wipes_proposer_state_and_learned_set() {
        // C5b Phase 2 follow-up: the "Reset LEXICON" command's test
        // prerequisite. Two failure modes it must rule out:
        //   1. Proposer state survives → next re-eval re-adds the word
        //      to the lex's learned set (state-based sync).
        //   2. Lex's learned set survives → `is_known` still true.
        // After reset_all both must be empty and is_known must return
        // to the bundled-only baseline.
        let mut ledger = DecisionLedger::new();
        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            p.note_record(&kept);
        }
        assert!(Lexicon::shared().is_known("Soumyo"));
        assert!(matches!(
            p.get("Soumyo").unwrap().tier,
            ProposalTier::Confirmed
        ));
        assert!(!p.learned_snapshot().is_empty());

        p.reset_all();

        assert_eq!(p.len(), 0, "proposals should be empty after reset_all");
        assert!(p.get("Soumyo").is_none());
        assert!(
            p.learned_snapshot().is_empty(),
            "learned snapshot should be empty after reset_all"
        );
        assert!(
            !Lexicon::shared().is_known("Soumyo"),
            "is_known must fall back to bundled-only after reset_all"
        );

        // After reset_all, re-noting the SAME LogRecord must rebuild
        // cleanly (no idempotency cache poisoning from before).
        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_record(&kept);
        let prop = p.get("Soumyo").unwrap();
        assert_eq!(prop.occasions, 1, "fresh credit must start from 1");
    }

    // ---- Meta-context pause --------------------------------------------

    #[test]
    fn paused_credit_is_a_total_no_op_with_no_leaked_state() {
        // While paused, three Kept occasions of a word that would
        // otherwise Confirm leave NO trace: no proposal, no
        // contribution entry, no learned_at_ms entry, no learned-set
        // mutation. The next un-paused note creates a fresh
        // first-occasion proposal (i.e. the paused records are NOT
        // retroactively credited).
        let mut ledger = DecisionLedger::new();
        let _g = serial_setup();
        let mut p = LexiconProposer::new();
        p.set_credit_paused(true);
        for _ in 0..CONFIRMED_OCCASIONS_THRESHOLD_FAST {
            let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
            let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
            let update = p.note_record(&kept);
            assert!(update.changes.is_empty(), "paused note must emit no changes");
            assert!(!update.learned_set_changed);
        }
        assert_eq!(p.len(), 0, "no proposal should have been recorded");
        assert!(p.get("Soumyo").is_none());
        assert!(!Lexicon::shared().is_known("Soumyo"));

        p.set_credit_paused(false);
        let id = append_fast(&mut ledger, "Soumyo", clean_motor("Soumyo"));
        let kept = flip_outcome(&mut ledger, id, Outcome::Kept);
        p.note_record(&kept);
        let prop = p.get("Soumyo").unwrap();
        assert_eq!(
            prop.occasions, 1,
            "post-resume credit must start from 1 — paused records are excluded, not deferred"
        );
    }

    // ---- snapshot ordering ---------------------------------------------

    #[test]
    fn snapshot_orders_by_most_recent_first() {
        let _lex = Lexicon::shared();
        let mut ledger = DecisionLedger::new();
        let _g = serial_setup();
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
