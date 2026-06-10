//! Component 5e — **Per-pattern kill-switch** (M3 Phase 1, module 2).
//!
//! The word-pattern store ([`crate::word_pattern`]) *learns* `typed → target`
//! corrections. This module *decides* what the engine would be allowed to do
//! with one — the per-pattern kill-switch from the M3 brief (CLAUDE.md → M3:
//! "correction turns on when the map is confident about *this* person's
//! patterns" — **per-pattern, not a global threshold**).
//!
//! ## The two tiers it gates
//!
//! * **Tier-1** — the silent fix (auto-correct + a dot; user backspaces to
//!   undo). The high bar: we only silently rewrite when *all four* gates hold.
//! * **Tier-2** — flag only (squiggle + a suggestion box on arrow-to). The
//!   fallback when there's real confidence in the pattern but a Tier-1 gate
//!   fails — most importantly **when the typo is itself a real word** (the
//!   single most important safety rule: never silently rewrite a word the
//!   user may have meant, but a *visible, dismissable* suggestion is fine).
//! * **Silent** — do nothing, with a reason. Not enough evidence yet, the
//!   target isn't a real word, or the pattern has gone stale.
//!
//! ## The four Tier-1 gates (brief decision 1)
//!
//! A pattern is Tier-1-ready iff ALL hold:
//!   a. **Enough recent evidence** — decayed weight ≥
//!      [`TIER1_MIN_OBSERVATIONS`].
//!   b. **Typed is NOT a real word** — else a silent rewrite could destroy
//!      something the user meant (→ Tier-2 instead).
//!   c. **Target IS a real, common word** — known to the lexicon AND frequent
//!      enough ([`COMMON_TARGET_MIN_FREQUENCY`]); we don't silently steer
//!      toward a non-word or a rarity.
//!   d. **Recent** — observed within [`STALE_AFTER_MS`] (one 30-day
//!      half-life); a pattern the hand has grown out of shouldn't act.
//!
//! On top of the gates, the **3-strike safety brake**: if the user has undone
//! this pattern's Tier-1 fix [`UNDO_BRAKE_STRIKES`] times in a row
//! (`consecutive_undos`), it is demoted Tier-1 → Tier-2 until it rebuilds —
//! even when all four gates pass.
//!
//! ## Shape, mirroring [`crate::decision`]
//!
//! The core is a **pure function** over injected [`PatternFacts`] — no
//! lexicon, no store, no I/O — so every gate is unit-testable in isolation
//! (exactly how [`crate::decision::decide`] takes a pre-computed `is_known`
//! rather than the [`Lexicon`]). [`classify`] is the thin convenience wrapper
//! that assembles the facts from a [`WordPatternStore`] + [`Lexicon`]. Every
//! "not Tier-1" outcome carries a reason, so the FEED/debug surface never has
//! to guess why the engine stayed quiet or chose a squiggle.
//!
//! ## Still OFF
//!
//! Phase 1 is observe-and-classify only. Nothing reads a [`PatternReadiness`]
//! into an actual injection yet — that wiring (and the undo/accept feedback
//! that drives the brake) is Phase 2. This module makes the *decision*
//! inspectable so we can judge correction quality against real data before a
//! single character is ever rewritten.

use serde::{Deserialize, Serialize};

use crate::lexicon::Lexicon;
use crate::motor_map::HALF_LIFE_MS;
use crate::word_pattern::{WordPatternStore, TIER1_MIN_OBSERVATIONS, UNDO_BRAKE_STRIKES};

/// Bump on any change to the classification policy or the serialized
/// [`PatternReadiness`] shape.
pub const KILL_SWITCH_VERSION: u32 = 1;

/// **EASILY-FLIPPED CONSTANT.** Minimum lexicon frequency (Norvig unigram
/// count) for a target to count as "common" enough to silently steer toward
/// (Tier-1 gate c). Below it, a confident pattern is demoted to Tier-2.
///
/// Calibrated against the bundled corpus: the frequency table is the top-50k
/// words, its floor is ~333k, and seed-only known words score 1. Every
/// realistic target seen in real sessions sits well above 1M (`slack` 1.95M,
/// `gmail` 3.2M, `hopefully` 10.3M, `receipt` 14.5M, `the` 23B), while 1M
/// holds the rarer ~half of the corpus at Tier-2. Conservative starting point;
/// tune from real `word_patterns.json` data.
pub const COMMON_TARGET_MIN_FREQUENCY: u64 = 1_000_000;

/// Recency window for Tier-1 gate (d): a pattern not observed within this many
/// ms is "stale" — the hand may have grown out of it — and won't act until
/// refreshed. One 30-day half-life, shared with the motor map's decay so the
/// two layers forget on the same clock.
pub const STALE_AFTER_MS: f32 = HALF_LIFE_MS;

/// What the engine would be allowed to do for one learned pattern. Observe-only
/// in Phase 1 — a *classification*, never an injection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PatternReadiness {
    /// All four gates hold and the brake is clear — eligible for the silent
    /// Tier-1 fix.
    Tier1Ready,
    /// Real confidence exists, but a Tier-1 gate failed (or the brake tripped)
    /// — show a Tier-2 suggestion instead of silently rewriting.
    Tier2Only { reason: Tier2Reason },
    /// Do nothing, with a reason — there isn't confidence to act on yet.
    Silent { reason: SilentReason },
}

/// Why a confident pattern is held at Tier-2 rather than Tier-1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier2Reason {
    /// **The single most important safety rule.** The typed word is itself a
    /// real word — never silently rewrite it; a dismissable suggestion is OK.
    TypedIsRealWord,
    /// The target is a real word but below [`COMMON_TARGET_MIN_FREQUENCY`] —
    /// confident enough to suggest, not to silently steer toward.
    TargetNotCommon,
    /// The user has undone this fix [`UNDO_BRAKE_STRIKES`] times in a row —
    /// demoted until it rebuilds (the 3-strike brake).
    BrakeTripped,
}

/// Why a pattern doesn't act at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SilentReason {
    /// No such pattern has ever been observed.
    Unseen,
    /// Decayed weight is below [`TIER1_MIN_OBSERVATIONS`] — not enough recent
    /// evidence to be confident yet.
    InsufficientObservations,
    /// Last observed longer ago than [`STALE_AFTER_MS`] — the hand may have
    /// moved on; wait for it to recur before acting.
    Stale,
    /// The target isn't a real word — never a valid correction destination
    /// (learning the word is the lexicon proposer's job, not ours).
    TargetNotAWord,
}

/// The pre-computed facts the policy decides on — injected so the decision is
/// a pure function (mirrors how [`crate::decision::decide`] takes `is_known`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PatternFacts {
    /// Decayed occurrence weight of this `typed → target` pattern.
    pub weight: f32,
    /// Time since the pattern was last observed (ms), for the recency gate.
    pub age_ms: u64,
    /// Consecutive user-undos of this pattern's Tier-1 fix (the brake).
    pub consecutive_undos: u32,
    /// Is the typed word itself a real word? (Tier-1 gate b.)
    pub typed_is_known: bool,
    /// Is the target a real word at all? (Tier-1 gate c, membership half.)
    pub target_is_known: bool,
    /// The target's lexicon frequency (Tier-1 gate c, commonness half).
    pub target_frequency: u64,
}

/// Classify one pattern from pre-computed facts. **Pure** — no lexicon, no
/// store, no I/O.
///
/// Order: the "no confidence yet" gates first (→ [`PatternReadiness::Silent`]),
/// then — once confidence is established — the gates whose failure still leaves
/// a visible suggestion (→ [`PatternReadiness::Tier2Only`]), else
/// [`PatternReadiness::Tier1Ready`].
pub fn classify_pattern(f: &PatternFacts) -> PatternReadiness {
    // --- Confidence gates: failing any means "nothing to act on yet". ---
    if f.weight <= 0.0 {
        return PatternReadiness::Silent {
            reason: SilentReason::Unseen,
        };
    }
    // A non-word target can never be a valid silent OR suggested destination,
    // so it disqualifies regardless of how often it's been seen.
    if !f.target_is_known {
        return PatternReadiness::Silent {
            reason: SilentReason::TargetNotAWord,
        };
    }
    if f.weight < TIER1_MIN_OBSERVATIONS {
        return PatternReadiness::Silent {
            reason: SilentReason::InsufficientObservations,
        };
    }
    // Compare in f64: a 30-day window is ~2.6e9 ms, past f32's integer
    // precision (~256ms ULP), so an f32 compare would quantise the boundary.
    if f.age_ms as f64 > f64::from(STALE_AFTER_MS) {
        return PatternReadiness::Silent {
            reason: SilentReason::Stale,
        };
    }

    // --- Confidence established. A failing Tier-1 gate now demotes to a
    //     visible Tier-2 suggestion rather than silence. ---

    // The single most important safety rule, checked first: never silently
    // rewrite a real word.
    if f.typed_is_known {
        return PatternReadiness::Tier2Only {
            reason: Tier2Reason::TypedIsRealWord,
        };
    }
    if f.target_frequency < COMMON_TARGET_MIN_FREQUENCY {
        return PatternReadiness::Tier2Only {
            reason: Tier2Reason::TargetNotCommon,
        };
    }
    // Operational override: even a fully-qualified pattern is demoted while the
    // user is actively rejecting it.
    if f.consecutive_undos >= UNDO_BRAKE_STRIKES {
        return PatternReadiness::Tier2Only {
            reason: Tier2Reason::BrakeTripped,
        };
    }

    PatternReadiness::Tier1Ready
}

/// Convenience wrapper: assemble [`PatternFacts`] for `typed → target` from a
/// [`WordPatternStore`] (weight / recency / brake) and a [`Lexicon`]
/// (membership / frequency), then [`classify_pattern`]. Recency is measured
/// against the store's `last_now`, consistent with how the store decays reads.
pub fn classify(
    typed: &str,
    target: &str,
    store: &WordPatternStore,
    lexicon: &Lexicon,
) -> PatternReadiness {
    let Some(snap) = store.snapshot(typed, target) else {
        return PatternReadiness::Silent {
            reason: SilentReason::Unseen,
        };
    };
    let facts = PatternFacts {
        weight: snap.weight,
        age_ms: store.last_now().saturating_sub(snap.last_update),
        consecutive_undos: snap.consecutive_undos,
        typed_is_known: lexicon.is_known(typed),
        target_is_known: lexicon.is_known(target),
        target_frequency: lexicon.frequency(target),
    };
    classify_pattern(&facts)
}

// ============================================================================
// M3 correction Step 2 — common-vs-personal auto-fire classifier.
// ============================================================================
//
// A SEPARATE decision from [`classify_pattern`] above. That one is the parked
// automatic-readiness model (silent Tier-1 vs flag Tier-2), where a *common*
// target is a REQUIREMENT to act. Step 2 asks a different question:
//
//   Should TypeAssist OWN this correction, or DEFER it to macOS autocorrect?
//
// The product's differentiated value is the user's PERSONAL long tail — slips
// macOS does not fix. macOS handles standard typos of common dictionary words;
// it misses (a) typos of rarer words it lacks, and (b) severe jumbles it can't
// decode (even toward a common word — the "autocorrect on but misses" case).
//
// Crucially we are BLIND to macOS autocorrect: our tap never sees its
// substitutions (it rewrites via the text-input layer, not posted CGEvents), so
// we cannot detect or coordinate with it at runtime. Acting on a typo macOS
// also fixes therefore risks an unobservable collision. So "defer on common" is
// a SAFETY stance, decided purely by inference (lexicon frequency + jumble
// severity), never by watching macOS.
//
// Observe-only / dry-run: nothing here injects. The manual allow-list (Step 1)
// remains the user's override for anything classified Defer.

/// **EASILY-FLIPPED CONSTANT (provisional).** Target lexicon frequency at/above
/// which we assume macOS autocorrect reliably handles standard typos of the
/// word, so TypeAssist defers rather than risk an (unobservable) collision.
/// Below it, the target is rare enough that macOS likely lacks it → the user's
/// personal value → act. Starts equal to [`COMMON_TARGET_MIN_FREQUENCY`]; tune
/// by watching the Step-2 dry-run against real typing.
pub const COMMON_DEFER_FREQUENCY: u64 = COMMON_TARGET_MIN_FREQUENCY;

/// **EASILY-FLIPPED CONSTANT (provisional).** Largest edit distance macOS
/// autocorrect can plausibly *decode* as a typo of a known word. At/below this,
/// a common-word typo is "standard" → macOS handles it → defer. ABOVE this, the
/// jumble is severe enough that macOS can't map it back — so TypeAssist acts
/// even toward a common target (the "autocorrect on but misses" case). 2 mirrors
/// the original capture guard, so the dist≤2 common-word typos macOS already
/// fixes are exactly the ones we defer, while the dist-3+ jumbles the widened
/// capture now learns become ours. Tune from the dry-run.
pub const MACOS_DECODABLE_MAX_DISTANCE: usize = 2;

/// Step-2 decision: should TypeAssist own this correction or defer it to macOS?
/// Observe-only — a classification, never an injection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutoFireClass {
    /// The user's personal slip, where macOS autocorrect is unlikely to help:
    /// the target is rarer than [`COMMON_DEFER_FREQUENCY`] (macOS likely lacks
    /// it) OR the jumble exceeds [`MACOS_DECODABLE_MAX_DISTANCE`] (macOS can't
    /// decode it). All the act-worthiness gates have already passed.
    Personal,
    /// A common dictionary word with a small, standard typo — macOS autocorrect
    /// already handles it, so we defer to avoid an unobservable collision. The
    /// manual allow-list can still override.
    DeferToAutocorrect,
    /// Not act-worthy yet, with a reason (a gate failed). Reuses the same gate
    /// vocabulary as [`PatternReadiness`].
    NotActionable { reason: NotActionableReason },
}

/// Why an auto-fire candidate isn't act-worthy. Ordered the same way the
/// classifier checks them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotActionableReason {
    /// No such pattern has ever been observed.
    Unseen,
    /// Target isn't a real word — never a valid correction destination.
    TargetNotAWord,
    /// Decayed weight below [`TIER1_MIN_OBSERVATIONS`] — not confident yet.
    InsufficientObservations,
    /// Last observed longer ago than [`STALE_AFTER_MS`].
    Stale,
    /// **The safety rule.** The typed word is itself a real word — never auto-
    /// rewrite it (the manual allow-list could, with the user's explicit opt-in).
    TypedIsRealWord,
    /// Undone [`UNDO_BRAKE_STRIKES`] times in a row — held off until it rebuilds.
    BrakeTripped,
}

/// Pre-computed facts for the auto-fire decision — injected so the policy is a
/// pure function (mirrors [`PatternFacts`]). Adds the jumble-severity inputs
/// the common-vs-personal split needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoFireFacts {
    /// Decayed occurrence weight of the pattern.
    pub weight: f32,
    /// Time since last observed (ms), for the recency gate.
    pub age_ms: u64,
    /// Consecutive user-undos (the brake).
    pub consecutive_undos: u32,
    /// Is the typed word itself a real word? (Safety gate.)
    pub typed_is_known: bool,
    /// Is the target a real word at all?
    pub target_is_known: bool,
    /// The target's lexicon frequency (commonness — the defer signal).
    pub target_frequency: u64,
    /// Edit distance `typed → target` (jumble severity — the "macOS can't
    /// decode it" signal that overrides defer for a common target).
    pub edit_distance: usize,
}

/// Classify one auto-fire candidate from pre-computed facts. **Pure** — no
/// lexicon, no store, no I/O.
///
/// Order: the act-worthiness gates first (→ [`AutoFireClass::NotActionable`]),
/// then the common-vs-personal split.
pub fn classify_auto_fire(f: &AutoFireFacts) -> AutoFireClass {
    use AutoFireClass::NotActionable;
    // --- Act-worthiness gates (same vocabulary as the Tier model). ---
    if f.weight <= 0.0 {
        return NotActionable {
            reason: NotActionableReason::Unseen,
        };
    }
    if !f.target_is_known {
        return NotActionable {
            reason: NotActionableReason::TargetNotAWord,
        };
    }
    if f.weight < TIER1_MIN_OBSERVATIONS {
        return NotActionable {
            reason: NotActionableReason::InsufficientObservations,
        };
    }
    if f.age_ms as f64 > f64::from(STALE_AFTER_MS) {
        return NotActionable {
            reason: NotActionableReason::Stale,
        };
    }
    // Safety: never auto-rewrite a real word (the allow-list can, opt-in).
    if f.typed_is_known {
        return NotActionable {
            reason: NotActionableReason::TypedIsRealWord,
        };
    }
    if f.consecutive_undos >= UNDO_BRAKE_STRIKES {
        return NotActionable {
            reason: NotActionableReason::BrakeTripped,
        };
    }

    // --- Act-worthy. Defer to macOS only for a common word with a standard
    //     (decodable) typo; otherwise it's the user's personal value. ---
    let target_common = f.target_frequency >= COMMON_DEFER_FREQUENCY;
    let severe_jumble = f.edit_distance > MACOS_DECODABLE_MAX_DISTANCE;
    if target_common && !severe_jumble {
        AutoFireClass::DeferToAutocorrect
    } else {
        AutoFireClass::Personal
    }
}

/// Convenience wrapper: assemble [`AutoFireFacts`] for `typed → target` from a
/// [`WordPatternStore`] + [`Lexicon`] (and the edit distance between them), then
/// [`classify_auto_fire`].
pub fn classify_auto_fire_for(
    typed: &str,
    target: &str,
    store: &WordPatternStore,
    lexicon: &Lexicon,
) -> AutoFireClass {
    let Some(snap) = store.snapshot(typed, target) else {
        return AutoFireClass::NotActionable {
            reason: NotActionableReason::Unseen,
        };
    };
    let facts = AutoFireFacts {
        weight: snap.weight,
        age_ms: store.last_now().saturating_sub(snap.last_update),
        consecutive_undos: snap.consecutive_undos,
        typed_is_known: lexicon.is_known(typed),
        target_is_known: lexicon.is_known(target),
        target_frequency: lexicon.frequency(target),
        edit_distance: crate::word_pattern::edit_distance(typed, target),
    };
    classify_auto_fire(&facts)
}

// Sanity at compile time.
const _: () = assert!(KILL_SWITCH_VERSION >= 1);

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Outcome;

    // A fully-qualified Tier-1 pattern: enough weight, fresh, clear brake,
    // typed not a word, target a common real word. Tests mutate one field to
    // exercise one gate at a time.
    fn ready_facts() -> PatternFacts {
        PatternFacts {
            weight: TIER1_MIN_OBSERVATIONS,
            age_ms: 0,
            consecutive_undos: 0,
            typed_is_known: false,
            target_is_known: true,
            target_frequency: COMMON_TARGET_MIN_FREQUENCY,
        }
    }

    // ---- pure classifier: the happy path -------------------------------

    #[test]
    fn all_gates_pass_is_tier1_ready() {
        assert_eq!(
            classify_pattern(&ready_facts()),
            PatternReadiness::Tier1Ready
        );
    }

    #[test]
    fn exactly_at_thresholds_is_tier1_ready() {
        // weight == floor and frequency == floor both qualify (>=, not >).
        let f = ready_facts();
        assert_eq!(classify_pattern(&f), PatternReadiness::Tier1Ready);
    }

    // ---- Silent reasons (no confidence yet) ----------------------------

    #[test]
    fn zero_weight_is_unseen() {
        let f = PatternFacts {
            weight: 0.0,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Silent {
                reason: SilentReason::Unseen
            }
        );
    }

    #[test]
    fn below_threshold_weight_is_insufficient() {
        let f = PatternFacts {
            weight: TIER1_MIN_OBSERVATIONS - 0.1,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Silent {
                reason: SilentReason::InsufficientObservations
            }
        );
    }

    #[test]
    fn stale_pattern_is_silent() {
        let f = PatternFacts {
            age_ms: STALE_AFTER_MS as u64 + 1,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Silent {
                reason: SilentReason::Stale
            }
        );
    }

    #[test]
    fn non_word_target_is_silent_even_with_high_weight() {
        let f = PatternFacts {
            weight: TIER1_MIN_OBSERVATIONS * 10.0,
            target_is_known: false,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Silent {
                reason: SilentReason::TargetNotAWord
            }
        );
    }

    // ---- Tier-2 reasons (confidence, but a Tier-1 gate fails) ----------

    #[test]
    fn typed_is_real_word_demotes_to_tier2() {
        // THE safety rule: confident pattern, but the typo is a real word.
        let f = PatternFacts {
            typed_is_known: true,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Tier2Only {
                reason: Tier2Reason::TypedIsRealWord
            }
        );
    }

    #[test]
    fn uncommon_target_demotes_to_tier2() {
        let f = PatternFacts {
            target_frequency: COMMON_TARGET_MIN_FREQUENCY - 1,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Tier2Only {
                reason: Tier2Reason::TargetNotCommon
            }
        );
    }

    #[test]
    fn brake_tripped_demotes_to_tier2() {
        let f = PatternFacts {
            consecutive_undos: UNDO_BRAKE_STRIKES,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Tier2Only {
                reason: Tier2Reason::BrakeTripped
            }
        );
    }

    #[test]
    fn brake_below_strikes_does_not_demote() {
        let f = PatternFacts {
            consecutive_undos: UNDO_BRAKE_STRIKES - 1,
            ..ready_facts()
        };
        assert_eq!(classify_pattern(&f), PatternReadiness::Tier1Ready);
    }

    // ---- Gate precedence ------------------------------------------------

    #[test]
    fn typed_is_real_word_takes_precedence_over_brake() {
        // Both would force Tier-2; the more fundamental reason is reported.
        let f = PatternFacts {
            typed_is_known: true,
            consecutive_undos: UNDO_BRAKE_STRIKES,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Tier2Only {
                reason: Tier2Reason::TypedIsRealWord
            }
        );
    }

    #[test]
    fn confidence_gates_take_precedence_over_tier2_gates() {
        // Insufficient weight AND typed-is-real-word: not-enough-evidence wins
        // (there's no confidence to even offer a suggestion).
        let f = PatternFacts {
            weight: 1.0,
            typed_is_known: true,
            ..ready_facts()
        };
        assert_eq!(
            classify_pattern(&f),
            PatternReadiness::Silent {
                reason: SilentReason::InsufficientObservations
            }
        );
    }

    // ---- end-to-end via the store + real lexicon -----------------------

    fn observe_n(store: &mut WordPatternStore, typed: &str, target: &str, n: u32, now: u64) {
        for _ in 0..n {
            store.observe_correction(Outcome::CorrectedToOther, typed, target, now);
        }
    }

    #[test]
    fn end_to_end_teh_to_the_is_tier1_after_enough_observations() {
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        // "teh" is not a word; "the" is a common word — the canonical Tier-1.
        observe_n(&mut store, "teh", "the", 12, 1_000_000);
        assert_eq!(
            classify("teh", "the", &store, lex),
            PatternReadiness::Tier1Ready
        );
    }

    #[test]
    fn end_to_end_unseen_pattern_is_silent() {
        let lex = Lexicon::shared();
        let store = WordPatternStore::new();
        assert_eq!(
            classify("teh", "the", &store, lex),
            PatternReadiness::Silent {
                reason: SilentReason::Unseen
            }
        );
    }

    #[test]
    fn end_to_end_real_word_typo_is_tier2() {
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        // "form" and "from" are BOTH real words — a confident pattern here must
        // never silently rewrite (it'd clobber a word the user may have meant).
        assert!(lex.is_known("form"));
        observe_n(&mut store, "form", "from", 12, 1_000_000);
        assert_eq!(
            classify("form", "from", &store, lex),
            PatternReadiness::Tier2Only {
                reason: Tier2Reason::TypedIsRealWord
            }
        );
    }

    #[test]
    fn end_to_end_below_threshold_is_insufficient() {
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        observe_n(&mut store, "teh", "the", 3, 1_000_000);
        assert_eq!(
            classify("teh", "the", &store, lex),
            PatternReadiness::Silent {
                reason: SilentReason::InsufficientObservations
            }
        );
    }

    // ---- Step 2: common-vs-personal auto-fire classifier ----------------

    // Act-worthy facts: a non-word typo of a COMMON word, small (decodable)
    // distance — the canonical "macOS handles it" / defer case. Tests mutate
    // one field at a time.
    fn defer_facts() -> AutoFireFacts {
        AutoFireFacts {
            weight: TIER1_MIN_OBSERVATIONS,
            age_ms: 0,
            consecutive_undos: 0,
            typed_is_known: false,
            target_is_known: true,
            target_frequency: COMMON_DEFER_FREQUENCY,
            edit_distance: 2,
        }
    }

    #[test]
    fn common_word_small_typo_defers_to_autocorrect() {
        assert_eq!(
            classify_auto_fire(&defer_facts()),
            AutoFireClass::DeferToAutocorrect
        );
    }

    #[test]
    fn rare_target_is_personal() {
        // Below the defer frequency → macOS likely lacks it → we act.
        let f = AutoFireFacts {
            target_frequency: COMMON_DEFER_FREQUENCY - 1,
            ..defer_facts()
        };
        assert_eq!(classify_auto_fire(&f), AutoFireClass::Personal);
    }

    #[test]
    fn severe_jumble_of_common_word_is_personal() {
        // The "autocorrect on but misses" case: target is common, but the
        // jumble is too severe for macOS to decode → ours.
        let f = AutoFireFacts {
            edit_distance: MACOS_DECODABLE_MAX_DISTANCE + 1,
            ..defer_facts()
        };
        assert_eq!(classify_auto_fire(&f), AutoFireClass::Personal);
    }

    #[test]
    fn typed_real_word_is_not_actionable_even_if_personal_shaped() {
        // Safety: never auto-rewrite a real word, regardless of the split.
        let f = AutoFireFacts {
            typed_is_known: true,
            target_frequency: COMMON_DEFER_FREQUENCY - 1,
            ..defer_facts()
        };
        assert_eq!(
            classify_auto_fire(&f),
            AutoFireClass::NotActionable {
                reason: NotActionableReason::TypedIsRealWord
            }
        );
    }

    #[test]
    fn auto_fire_gates_mirror_the_tier_model() {
        // Unseen.
        assert_eq!(
            classify_auto_fire(&AutoFireFacts {
                weight: 0.0,
                ..defer_facts()
            }),
            AutoFireClass::NotActionable {
                reason: NotActionableReason::Unseen
            }
        );
        // Target not a word.
        assert_eq!(
            classify_auto_fire(&AutoFireFacts {
                target_is_known: false,
                ..defer_facts()
            }),
            AutoFireClass::NotActionable {
                reason: NotActionableReason::TargetNotAWord
            }
        );
        // Insufficient observations.
        assert_eq!(
            classify_auto_fire(&AutoFireFacts {
                weight: TIER1_MIN_OBSERVATIONS - 0.1,
                ..defer_facts()
            }),
            AutoFireClass::NotActionable {
                reason: NotActionableReason::InsufficientObservations
            }
        );
        // Stale.
        assert_eq!(
            classify_auto_fire(&AutoFireFacts {
                age_ms: STALE_AFTER_MS as u64 + 1,
                ..defer_facts()
            }),
            AutoFireClass::NotActionable {
                reason: NotActionableReason::Stale
            }
        );
        // Brake tripped.
        assert_eq!(
            classify_auto_fire(&AutoFireFacts {
                consecutive_undos: UNDO_BRAKE_STRIKES,
                ..defer_facts()
            }),
            AutoFireClass::NotActionable {
                reason: NotActionableReason::BrakeTripped
            }
        );
    }

    #[test]
    fn end_to_end_teh_to_the_defers_to_autocorrect() {
        // teh→the: common target, distance 2 → macOS's job, we defer. (Step 1's
        // manual allow-list is the override that let Alice test it.)
        let lex = Lexicon::shared();
        let mut store = WordPatternStore::new();
        observe_n(&mut store, "teh", "the", 12, 1_000_000);
        assert_eq!(
            classify_auto_fire_for("teh", "the", &store, lex),
            AutoFireClass::DeferToAutocorrect
        );
    }
}
