//! Component 5e — **Per-pattern correction classifier** (M3 Phase 1, module 2).
//!
//! The word-pattern store ([`crate::word_pattern`]) *learns* `typed → target`
//! corrections. This module *decides* what the engine would be allowed to do
//! with one — the per-pattern kill-switch from the M3 brief (CLAUDE.md → M3:
//! "correction turns on when the map is confident about *this* person's
//! patterns" — **per-pattern, not a global threshold**).
//!
//! ## One model: suggest-only. There is no silent path.
//!
//! Every correction this engine would ever make is a **suggestion the user
//! confirms** (confirm-to-accept). The earlier Tier-1 "silent auto-fix" outcome
//! is **retired** — nothing is ever marked for silent application, regardless of
//! how confident we are. This is a deliberate product stance: TypeAssist must
//! never feel like a "worse autocorrect" (Principle #9), and a survivor's typed
//! word is never overwritten without their say-so. So [`PatternReadiness`] has
//! exactly two outcomes:
//!
//! * **[`PatternReadiness::Suggest`]** — surface a confirm-to-accept suggestion
//!   (which evidence tier admitted it is recorded for observability).
//! * **[`PatternReadiness::Observe`]** — do nothing, with a reason. Not enough
//!   evidence yet, the target isn't a real word, the pair isn't a single motor
//!   slip, or the source/target is a single ambiguous letter.
//!
//! ## Risk-tiered evidence bar
//!
//! How much evidence we demand before suggesting depends on the **risk** that
//! the source word was actually *intended*:
//!
//! * **Non-word source** (`teh`, `wrd`) → a real-word target: low risk (the
//!   typed form isn't a word, so it's almost certainly a slip). Low bar —
//!   [`NONWORD_SOURCE_EVIDENCE_BAR`].
//! * **Real-word source** (in the dictionary *or* the user's learned vocab):
//!   high risk (the user may have meant the word they typed). High bar —
//!   [`REALWORD_SOURCE_EVIDENCE_BAR`]. This is the **hard vocabulary rule**
//!   (brief): a known token is treated cautiously, never cheaply rewritten.
//! * **Target not a real word** → always [`PatternReadiness::Observe`]: we never
//!   steer toward a non-word, however often the pair is seen. A real-but-uncommon
//!   target still earns a suggestion (commonness no longer gates the action now
//!   that nothing is silent).
//!
//! ## Structural disqualifiers (independent of evidence)
//!
//! Some pairs are never nomination candidates *at any count*:
//!
//! * **Not a single motor slip** ([`crate::slip_class::single_motor_edit`]) —
//!   word-swaps that aren't near-misses (`so → for`, `to → for`), non-adjacent
//!   substitutions, and apostrophe/punctuation/casing fixes (`dont → don't`).
//! * **Single-letter source** (`s → is`, `f → of`) — a lone typed letter is
//!   intent-ambiguous; never nominate it. (Legit single letters `a` / `i` are
//!   known words, so they take the cautious real-word path anyway and are never
//!   themselves a correction target — see below.)
//! * **Single-letter target** — never steer *toward* a bare letter, for the same
//!   ambiguity reason.
//!
//! On top of all this, the **3-strike safety brake**: if the user has undone a
//! pattern's suggestion [`UNDO_BRAKE_STRIKES`] times in a row
//! (`consecutive_undos`), the suggestion is suppressed
//! ([`ObserveReason::BrakeTripped`]) until it rebuilds.
//!
//! ## Shape, mirroring [`crate::decision`]
//!
//! The core is a **pure function** over injected [`PatternFacts`] — no
//! lexicon, no store, no I/O — so every gate is unit-testable in isolation.
//! [`classify`] is the thin convenience wrapper that assembles the facts from a
//! [`WordPatternStore`] + [`Lexicon`]. Every [`PatternReadiness::Observe`]
//! outcome carries a reason, so the debug surface never has to guess why the
//! engine stayed quiet.
//!
//! ## Still OFF
//!
//! Phase 1 is observe-and-classify only. Nothing reads a [`PatternReadiness`]
//! into an actual suggestion yet — that wiring (and the undo/accept feedback
//! that drives the brake) is later. This module makes the *decision*
//! inspectable so we can judge correction quality against real data before a
//! single suggestion is ever shown.

use serde::{Deserialize, Serialize};

use crate::lexicon::Lexicon;
use crate::motor_map::{MotorMap, HALF_LIFE_MS};
use crate::word_pattern::{WordPatternStore, UNDO_BRAKE_STRIKES};

/// Bump on any change to the classification policy or the serialized
/// [`PatternReadiness`] shape.
///
/// v2 (2026-06-17): non-motor pairs (`dont → don't`) classify as not-a-motor-slip
///   and can never be nominated.
/// v3 (2026-06-18): **risk-tiered, suggest-only rewrite.** Tier-1 silent auto-fix
///   retired; every correction is a confirm-to-accept suggestion. Evidence bar
///   now depends on whether the source is a real word (low bar for non-words,
///   high bar for dictionary/learned-vocab words). Added single-letter
///   source/target guards and the strict single-motor-error nomination filter.
///   Target-commonness no longer gates (nothing is silent to gate).
pub const KILL_SWITCH_VERSION: u32 = 3;

/// **EASILY-FLIPPED CONSTANT — placeholder, calibrate later.** Decayed
/// occurrence weight a **non-word-source** pattern (e.g. `teh → the`) must reach
/// before it is suggested. Low, because a non-word typed form is almost
/// certainly a slip — the risk of suggesting against the user's intent is small.
///
/// Set to ~4 as a conservative starting point: enough that a one-off fix doesn't
/// surface, low enough that a clearly-repeated non-word slip earns a suggestion
/// quickly. Re-pin from real `word_patterns.json` data once the suggestion UI is
/// live and accept/undo feedback exists.
pub const NONWORD_SOURCE_EVIDENCE_BAR: f32 = 4.0;

/// **EASILY-FLIPPED CONSTANT.** Decayed occurrence weight a **real-word-source**
/// pattern must reach before it is suggested — the cautious, high bar the **hard
/// vocabulary rule** demands. A token in the dictionary *or* the user's learned
/// vocabulary may well have been intended, so we wait for strong, repeated
/// evidence before ever suggesting it be changed.
///
/// **Calibrated against the real store (610 patterns, 2026-06-17) at 12.** After
/// filtering non-motor pairs, the motor slips leave a clean empty band between
/// the repeatedly-seen unambiguous pairs and the ambiguous tail; 12 falls in
/// that gap. 12 favours precision — a wrong correction costs the user as much as
/// a missed one. This is also the value the Impact panel uses for its "Ready"
/// grouping (`apps/tauri` `read_word_patterns`). Re-calibrate once more distinct
/// real-word-source motor pairs accrue.
pub const REALWORD_SOURCE_EVIDENCE_BAR: f32 = 12.0;

/// **EASILY-FLIPPED CONSTANT — placeholder.** The lowest the evidence bar can be
/// eased to for a **non-word source** when the slip lands on a strongly-affected
/// key (the Motor-Map-aware bonus). Physiology substitutes for repetition: a
/// slip on a key the motor map shows slipping often is almost certainly a true
/// motor miss, so it needs far fewer sightings. ~2 is a deliberately generous
/// floor; the teach-stop (3-strike undo brake) keeps a generous bar from
/// becoming nagging once suggestions surface live.
pub const AFFECTED_BAR_FLOOR: f32 = 2.0;

/// **EASILY-FLIPPED CONSTANT — placeholder.** The key-affectedness (decayed slip
/// rate, `[0,1]`) at which the motor bonus **saturates** — i.e. the bar is eased
/// all the way to its floor. A key slipping at or above this rate counts as
/// "strongly affected." Below it the easing is graded (linear in affectedness),
/// so the bar relaxes smoothly as the map's evidence grows and tightens again as
/// the hand recovers. 0.20 (a slip roughly one press in five) is a conservative
/// starting point; calibrate against the shadow log (sharp vs noisy).
pub const STRONG_AFFECTEDNESS: f32 = 0.20;

/// **EASILY-FLIPPED CONSTANT — placeholder, calibrate from dogfooding.**
/// Unigram-frequency floor for [`is_host_redundant`]'s *target*: at or above
/// this count the target is a "common" dictionary word a host app's own
/// autocorrect reliably converges to, so a `(non-word → this)` fix is one the
/// host would *also* make. Below it the fix is idiosyncratic and stays Jordan's
/// territory.
///
/// Anchored against `lexicon/words_freq.txt`: `the` ≈ 23.1B, `have` ≈ 1.56B,
/// `because` ≈ 271M, `application` ≈ 153M, `browser` ≈ 65M, `recovery` ≈ 38M,
/// `train` ≈ 32M all clear `10M`; `inconsistency` ≈ 1.0M, `perseverance`
/// ≈ 826K, and absent words (`reminiscence`, `kubernetes` → 0) fall below it.
/// 10M is a deliberate **fail-toward-firing** starting point — set high enough
/// that only clearly-common words are treated as host-redundant. Re-pin from a
/// real coexistence dogfooding session (CLAUDE.md → "Watch Dog Not Attack Dog").
pub const COMMON_FREQ_THRESHOLD: u64 = 10_000_000;

/// **QA-11 Option A "common" pair predicate** (CLAUDE.md → M3 autocorrect
/// coexistence, "Watch Dog Not Attack Dog"). True iff `(typed → target)` is the
/// kind of fix a host app's *own* corrector would also make — a **non-word**
/// typed form converging on a **common** dictionary target. These are the
/// *redundant* fixes Jordan stands down on **when** he behaviourally senses an
/// active competing corrector; everything else is HIS territory and always
/// fires:
///
/// * a **real-word source** (`is_known(typed)`) — the hard vocabulary rule; the
///   user may have meant it, never cheaply deferred;
/// * an **uncommon / idiosyncratic target** (`reminiscence`, `inconsistency`)
///   or a **learned-personal** pattern (`kubernetis → kubernetes`, target
///   absent from the freq table → 0) — the motor garbles a host won't catch.
///
/// Pure over the injected [`Lexicon`]; unit-testable in isolation. "Common" is a
/// property of the *pair*, never a lane or app gate — `teh → the` is redundant
/// (the host fixes it) while `teh → tech` (uncommon-ish) need not be. The
/// behavioural "is a competitor active right now" decision lives in the engine
/// (`CompetitorSense`); this predicate only answers "is this fix common".
pub fn is_host_redundant(typed: &str, target: &str, lexicon: &Lexicon) -> bool {
    !lexicon.is_known(typed) && lexicon.frequency(target) >= COMMON_FREQ_THRESHOLD
}

// ---- Shadow-curve experiment (observe-only) --------------------------------
//
// A *length-scaled* variant of the evidence bar and the motor-edit budget, read
// ONLY by the shadow dry-run (`shadow_suggestion` in the app's engine). The live
// Impact ledger, the KILL_SWITCH_CLASSIFY debug line, and `single_motor_edit`
// are unchanged — nothing here is ever applied (`correction_enabled` stays off).
// Intent: longer words are typed less ambiguously (a 7-letter non-word is almost
// never the word the user meant) and slip in more than one place, so the bar
// drops and the motor-edit budget grows with length. Real-word sources keep the
// high cautious bar regardless of length.

/// **EASILY-FLIPPED CONSTANTS — shadow curve.** Non-word-source evidence bar by
/// source-word length: short (2–3) keeps the flat 4; mid (4–6) eases to 3; long
/// (7+) drops to 1, where a single confident sighting suffices. Real-word sources
/// ignore this and keep [`REALWORD_SOURCE_EVIDENCE_BAR`].
pub const SHADOW_NONWORD_BAR_SHORT: f32 = 4.0;
pub const SHADOW_NONWORD_BAR_MID: f32 = 3.0;
pub const SHADOW_NONWORD_BAR_LONG: f32 = 1.0;

/// The length-scaled non-word-source base bar (see the constants above).
pub fn shadow_nonword_bar_for_len(source_len: usize) -> f32 {
    match source_len {
        0..=3 => SHADOW_NONWORD_BAR_SHORT,
        4..=6 => SHADOW_NONWORD_BAR_MID,
        _ => SHADOW_NONWORD_BAR_LONG,
    }
}

/// The length-scaled motor-edit budget (max motor-class errors a candidate may
/// differ by): short words (2–3) allow 1, length 4+ allow up to 2. Feeds
/// [`crate::slip_class::motor_edit_within_budget`].
///
/// **Note (2026-06-18):** this length-scaled budget drives the learned /
/// length-band shadow classifier ([`classify_explained_scaled`]). The
/// dictionary-driven **bold convergence** path no longer uses it — it is capped
/// at a single motor edit ([`SHADOW_BOLD_MOTOR_BUDGET`]) regardless of length, so
/// only unambiguous one-slip non-words converge. The budget-2 machinery is left
/// in place (unused by the bold path) for a possible later spelling pass.
pub fn shadow_motor_budget_for_len(source_len: usize) -> usize {
    if source_len <= 3 {
        1
    } else {
        2
    }
}

/// **Bold-convergence edit cap.** The dictionary-driven, first-sighting bold
/// path admits a candidate only when it is reachable by a **single** motor edit
/// (one adjacent-key sub, one transposition, one drop, or one insert) at *every*
/// length — never two. A two-edit pair is ambiguous enough that it should earn
/// its suggestion through learned evidence, not first-sighting convergence, so it
/// falls through to the length-band path instead.
pub const SHADOW_BOLD_MOTOR_BUDGET: usize = 1;

/// Recency window: a pattern not observed within this many ms is "stale" — the
/// hand may have grown out of it — and won't be suggested until refreshed. One
/// 30-day half-life, shared with the motor map's decay so the two layers forget
/// on the same clock.
pub const STALE_AFTER_MS: f32 = HALF_LIFE_MS;

/// What the engine would be allowed to do for one learned pattern. Observe-only
/// in Phase 1 — a *classification*, never an injection. **Two outcomes only:
/// there is no silent path** (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PatternReadiness {
    /// Eligible to surface as a confirm-to-accept suggestion. `tier` records
    /// which evidence bar admitted it, for observability.
    Suggest { tier: SuggestTier },
    /// Do nothing, with a reason — not a candidate (yet, or ever).
    Observe { reason: ObserveReason },
}

/// Which risk tier admitted a [`PatternReadiness::Suggest`]. Both surface the
/// *same* confirm-to-accept suggestion; the tier only records the evidence bar
/// the pattern cleared (useful for the debug/Impact surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestTier {
    /// Source is not a real word — cleared the low [`NONWORD_SOURCE_EVIDENCE_BAR`].
    NonWordSource,
    /// Source is a real word (dictionary or learned vocab) — cleared the high,
    /// cautious [`REALWORD_SOURCE_EVIDENCE_BAR`] (the hard vocabulary rule).
    RealWordSource,
}

/// Why a pattern is observed rather than suggested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserveReason {
    /// No such pattern has ever been observed.
    Unseen,
    /// The `typed → target` pair isn't a single motor error — a word-swap that
    /// isn't a near-miss (`so → for`), a non-adjacent substitution, or a
    /// punctuation/apostrophe/casing fix (`dont → don't`). Never a candidate,
    /// however often it's seen.
    NotAMotorSlip,
    /// The typed form is a single letter (`s → is`) — intent-ambiguous, never
    /// nominated at any count.
    SingleLetterSource,
    /// The target is a single letter — we never steer toward a bare letter.
    SingleLetterTarget,
    /// The target isn't a real word — never a valid correction destination
    /// (learning the word is the lexicon proposer's job, not ours).
    TargetNotAWord,
    /// Decayed weight is below the applicable evidence bar
    /// ([`NONWORD_SOURCE_EVIDENCE_BAR`] or [`REALWORD_SOURCE_EVIDENCE_BAR`]) —
    /// not enough recent evidence to suggest yet.
    InsufficientEvidence,
    /// Last observed longer ago than [`STALE_AFTER_MS`] — the hand may have
    /// moved on; wait for it to recur before suggesting.
    Stale,
    /// The user has undone this pattern's suggestion [`UNDO_BRAKE_STRIKES`]
    /// times in a row — suppressed until it rebuilds (the 3-strike brake).
    BrakeTripped,
}

/// The pre-computed facts the policy decides on — injected so the decision is
/// a pure function (mirrors how [`crate::decision::decide`] takes `is_known`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PatternFacts {
    /// Decayed occurrence weight of this `typed → target` pattern.
    pub weight: f32,
    /// Time since the pattern was last observed (ms), for the recency gate.
    pub age_ms: u64,
    /// Consecutive user-undos of this pattern's suggestion (the brake).
    pub consecutive_undos: u32,
    /// Is the typed word itself a real word — in the dictionary **or** the
    /// user's learned vocabulary? Drives the risk tier (the hard vocabulary
    /// rule). [`Lexicon::is_known`] already unions both sets.
    pub typed_is_known: bool,
    /// Is the target a real word at all? A non-word target is never suggested.
    pub target_is_known: bool,
    /// Character length of the typed (source) word — `1` is disqualifying.
    pub source_len: usize,
    /// Character length of the target word — `1` is disqualifying.
    pub target_len: usize,
    /// Is `typed → target` a single motor error (adjacent substitution,
    /// transposition, one dropped/extra letter)? Computed by
    /// [`crate::slip_class::single_motor_edit`].
    pub is_single_motor_error: bool,
    /// Affectedness of the **most-affected key the slip involves**, in `[0, 1]`
    /// — the motor map's decayed slip rate for that key (see
    /// [`crate::MotorMap::affectedness`]). `0.0` when no involved key is
    /// well-sampled. This eases the evidence bar (physiology substitutes for
    /// repetition); it **never** crosses a structural gate. The *most*-affected
    /// key drives it: one strongly-affected key in the slip is enough signal.
    pub key_affectedness: f32,
}

/// The risk-tiered evidence bar for a pattern, eased by motor-map affectedness.
/// Returns `(bar_used, base_bar, tier)`.
///
/// `base_bar` is the un-eased tier bar (high for a real-word source, low for a
/// non-word source). `bar_used` is `base_bar` relaxed toward a **floor** in
/// proportion to `key_affectedness` (saturating at [`STRONG_AFFECTEDNESS`]):
/// a strongly-affected key needs far fewer sightings. The floor is tier-aware so
/// the **hard vocabulary rule** still holds — a real-word source, even fully
/// eased, never drops below the *non-word base bar*, so it stays strictly more
/// cautious than a non-word slip. Easing only lowers the *repetition* needed; it
/// never touches a structural gate or makes anything silent.
fn evidence_bar(typed_is_known: bool, key_affectedness: f32) -> (f32, f32, SuggestTier) {
    let (base_bar, floor, tier) = if typed_is_known {
        // Real-word floor = the non-word BASE bar: eased, but never as cheap as
        // a non-word slip — caution preserved.
        (
            REALWORD_SOURCE_EVIDENCE_BAR,
            NONWORD_SOURCE_EVIDENCE_BAR,
            SuggestTier::RealWordSource,
        )
    } else {
        (
            NONWORD_SOURCE_EVIDENCE_BAR,
            AFFECTED_BAR_FLOOR,
            SuggestTier::NonWordSource,
        )
    };
    // Graded, saturating at STRONG_AFFECTEDNESS: t ∈ [0,1] is how far toward the
    // floor we ease.
    let t = (key_affectedness / STRONG_AFFECTEDNESS).clamp(0.0, 1.0);
    let bar_used = base_bar - t * (base_bar - floor);
    (bar_used, base_bar, tier)
}

/// Shadow-curve variant of [`evidence_bar`]: the non-word base bar is
/// **length-scaled** ([`shadow_nonword_bar_for_len`]) instead of the flat
/// [`NONWORD_SOURCE_EVIDENCE_BAR`]. Real-word sources are **unchanged** — same
/// high [`REALWORD_SOURCE_EVIDENCE_BAR`] base and same [`NONWORD_SOURCE_EVIDENCE_BAR`]
/// floor, so the hard vocabulary rule is untouched. Motor-Map easing applies on
/// top exactly as before, but the non-word floor is clamped to the (possibly
/// lower) base so easing can only ever *lower* the bar — a long-word base of 1
/// has no easing room and stays 1.
fn evidence_bar_scaled(
    typed_is_known: bool,
    key_affectedness: f32,
    source_len: usize,
) -> (f32, f32, SuggestTier) {
    let (base_bar, floor, tier) = if typed_is_known {
        (
            REALWORD_SOURCE_EVIDENCE_BAR,
            NONWORD_SOURCE_EVIDENCE_BAR,
            SuggestTier::RealWordSource,
        )
    } else {
        let base = shadow_nonword_bar_for_len(source_len);
        // Clamp the floor to the base: a base already below AFFECTED_BAR_FLOOR
        // (the long-word bar of 1) must not be *raised* by "easing".
        (base, AFFECTED_BAR_FLOOR.min(base), SuggestTier::NonWordSource)
    };
    let t = (key_affectedness / STRONG_AFFECTEDNESS).clamp(0.0, 1.0);
    let bar_used = base_bar - t * (base_bar - floor);
    (bar_used, base_bar, tier)
}

/// A classification plus the *why* the shadow log surfaces: the affectedness
/// score, the bar actually used, the un-eased base bar, and whether the
/// motor-map bonus is what let it surface (`surfaced_early`). Observe-only
/// diagnostics — the verdict itself is [`Self::readiness`].
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifyExplain {
    pub readiness: PatternReadiness,
    /// Most-affected involved key's slip rate, `[0, 1]`.
    pub affectedness: f32,
    /// The evidence bar actually applied (eased by affectedness).
    pub bar_used: f32,
    /// The un-eased tier base bar (what would apply with no motor evidence).
    pub base_bar: f32,
    /// The motor bonus lowered the bar below the base bar.
    pub eased_by_motor: bool,
    /// This is a `Suggest` that **only** surfaced because of the easing — its
    /// weight is below the base bar, so without the affected-key bonus it would
    /// still be `Observe { InsufficientEvidence }`.
    pub surfaced_early: bool,
}

/// Classify one pattern from pre-computed facts. **Pure** — no lexicon, no
/// store, no I/O. Thin wrapper over [`classify_pattern_explained`].
pub fn classify_pattern(f: &PatternFacts) -> PatternReadiness {
    classify_pattern_explained(f).readiness
}

/// Classify one pattern AND explain the evidence-bar reasoning (for the shadow
/// log). **Pure.**
///
/// Order: structural disqualifiers first (a pair that's the wrong *shape* is
/// never a candidate, independent of evidence — and **independent of
/// affectedness**), then "no confidence yet" gates, then the motor-eased
/// risk-tiered evidence bar, then the operational brake. Anything that survives
/// is a confirm-to-accept suggestion tagged with its tier.
pub fn classify_pattern_explained(f: &PatternFacts) -> ClassifyExplain {
    let (bar_used, base_bar, tier) = evidence_bar(f.typed_is_known, f.key_affectedness);
    classify_core(f, bar_used, base_bar, tier)
}

/// Shadow-curve variant of [`classify_pattern_explained`]: identical gate
/// sequence, but the evidence bar is the **length-scaled** [`evidence_bar_scaled`]
/// (non-word base by source length; real-word unchanged). The structural
/// motor-shape gate (`f.is_single_motor_error`) is assumed to have been computed
/// with the **length-scaled motor-edit budget** by the caller's fact-builder.
/// Pure. Read only by the shadow dry-run — nothing here is ever applied.
pub fn classify_pattern_explained_scaled(f: &PatternFacts) -> ClassifyExplain {
    let (bar_used, base_bar, tier) =
        evidence_bar_scaled(f.typed_is_known, f.key_affectedness, f.source_len);
    classify_core(f, bar_used, base_bar, tier)
}

/// The shared gate sequence over pre-computed facts and an already-resolved
/// evidence bar. Order: structural disqualifiers first (a pair that's the wrong
/// *shape* is never a candidate, independent of evidence — and independent of
/// affectedness), then "no confidence yet" gates, then the evidence bar, then the
/// operational brake. Anything that survives is a confirm-to-accept suggestion
/// tagged with its tier.
fn classify_core(
    f: &PatternFacts,
    bar_used: f32,
    base_bar: f32,
    tier: SuggestTier,
) -> ClassifyExplain {
    let eased_by_motor = bar_used < base_bar;
    let explain = |readiness: PatternReadiness, surfaced_early: bool| ClassifyExplain {
        readiness,
        affectedness: f.key_affectedness,
        bar_used,
        base_bar,
        eased_by_motor,
        surfaced_early,
    };

    // --- Structural disqualifiers: never a candidate at any count. ---
    if !f.is_single_motor_error {
        return explain(
            PatternReadiness::Observe {
                reason: ObserveReason::NotAMotorSlip,
            },
            false,
        );
    }
    if f.source_len == 1 {
        return explain(
            PatternReadiness::Observe {
                reason: ObserveReason::SingleLetterSource,
            },
            false,
        );
    }
    if f.target_len == 1 {
        return explain(
            PatternReadiness::Observe {
                reason: ObserveReason::SingleLetterTarget,
            },
            false,
        );
    }

    // --- Confidence gates: nothing to suggest yet. ---
    if f.weight <= 0.0 {
        return explain(
            PatternReadiness::Observe {
                reason: ObserveReason::Unseen,
            },
            false,
        );
    }
    // A non-word target can never be a valid suggestion destination, so it
    // disqualifies regardless of how often it's been seen.
    if !f.target_is_known {
        return explain(
            PatternReadiness::Observe {
                reason: ObserveReason::TargetNotAWord,
            },
            false,
        );
    }

    // Motor-eased risk-tiered evidence bar. `bar_used` is the tier base relaxed
    // toward its floor in proportion to the affected key's slip rate.
    if f.weight < bar_used {
        return explain(
            PatternReadiness::Observe {
                reason: ObserveReason::InsufficientEvidence,
            },
            false,
        );
    }
    // Compare in f64: a 30-day window is ~2.6e9 ms, past f32's integer
    // precision (~256ms ULP), so an f32 compare would quantise the boundary.
    if f.age_ms as f64 > f64::from(STALE_AFTER_MS) {
        return explain(
            PatternReadiness::Observe {
                reason: ObserveReason::Stale,
            },
            false,
        );
    }
    // Operational override: suppress a fully-qualified suggestion while the user
    // is actively rejecting it (the teach-stop, so a generous bar can't nag).
    if f.consecutive_undos >= UNDO_BRAKE_STRIKES {
        return explain(
            PatternReadiness::Observe {
                reason: ObserveReason::BrakeTripped,
            },
            false,
        );
    }

    // Surfaced. It's "early" iff it cleared only the eased bar — its weight is
    // below the un-eased base bar, so the affected-key bonus is what surfaced it.
    let surfaced_early = f.weight < base_bar;
    explain(PatternReadiness::Suggest { tier }, surfaced_early)
}

/// Assemble [`PatternFacts`] for `typed → target` from a [`WordPatternStore`]
/// (weight / recency / brake), a [`Lexicon`] (membership), the strict
/// [`crate::slip_class::single_motor_edit`] (motor shape), and a [`MotorMap`]
/// (per-key affectedness over the slip's involved keys). Recency is measured
/// against the store's `last_now`, consistent with how the store decays reads.
///
/// `typed_is_known` uses [`Lexicon::is_known`], which already unions the bundled
/// dictionary with the user's runtime-learned vocabulary — so the hard
/// vocabulary rule covers "dictionary OR my learned vocab" with one call.
/// `key_affectedness` is the MAX affectedness over the slip's involved keys
/// ([`crate::slip_class::involved_keys`]) — one strongly-affected key suffices.
fn build_facts(
    typed: &str,
    target: &str,
    store: &WordPatternStore,
    lexicon: &Lexicon,
    motor_map: &MotorMap,
) -> PatternFacts {
    // The strict single-motor filter (budget fixed at 1) is the live motor gate.
    let is_single_motor_error = crate::slip_class::single_motor_edit(typed, target).is_some();
    build_facts_with_motor(typed, target, store, lexicon, motor_map, is_single_motor_error)
}

/// Shadow-curve fact-builder: identical to [`build_facts`] except the motor-shape
/// gate uses the **length-scaled** motor-edit budget
/// ([`shadow_motor_budget_for_len`] → [`crate::slip_class::motor_edit_within_budget`]),
/// so length-4+ words may be admitted at up to 2 motor errors.
fn build_facts_scaled(
    typed: &str,
    target: &str,
    store: &WordPatternStore,
    lexicon: &Lexicon,
    motor_map: &MotorMap,
) -> PatternFacts {
    let source_len = crate::word_pattern::normalize_word(typed).chars().count();
    let budget = shadow_motor_budget_for_len(source_len);
    let is_single_motor_error =
        crate::slip_class::motor_edit_within_budget(typed, target, budget).is_some();
    build_facts_with_motor(typed, target, store, lexicon, motor_map, is_single_motor_error)
}

/// Shared fact assembly given an already-decided motor-shape gate. The motor gate
/// is the only thing that differs between the live (budget-1) and shadow
/// (length-scaled budget) paths; everything else is identical.
fn build_facts_with_motor(
    typed: &str,
    target: &str,
    store: &WordPatternStore,
    lexicon: &Lexicon,
    motor_map: &MotorMap,
    is_single_motor_error: bool,
) -> PatternFacts {
    // Structural facts hold regardless of whether the pattern was ever observed,
    // so compute them from the raw pair. A missing pattern still flows through
    // `classify_pattern` (weight 0 → Unseen) unless a structural gate fires
    // first — reporting the more fundamental reason.
    let source_len = crate::word_pattern::normalize_word(typed).chars().count();
    let target_len = crate::word_pattern::normalize_word(target).chars().count();

    // Affectedness of the most-affected key the slip touches. A well-sampled
    // strongly-slipping key eases the bar; an unsampled key contributes 0.0.
    let key_affectedness = crate::slip_class::involved_keys(typed, target)
        .into_iter()
        .map(|k| motor_map.affectedness(k))
        .fold(0.0_f32, f32::max);

    let snap = store.snapshot(typed, target);
    let (weight, age_ms, consecutive_undos) = match &snap {
        Some(s) => (
            s.weight,
            store.last_now().saturating_sub(s.last_update),
            s.consecutive_undos,
        ),
        None => (0.0, 0, 0),
    };

    PatternFacts {
        weight,
        age_ms,
        consecutive_undos,
        typed_is_known: lexicon.is_known(typed),
        target_is_known: lexicon.is_known(target),
        source_len,
        target_len,
        is_single_motor_error,
        key_affectedness,
    }
}

/// Classify `typed → target` against the live stores. Motor-map-aware: a slip on
/// a strongly-affected key needs fewer sightings to surface (see
/// [`evidence_bar`]). Pass an empty [`MotorMap`] for the un-eased base behaviour.
pub fn classify(
    typed: &str,
    target: &str,
    store: &WordPatternStore,
    lexicon: &Lexicon,
    motor_map: &MotorMap,
) -> PatternReadiness {
    classify_pattern(&build_facts(typed, target, store, lexicon, motor_map))
}

/// Like [`classify`] but returns the full [`ClassifyExplain`] — affectedness,
/// the eased bar, the base bar, and whether the motor bonus is what surfaced it.
/// The shadow log reads this to mark early-surfacing suggestions.
pub fn classify_explained(
    typed: &str,
    target: &str,
    store: &WordPatternStore,
    lexicon: &Lexicon,
    motor_map: &MotorMap,
) -> ClassifyExplain {
    classify_pattern_explained(&build_facts(typed, target, store, lexicon, motor_map))
}

/// Shadow-curve variant of [`classify_explained`]: the **length-scaled** evidence
/// bar ([`evidence_bar_scaled`]) and the **length-scaled** motor-edit budget
/// ([`build_facts_scaled`]). Every other gate — target must be a real word, no
/// single-letter source/target, the motor-class shape requirement, the real-word
/// caution, recency, and the undo brake — is unchanged, and Motor-Map easing
/// still applies on top of the bar. Read ONLY by the shadow dry-run; nothing here
/// is ever applied. Pass an empty [`MotorMap`] for the un-eased base behaviour.
pub fn classify_explained_scaled(
    typed: &str,
    target: &str,
    store: &WordPatternStore,
    lexicon: &Lexicon,
    motor_map: &MotorMap,
) -> ClassifyExplain {
    classify_pattern_explained_scaled(&build_facts_scaled(typed, target, store, lexicon, motor_map))
}

// ---- Shadow convergence model (dictionary-driven, observe-only) ------------
//
// A *generative* counterpart to the learned-pattern shadow classifier. Instead
// of waiting for a `typed → target` pair to accrue evidence, it asks: for a
// just-sealed token, how many **base-dictionary words** are reachable from it by
// a **single** motor edit? When a NON-word converges on exactly ONE dictionary
// word — and that word is not one the user has registered as their own — the slip
// is unambiguous and can surface on **first sighting**, no evidence threshold.
// Two-or-more candidates (genuine ambiguity), anything reachable only via *two*
// edits, or a real-word source fall back to the learned / length-band path. Read
// only by the shadow dry-run; nothing here is ever applied. (The length-scaled
// budget-2 machinery stays in place but is no longer used by this path — see
// [`SHADOW_BOLD_MOTOR_BUDGET`].)

/// The result of scanning the base dictionary for motor-edit candidates of a
/// just-sealed token. Observe-only.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvergenceScan {
    /// Base-dictionary words reachable from the typed token within a **single**
    /// motor edit ([`SHADOW_BOLD_MOTOR_BUDGET`]) — the real-word candidates
    /// (step 3). Empty when the source is a known word (no generation needed).
    pub candidates: Vec<String>,
    /// Motor-edit cap applied to candidate generation — always
    /// [`SHADOW_BOLD_MOTOR_BUDGET`] (1) for the bold path.
    pub budget: usize,
    /// The typed token is not a known word — neither in the base dictionary nor
    /// in the user's learned vocabulary (`!Lexicon::is_known`).
    pub typed_is_non_word: bool,
    /// Max motor-map affectedness over the keys the single-candidate edit
    /// touches (`0.0` unless there is exactly one candidate). Surfaced even when
    /// `~0` today, per the brief — the signal to watch as the map fills in.
    pub affectedness: f32,
    /// The convergence verdict: a non-word source, exactly one base-dictionary
    /// candidate, and that candidate is not the user's own learned word. This is
    /// the "would-suggest (bold)" condition — fires regardless of how many times
    /// the pair has been seen.
    pub convergent: bool,
}

/// Exhaustive convergence-candidate scan: every clean-dictionary word reachable
/// from `typed_n` within `budget` motor edits, found by walking the whole
/// dictionary. O(lexicon) per call. Retained as (a) the fallback for any budget
/// the single-edit neighbourhood generator doesn't cover and (b) the correctness
/// oracle the parity test pins [`convergence_candidates_neighborhood`] against.
fn convergence_candidates_fullscan(typed_n: &str, lexicon: &Lexicon, budget: usize) -> Vec<String> {
    let tlen = typed_n.chars().count() as isize;
    let budget_i = budget as isize;
    let mut candidates: Vec<String> = Vec::new();
    for w in lexicon.iter_clean() {
        // Length pre-filter: a motor edit changes length by at most `budget`, so
        // anything outside that band can't be a candidate — skip the DP.
        let wl = w.chars().count() as isize;
        if (wl - tlen).abs() > budget_i {
            continue;
        }
        if crate::slip_class::motor_edit_within_budget(typed_n, w, budget).is_some() {
            candidates.push(w.to_string());
        }
    }
    candidates.sort();
    candidates
}

/// Fast convergence-candidate generation for the single-edit bold budget:
/// generate `typed_n`'s motor-edit neighbourhood
/// ([`crate::slip_class::motor_edit_neighborhood`]), keep the entries that are
/// clean-dictionary words, and re-validate each under the exact same
/// [`crate::slip_class::motor_edit_within_budget`] rule the full scan uses.
/// Superset generation + exact re-validation ⇒ provably the identical set as
/// [`convergence_candidates_fullscan`] at `budget == 1` (pinned by the
/// `convergence_neighborhood_matches_fullscan_parity` test), at a cost that
/// scales with the typed word rather than the ~81k-word dictionary.
fn convergence_candidates_neighborhood(
    typed_n: &str,
    lexicon: &Lexicon,
    budget: usize,
) -> Vec<String> {
    let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for cand in crate::slip_class::motor_edit_neighborhood(typed_n) {
        if lexicon.is_in_clean(&cand)
            && crate::slip_class::motor_edit_within_budget(typed_n, &cand, budget).is_some()
        {
            set.insert(cand);
        }
    }
    let mut candidates: Vec<String> = set.into_iter().collect();
    candidates.sort();
    candidates
}

/// Scan the base dictionary for motor-edit candidates of `typed` and decide the
/// convergence verdict (see [`ConvergenceScan`]). **Read-only.**
///
/// Step 1 (skip the user's own words) is handled by `typed_is_non_word`: a token
/// in the dictionary *or* the user's learned vocabulary is a known word, so no
/// candidates are generated and it can never be a bold candidate. Candidate
/// generation (step 2) enumerates the base dictionary ([`Lexicon::iter_clean`]),
/// keeping every word reachable by a **single** motor edit
/// ([`SHADOW_BOLD_MOTOR_BUDGET`]) under the adjacency-restricted rule
/// ([`crate::slip_class::motor_edit_within_budget`]). A cheap length pre-filter
/// skips words that can't possibly be one edit away.
pub fn shadow_convergence_scan(
    typed: &str,
    lexicon: &Lexicon,
    motor_map: &MotorMap,
) -> ConvergenceScan {
    let typed_n = crate::word_pattern::normalize_word(typed);
    // The bold path is capped at a SINGLE motor edit at every length (not the
    // length-scaled budget) — only unambiguous one-slip non-words converge.
    let budget = SHADOW_BOLD_MOTOR_BUDGET;
    let typed_is_non_word = !typed_n.is_empty() && !lexicon.is_known(&typed_n);

    // Candidate generation (steps 2–3). At the single-edit bold budget we
    // generate the typed token's motor-edit neighbourhood and probe the lexicon —
    // O(word), not O(dictionary) — which is the fix for the ~840ms long-word fire
    // latency; any other budget falls back to the exhaustive scan. The two return
    // the IDENTICAL set at budget 1, pinned by
    // `convergence_neighborhood_matches_fullscan_parity`. Both sort the result, so
    // the "single candidate" verdict is stable across the OS-nondeterministic
    // HashSet iteration in the lexicon.
    let candidates: Vec<String> = if !typed_is_non_word {
        Vec::new()
    } else if budget == 1 {
        convergence_candidates_neighborhood(&typed_n, lexicon, budget)
    } else {
        convergence_candidates_fullscan(&typed_n, lexicon, budget)
    };

    // Affectedness of the edit's keys, meaningful only when one candidate exists.
    let affectedness = if candidates.len() == 1 {
        crate::slip_class::involved_keys(&typed_n, &candidates[0])
            .into_iter()
            .map(|k| motor_map.affectedness(k))
            .fold(0.0_f32, f32::max)
    } else {
        0.0
    };

    // Convergence (step 4): non-word source, exactly one candidate, and that
    // candidate is not a word the user has registered as their own.
    let convergent =
        typed_is_non_word && candidates.len() == 1 && !lexicon.is_learned(&candidates[0]);

    ConvergenceScan {
        candidates,
        budget,
        typed_is_non_word,
        affectedness,
        convergent,
    }
}

// Sanity at compile time.
const _: () = assert!(KILL_SWITCH_VERSION >= 1);
const _: () = assert!(NONWORD_SOURCE_EVIDENCE_BAR <= REALWORD_SOURCE_EVIDENCE_BAR);

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Outcome;

    #[test]
    fn host_redundant_only_for_nonword_source_to_common_target() {
        let lex = Lexicon::shared();
        // Non-word source → common target: the redundant lane a host also fixes.
        assert!(is_host_redundant("teh", "the", lex));
        assert!(is_host_redundant("becuse", "because", lex));
        // Real-word source → never redundant (hard vocabulary rule), even to a
        // common target: the user may have meant the word they typed.
        assert!(!is_host_redundant("form", "from", lex));
        // Uncommon / idiosyncratic target → Jordan's territory, always fires.
        assert!(!is_host_redundant("incocnsistency", "inconsistency", lex));
        assert!(!is_host_redundant("reminiscience", "reminiscence", lex));
        // Learned-personal pattern (target absent from the freq table → 0).
        assert!(!is_host_redundant("kubernetis", "kubernetes", lex));
    }

    // A fully-qualified non-word-source suggestion: a single motor slip, both
    // ends multi-letter, enough weight, fresh, clear brake, typed not a word,
    // target a real word. Tests mutate one field to exercise one gate.
    fn ready_facts() -> PatternFacts {
        PatternFacts {
            weight: REALWORD_SOURCE_EVIDENCE_BAR,
            age_ms: 0,
            consecutive_undos: 0,
            typed_is_known: false,
            target_is_known: true,
            source_len: 3,
            target_len: 3,
            is_single_motor_error: true,
            key_affectedness: 0.0,
        }
    }

    // ---- happy paths: both tiers -------------------------------------------

    #[test]
    fn non_word_source_above_low_bar_suggests() {
        let f = PatternFacts {
            weight: NONWORD_SOURCE_EVIDENCE_BAR,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Suggest {
                tier: SuggestTier::NonWordSource
            }
        );
    }

    #[test]
    fn real_word_source_above_high_bar_suggests() {
        let f = PatternFacts {
            typed_is_known: true,
            weight: REALWORD_SOURCE_EVIDENCE_BAR,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Suggest {
                tier: SuggestTier::RealWordSource
            }
        );
    }

    #[test]
    fn no_silent_path_exists() {
        // The whole point of the rewrite: the strongest possible evidence still
        // only ever yields a *suggestion*, never a silent fix.
        let f = PatternFacts {
            weight: REALWORD_SOURCE_EVIDENCE_BAR * 100.0,
            ..ready_facts()
        };
        assert!(matches!(
            classify_pattern(&f),
            PatternReadiness::Suggest { .. }
        ));
    }

    // ---- risk-tiered bar ---------------------------------------------------

    #[test]
    fn non_word_source_between_bars_still_suggests() {
        // Weight clears the low bar but not the high one; a non-word source only
        // needs the low bar.
        let f = PatternFacts {
            typed_is_known: false,
            weight: NONWORD_SOURCE_EVIDENCE_BAR + 0.5,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Suggest {
                tier: SuggestTier::NonWordSource
            }
        );
    }

    #[test]
    fn real_word_source_between_bars_is_insufficient() {
        // Same weight, but a real-word source must clear the HIGH bar — the hard
        // vocabulary rule. Below it → observe.
        let f = PatternFacts {
            typed_is_known: true,
            weight: NONWORD_SOURCE_EVIDENCE_BAR + 0.5,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::InsufficientEvidence
            }
        );
    }

    #[test]
    fn non_word_source_below_low_bar_is_insufficient() {
        let f = PatternFacts {
            typed_is_known: false,
            weight: NONWORD_SOURCE_EVIDENCE_BAR - 0.5,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::InsufficientEvidence
            }
        );
    }

    // ---- structural disqualifiers ------------------------------------------

    #[test]
    fn non_motor_pair_observes_regardless_of_weight() {
        let f = PatternFacts {
            is_single_motor_error: false,
            weight: REALWORD_SOURCE_EVIDENCE_BAR * 10.0,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::NotAMotorSlip
            }
        );
    }

    #[test]
    fn single_letter_source_never_suggests() {
        let f = PatternFacts {
            source_len: 1,
            weight: REALWORD_SOURCE_EVIDENCE_BAR * 10.0,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::SingleLetterSource
            }
        );
    }

    #[test]
    fn single_letter_target_never_suggests() {
        let f = PatternFacts {
            target_len: 1,
            weight: REALWORD_SOURCE_EVIDENCE_BAR * 10.0,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::SingleLetterTarget
            }
        );
    }

    // ---- confidence gates --------------------------------------------------

    #[test]
    fn zero_weight_is_unseen() {
        let f = PatternFacts {
            weight: 0.0,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::Unseen
            }
        );
    }

    #[test]
    fn non_word_target_observes_even_with_high_weight() {
        let f = PatternFacts {
            weight: REALWORD_SOURCE_EVIDENCE_BAR * 10.0,
            target_is_known: false,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::TargetNotAWord
            }
        );
    }

    #[test]
    fn stale_pattern_observes() {
        let f = PatternFacts {
            age_ms: STALE_AFTER_MS as u64 + 1,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::Stale
            }
        );
    }

    // ---- 3-strike brake ----------------------------------------------------

    #[test]
    fn brake_tripped_suppresses_suggestion() {
        let f = PatternFacts {
            consecutive_undos: UNDO_BRAKE_STRIKES,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::BrakeTripped
            }
        );
    }

    #[test]
    fn brake_below_strikes_still_suggests() {
        let f = PatternFacts {
            consecutive_undos: UNDO_BRAKE_STRIKES - 1,
            ..ready_facts()
        };
        assert!(matches!(
            classify_pattern(&f),
            PatternReadiness::Suggest { .. }
        ));
    }

    // ---- gate precedence ---------------------------------------------------

    #[test]
    fn structural_gate_precedes_confidence_gate() {
        // Single-letter source AND insufficient weight: the structural reason
        // (never a candidate at any count) wins.
        let f = PatternFacts {
            source_len: 1,
            weight: 0.0,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Observe {
                reason: ObserveReason::SingleLetterSource
            }
        );
    }

    // ---- end-to-end via the store + real lexicon ---------------------------

    fn observe_n(store: &mut WordPatternStore, typed: &str, target: &str, n: u32, now: u64) {
        for _ in 0..n {
            store.observe_correction(Outcome::CorrectedToOther, typed, target, now);
        }
    }

    #[test]
    fn end_to_end_teh_to_the_suggests_at_low_bar() {
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        // "teh" is not a word; "the" is a real word — a transposition, so it
        // suggests once it clears the LOW bar (non-word source).
        observe_n(&mut store, "teh", "the", 4, 1_000_000);
        assert_eq!(
            classify("teh", "the", &store, lex, &MotorMap::new()),
            PatternReadiness::Suggest {
                tier: SuggestTier::NonWordSource
            }
        );
    }

    #[test]
    fn end_to_end_non_word_below_low_bar_is_insufficient() {
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        observe_n(&mut store, "teh", "the", 2, 1_000_000);
        assert_eq!(
            classify("teh", "the", &store, lex, &MotorMap::new()),
            PatternReadiness::Observe {
                reason: ObserveReason::InsufficientEvidence
            }
        );
    }

    #[test]
    fn end_to_end_real_word_source_needs_high_bar() {
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        // "form" and "from" are BOTH real words and a transposition. As a
        // real-word source it must clear the HIGH bar — 5 sightings isn't enough.
        assert!(lex.is_known("form"));
        observe_n(&mut store, "form", "from", 5, 1_000_000);
        assert_eq!(
            classify("form", "from", &store, lex, &MotorMap::new()),
            PatternReadiness::Observe {
                reason: ObserveReason::InsufficientEvidence
            }
        );
        // …but it DOES become a (cautious) suggestion with enough evidence —
        // there is no silent path even for a real-word source.
        observe_n(&mut store, "form", "from", 12, 1_000_000);
        assert_eq!(
            classify("form", "from", &store, lex, &MotorMap::new()),
            PatternReadiness::Suggest {
                tier: SuggestTier::RealWordSource
            }
        );
    }

    #[test]
    fn end_to_end_single_letter_source_never_suggests() {
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        // "s → is": a dropped-letter motor slip, but a single-letter source is
        // intent-ambiguous and must never be nominated, however much evidence.
        observe_n(&mut store, "s", "is", 50, 1_000_000);
        assert_eq!(
            classify("s", "is", &store, lex, &MotorMap::new()),
            PatternReadiness::Observe {
                reason: ObserveReason::SingleLetterSource
            }
        );
    }

    #[test]
    fn end_to_end_word_swap_is_not_a_motor_slip() {
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        // "so → for": a substitution PLUS an insertion — a word-swap, not a
        // single motor error. Never nominated.
        observe_n(&mut store, "so", "for", 30, 1_000_000);
        assert_eq!(
            classify("so", "for", &store, lex, &MotorMap::new()),
            PatternReadiness::Observe {
                reason: ObserveReason::NotAMotorSlip
            }
        );
    }

    #[test]
    fn end_to_end_apostrophe_fix_is_not_a_motor_slip() {
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        observe_n(&mut store, "dont", "don't", 30, 1_000_000);
        assert_eq!(
            classify("dont", "don't", &store, lex, &MotorMap::new()),
            PatternReadiness::Observe {
                reason: ObserveReason::NotAMotorSlip
            }
        );
    }

    #[test]
    fn end_to_end_unseen_pattern_observes() {
        let lex = Lexicon::shared();
        let store = WordPatternStore::new();
        // A valid motor slip never observed → Unseen (structural gates pass).
        assert_eq!(
            classify("teh", "the", &store, lex, &MotorMap::new()),
            PatternReadiness::Observe {
                reason: ObserveReason::Unseen
            }
        );
    }

    #[test]
    fn legit_single_letters_are_known_words() {
        // The brief: ensure `a` and `i` are known words (so they take the
        // cautious real-word path, never flagged as typos to correct away).
        let lex = Lexicon::shared();
        assert!(lex.is_known("a"));
        assert!(lex.is_known("i"));
    }

    // ---- Motor-Map-aware evidence bar --------------------------------------

    #[test]
    fn evidence_bar_eases_with_affectedness_but_floors_per_tier() {
        // Non-word source: base 4, eases to the AFFECTED_BAR_FLOOR (2) at the
        // strong-affectedness saturation point, graded in between.
        let (b0, base, _) = evidence_bar(false, 0.0);
        assert_eq!(
            (b0, base),
            (NONWORD_SOURCE_EVIDENCE_BAR, NONWORD_SOURCE_EVIDENCE_BAR)
        );
        let (bfull, _, _) = evidence_bar(false, STRONG_AFFECTEDNESS);
        assert!((bfull - AFFECTED_BAR_FLOOR).abs() < 1e-6, "strong → floor");
        let (bhalf, _, _) = evidence_bar(false, STRONG_AFFECTEDNESS / 2.0);
        assert!(
            (bhalf - (NONWORD_SOURCE_EVIDENCE_BAR + AFFECTED_BAR_FLOOR) / 2.0).abs() < 1e-6,
            "graded midpoint"
        );
        // Affectedness beyond the saturation point doesn't push below the floor.
        let (bover, _, _) = evidence_bar(false, 1.0);
        assert!((bover - AFFECTED_BAR_FLOOR).abs() < 1e-6);

        // Real-word source stays cautious: even fully eased it never drops below
        // the NON-WORD base bar (the hard vocabulary rule holds).
        let (rwfull, rwbase, tier) = evidence_bar(true, 1.0);
        assert_eq!(rwbase, REALWORD_SOURCE_EVIDENCE_BAR);
        assert_eq!(tier, SuggestTier::RealWordSource);
        assert!((rwfull - NONWORD_SOURCE_EVIDENCE_BAR).abs() < 1e-6);
    }

    // ---- shadow curve: length-scaled bar + budget --------------------------

    #[test]
    fn shadow_bar_scales_with_source_length() {
        assert_eq!(shadow_nonword_bar_for_len(2), 4.0);
        assert_eq!(shadow_nonword_bar_for_len(3), 4.0);
        assert_eq!(shadow_nonword_bar_for_len(4), 3.0);
        assert_eq!(shadow_nonword_bar_for_len(6), 3.0);
        assert_eq!(shadow_nonword_bar_for_len(7), 1.0);
        assert_eq!(shadow_nonword_bar_for_len(12), 1.0);
    }

    #[test]
    fn shadow_motor_budget_scales_with_source_length() {
        assert_eq!(shadow_motor_budget_for_len(2), 1);
        assert_eq!(shadow_motor_budget_for_len(3), 1);
        assert_eq!(shadow_motor_budget_for_len(4), 2);
        assert_eq!(shadow_motor_budget_for_len(9), 2);
    }

    #[test]
    fn scaled_bar_uses_length_base_and_real_word_is_unchanged() {
        // Non-word base follows the length curve; with no affectedness the bar
        // IS the base.
        for (len, base) in [(3usize, 4.0f32), (5, 3.0), (8, 1.0)] {
            let (used, b, tier) = evidence_bar_scaled(false, 0.0, len);
            assert_eq!((b, tier), (base, SuggestTier::NonWordSource));
            assert!((used - base).abs() < 1e-6);
        }
        // Real-word source: identical to the un-scaled bar at every length —
        // high base 12, floor the non-word base 4. The cautious rule is untouched.
        for len in [2usize, 5, 9] {
            let (used, base, tier) = evidence_bar_scaled(true, 1.0, len);
            assert_eq!((base, tier), (REALWORD_SOURCE_EVIDENCE_BAR, SuggestTier::RealWordSource));
            assert!((used - NONWORD_SOURCE_EVIDENCE_BAR).abs() < 1e-6);
        }
    }

    #[test]
    fn scaled_long_word_bar_one_has_no_easing_room() {
        // A length-7+ non-word base is 1, already below AFFECTED_BAR_FLOOR (2);
        // easing must not RAISE it — it stays pinned at 1 even at full affectedness.
        let (used_lo, base, _) = evidence_bar_scaled(false, 0.0, 9);
        let (used_hi, _, _) = evidence_bar_scaled(false, 1.0, 9);
        assert_eq!(base, 1.0);
        assert!((used_lo - 1.0).abs() < 1e-6);
        assert!((used_hi - 1.0).abs() < 1e-6);
    }

    #[test]
    fn scaled_long_nonword_suggests_at_one_sighting() {
        // A 7-letter non-word motor slip with a single decayed sighting (~1.0)
        // clears the length-7+ bar of 1 in the shadow path, where the flat-4 bar
        // would still hold it as InsufficientEvidence.
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        // "progess -> progress": one dropped letter, non-word source, real target.
        observe_n(&mut store, "progess", "progress", 1, 1_000_000);
        // Flat classifier: below the flat bar of 4.
        assert_eq!(
            classify("progess", "progress", &store, lex, &MotorMap::new()),
            PatternReadiness::Observe {
                reason: ObserveReason::InsufficientEvidence
            }
        );
        // Shadow curve: length-7+ bar is 1 → suggests.
        let ex = classify_explained_scaled("progess", "progress", &store, lex, &MotorMap::new());
        assert_eq!(
            ex.readiness,
            PatternReadiness::Suggest {
                tier: SuggestTier::NonWordSource
            }
        );
    }

    #[test]
    fn scaled_real_word_long_source_still_held_to_high_bar() {
        // Length scaling must NOT lower the real-word bar: a real-word 7+ source
        // with modest evidence stays InsufficientEvidence in the shadow path too.
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        assert!(lex.is_known("records"));
        observe_n(&mut store, "records", "record", 3, 1_000_000);
        let ex = classify_explained_scaled("records", "record", &store, lex, &MotorMap::new());
        assert_eq!(
            ex.readiness,
            PatternReadiness::Observe {
                reason: ObserveReason::InsufficientEvidence
            }
        );
        assert!((ex.base_bar - REALWORD_SOURCE_EVIDENCE_BAR).abs() < 1e-6);
    }

    #[test]
    fn scaled_two_error_long_word_admitted_by_budget() {
        // A length-8 non-word slip with two adjacent-key substitutions (l→k and
        // n→m, distance 2, same length — within the store's ≤2/≤1 capture guard so
        // it actually records) is rejected by the budget-1 live classifier
        // (NotAMotorSlip — two diffs, not a transposition) but admitted by the
        // budget-2 shadow gate.
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        let typed = "buikdimg"; // "building" with l→k and n→m
        let target = "building";
        // Confirm the shape: budget 1 rejects, budget 2 admits at distance 2 (both
        // subs are adjacent, so the restricted metric counts them).
        assert!(crate::slip_class::single_motor_edit(typed, target).is_none());
        assert_eq!(
            crate::slip_class::motor_edit_within_budget(typed, target, 2),
            Some(2)
        );
        observe_n(&mut store, typed, target, 1, 1_000_000);
        // Live (budget-1) classifier: not a single motor slip.
        assert_eq!(
            classify(typed, target, &store, lex, &MotorMap::new()),
            PatternReadiness::Observe {
                reason: ObserveReason::NotAMotorSlip
            }
        );
        // Shadow (budget-2) classifier: structurally admitted; length-8 bar is 1,
        // one sighting clears it → suggests.
        let ex = classify_explained_scaled(typed, target, &store, lex, &MotorMap::new());
        assert_eq!(
            ex.readiness,
            PatternReadiness::Suggest {
                tier: SuggestTier::NonWordSource
            }
        );
    }

    // ---- shadow convergence model (dictionary-driven) ----------------------

    #[test]
    fn convergence_neighborhood_matches_fullscan_parity() {
        // HARD GATE for Lever 1: the fast neighbourhood generator must return the
        // BYTE-IDENTICAL candidate set as the exhaustive dictionary walk, for
        // every input. If this ever fails, the optimisation is wrong and must not
        // ship. Both produce sorted, de-duplicated vecs, so `==` is exact.
        let lex = Lexicon::load();

        // 1) Hand-picked inputs across every motor shape + the live repro words.
        let mut inputs: Vec<String> = [
            "teh",
            "antiicipated",
            "anticiipated",
            "whcih",
            "ther",
            "form",
            "wprd",
            "worrd",
            "wrd",
            "recieve",
            "competant",
            "ofthose",
            "a",
            "i",
            "",
            "xqzj",
            "helllo",
            "thsi",
            "becuase",
            "the",
            "word",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        // 2) Genuine near-miss → dictionary cases: sample real words and apply one
        //    motor edit each, exercising the exact situation the scan must nail.
        for (i, w) in lex.iter_clean().enumerate() {
            if i % 4000 != 0 {
                // ~20 words → ~40 mutated inputs. The hand-picked list above
                // already covers every edit shape; this just adds real-word
                // breadth. The oracle is O(lexicon) per input, so keep the count
                // low (this gate ran exhaustively at i%500 once and matched).
                continue;
            }
            let cs: Vec<char> = w.chars().collect();
            if cs.len() >= 4 {
                let mut m = cs.clone();
                m.remove(cs.len() / 2); // a deletion in the middle
                inputs.push(m.into_iter().collect());
                let mut m = cs.clone();
                m.swap(0, 1); // a front transposition
                inputs.push(m.into_iter().collect());
            }
        }

        for typed in &inputs {
            let typed_n = crate::word_pattern::normalize_word(typed);
            let full = convergence_candidates_fullscan(&typed_n, &lex, SHADOW_BOLD_MOTOR_BUDGET);
            let nbr =
                convergence_candidates_neighborhood(&typed_n, &lex, SHADOW_BOLD_MOTOR_BUDGET);
            assert_eq!(
                full, nbr,
                "candidate-set mismatch for typed={typed:?} (normalized={typed_n:?})"
            );
        }
    }

    #[test]
    fn convergence_fires_on_unique_candidate_non_word() {
        // "whcih" is a non-word; "which" is one adjacent transposition away — a
        // SINGLE motor edit, so it converges and fires on first sighting (no
        // store, no evidence). The bold path is capped at one edit at every length.
        let lex = Lexicon::shared();
        let scan = shadow_convergence_scan("whcih", lex, &MotorMap::new());
        assert!(scan.typed_is_non_word);
        assert_eq!(scan.candidates, vec!["which".to_string()]);
        assert!(scan.convergent);
        assert_eq!(scan.budget, SHADOW_BOLD_MOTOR_BUDGET);
        assert_eq!(scan.budget, 1);
    }

    #[test]
    fn convergence_does_not_fire_on_two_edit_pairs() {
        // These converged under the old budget-2 bold path but require TWO motor
        // edits (a non-adjacent a→e substitution = delete+insert, or a length-2
        // truncation), so under the single-edit cap they no longer converge and
        // fall through to the learned/length-band path.
        let lex = Lexicon::shared();
        for (typed, was_target) in [
            ("competant", "competent"), // a→e: non-adjacent sub = 2 edits
            ("recurrance", "recurrence"),
            ("ofthose", "those"), // length diff 2
            ("availbo", "avail"),
        ] {
            let scan = shadow_convergence_scan(typed, lex, &MotorMap::new());
            assert!(scan.typed_is_non_word, "{typed} should be a non-word");
            assert!(
                !scan.candidates.iter().any(|c| c == was_target),
                "{typed}: {was_target} is two edits away — must not be a single-edit candidate"
            );
            // Whatever single-edit candidates exist, this token is no longer a
            // first-sighting bold win for the old two-edit target.
            assert!(
                !(scan.convergent && scan.candidates.first().map(String::as_str) == Some(was_target)),
                "{typed} must no longer bold-converge on {was_target}"
            );
        }
    }

    #[test]
    fn convergence_holds_when_two_candidates() {
        // A non-word that sits one motor edit from MORE than one real word is
        // ambiguous — not bold, falls back to the learned/length-band path.
        let lex = Lexicon::shared();
        let scan = shadow_convergence_scan("ther", lex, &MotorMap::new());
        assert!(scan.typed_is_non_word);
        assert!(
            scan.candidates.len() >= 2,
            "expected ambiguity, got {:?}",
            scan.candidates
        );
        assert!(!scan.convergent);
    }

    #[test]
    fn convergence_skips_a_real_word_source() {
        // "form" is a real dictionary word — never a correction candidate, so no
        // candidates are generated and it is never bold (step 1 / non-word gate).
        let lex = Lexicon::shared();
        let scan = shadow_convergence_scan("form", lex, &MotorMap::new());
        assert!(!scan.typed_is_non_word);
        assert!(scan.candidates.is_empty());
        assert!(!scan.convergent);
    }

    #[test]
    fn convergence_skips_a_users_learned_word() {
        // A word the user uses (learned vocab) is known → not a non-word → never
        // a bold candidate, even if it isn't in the base dictionary.
        let lex = Lexicon::shared();
        lex.clear_learned();
        let coined = "zlorptak"; // certainly not in the base dictionary
        assert!(!lex.is_known(coined));
        lex.learn(coined);
        let scan = shadow_convergence_scan(coined, lex, &MotorMap::new());
        assert!(!scan.typed_is_non_word, "learned word is known, not a non-word");
        assert!(!scan.convergent);
        lex.clear_learned();
    }

    #[test]
    fn convergence_budget_one_for_short_words() {
        // Length 2–3 uses budget 1, so a short non-word only reaches dictionary
        // words within a single motor edit.
        let lex = Lexicon::shared();
        let scan = shadow_convergence_scan("teh", lex, &MotorMap::new());
        assert_eq!(scan.budget, 1);
        assert!(scan.typed_is_non_word);
        // "the" (transposition) is among the candidates.
        assert!(scan.candidates.iter().any(|c| c == "the"));
    }

    #[test]
    fn affected_key_surfaces_a_slip_below_the_base_bar() {
        // A non-word slip with weight 2.5 is below the base bar (4) → normally
        // InsufficientEvidence. A strongly-affected key eases the bar to 2 →
        // it surfaces, and is flagged surfaced_early.
        let weak = PatternFacts {
            weight: 2.5,
            key_affectedness: 0.0,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&weak),
            PatternReadiness::Observe {
                reason: ObserveReason::InsufficientEvidence
            }
        );

        let affected = PatternFacts {
            weight: 2.5,
            key_affectedness: STRONG_AFFECTEDNESS, // full bonus → bar 2
            ..ready_facts()
        };
        let ex = classify_pattern_explained(&affected);
        assert_eq!(
            ex.readiness,
            PatternReadiness::Suggest {
                tier: SuggestTier::NonWordSource
            }
        );
        assert!(ex.eased_by_motor);
        assert!(
            ex.surfaced_early,
            "weight 2.5 < base 4 → only surfaced via the bonus"
        );
        assert!((ex.bar_used - AFFECTED_BAR_FLOOR).abs() < 1e-6);
    }

    #[test]
    fn affectedness_never_crosses_a_structural_gate() {
        // Max affectedness must NOT rescue a single-letter source, a non-motor
        // pair, or a non-word target — easing only lowers repetition, never
        // relaxes safety.
        for f in [
            PatternFacts {
                source_len: 1,
                key_affectedness: 1.0,
                weight: 100.0,
                ..ready_facts()
            },
            PatternFacts {
                is_single_motor_error: false,
                key_affectedness: 1.0,
                weight: 100.0,
                ..ready_facts()
            },
            PatternFacts {
                target_is_known: false,
                key_affectedness: 1.0,
                weight: 100.0,
                ..ready_facts()
            },
        ] {
            assert!(
                matches!(classify_pattern(&f), PatternReadiness::Observe { .. }),
                "affectedness must not cross a structural gate"
            );
        }
    }

    #[test]
    fn high_evidence_suggestion_is_not_flagged_early() {
        // A slip that clears the base bar on repetition alone is a normal
        // suggestion — not "early", even if the key is affected.
        let f = PatternFacts {
            weight: NONWORD_SOURCE_EVIDENCE_BAR + 1.0,
            key_affectedness: STRONG_AFFECTEDNESS,
            ..ready_facts()
        };
        let ex = classify_pattern_explained(&f);
        assert!(matches!(ex.readiness, PatternReadiness::Suggest { .. }));
        assert!(
            !ex.surfaced_early,
            "weight ≥ base bar → would have surfaced anyway"
        );
    }

    #[test]
    fn end_to_end_affected_key_eases_real_store() {
        // "teh → the" with only 3 sightings is below the non-word base bar (4) →
        // InsufficientEvidence with an empty motor map…
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        observe_n(&mut store, "teh", "the", 3, 1_000_000);
        assert_eq!(
            classify("teh", "the", &store, lex, &MotorMap::new()),
            PatternReadiness::Observe {
                reason: ObserveReason::InsufficientEvidence
            }
        );
        // …but if the motor map shows 'h'/'e' (the transposed keys) strongly
        // affected, the eased bar lets the same 3 sightings surface.
        let mut mm = MotorMap::new();
        for _ in 0..20 {
            mm.observe_outcome(Outcome::Kept, "h", None, 1_000_000);
        }
        for _ in 0..20 {
            mm.observe_outcome(Outcome::CorrectedToOther, "g", Some("h"), 1_000_000);
        }
        // 'h': 20 correct + 20 slips → affectedness 0.5 ≥ STRONG → full ease.
        assert!(mm.affectedness('h') >= STRONG_AFFECTEDNESS);
        assert_eq!(
            classify("teh", "the", &store, lex, &mm),
            PatternReadiness::Suggest {
                tier: SuggestTier::NonWordSource
            }
        );
    }
}
