//! Linguistic gate — Component 5b's second axis on top of the motor
//! signal. Catches **malformed words from clean keystrokes** that motor
//! timing can't see: spatial substitutions, dropped-space merges, near
//! mangles of known words.
//!
//! The motor gate alone fast-tracks garbage: the worse a slip mangles
//! a word, the further from anything known → no candidate → FAST lane.
//! `themach`, `imapc`, `potentjual`, `okeep`, `si` all flowed through
//! motor-clean and reached `Provisional`. The linguistic gate **stacks
//! on motor** so a Kept must pass both to promote: not just typed
//! cleanly, but also linguistically plausible AND not a near mangle of
//! known vocabulary.
//!
//! Local-only — uses the bundled SCOWL + Norvig already in the
//! correction-engine crate; no new deps.
//!
//! ## Three signals
//!
//! 1. **Plausibility** — bigram log-probability over the bundled clean
//!    lexicon, with word-boundary markers. Common English letter
//!    pairs cluster high; `imapc`-shape garbage (the rare `pc` /
//!    word-final `c$` etc.) scores low. Names like `Soumyo` and
//!    `Krutrim` are unusual but their bigrams (`ou`, `um`, `ru`,
//!    `tr`, `ri`) still resemble real-word patterns. Acronyms like
//!    `ZAMS` are the awkward corner — short, atypical bigrams.
//!    Threshold is PLACEHOLDER; tune from the panel.
//!
//! 2. **Edit-2 proximity to known** — a no-candidate Kept is a mangle
//!    of something the user meant if a known word sits within edit
//!    distance 2. `imapc` ↔ `impact` (transpose `ap`/`pa` + delete
//!    `t` = 2 edits) → near-known. Composed via two
//!    [`crate::candidates::edit1`] passes; we early-exit as soon as
//!    a known word is found. Capped by [`MAX_PROXIMITY_LEN`] to keep
//!    long words cheap.
//!
//! 3. **Segmentable** — the word splits exactly into two-or-more
//!    *known* words (strict). Catches dropped-space merges where
//!    both halves are real (`andthe`, `tothe`). The brief's
//!    `themach` example (`the` known + `mach` *not* in SCOWL) is
//!    handled by the *prefix-merge* heuristic below — a known
//!    high-frequency prefix of length ≥ 3 followed by ≥ 2 chars of
//!    suffix. The prefix-merge path is a softer signal than full
//!    segmentation and may surface false positives on names that
//!    happen to start with `the`/`and`/etc; we surface it in the
//!    panel and tune from observation.
//!
//! ## What promotes
//!
//! Net fast-lane promotion needs all of:
//!   * No candidate (the lane premise).
//!   * Clean motor.
//!   * Plausibility ≥ floor.
//!   * Proximity = `FarFromKnown` (not edit-2 of a known word, not
//!     segmentable, not a prefix-merge).
//!
//! Slow lane is tightened too — the same plausibility and
//! segmentation gates apply. Slow-lane proximity is implicit (it has
//! a candidate by definition) but a Slow-lane Kept on an ill-formed
//! or segmentable token still holds.
//!
//! Observe-only — all of this surfaces on the LEXICON panel. The
//! proposer never writes to `is_known`.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::candidates::edit1;
use crate::lexicon::Lexicon;

// ---- Tunable PLACEHOLDERS — calibrate against the panel -------------------
//
// **What each signal is and isn't doing.** The calibration corpus
// (Soumyo / Krutrim / ZAMS vs imapc / potentjual / themach) revealed
// that mean bigram log-probability does NOT separate spatial mangles
// from real names: Soumyo scored -2.93, imapc scored -2.92. The
// bigrams `im / ma / ap / pc / c$` in imapc are all individually
// plausible. So plausibility is a backstop for truly weird character
// sequences (`qzqz`-shape) — it doesn't carry the load on spatial
// mangles. The proximity gate does that work, gated on the **frequency**
// of the matched known word: `impact` (61M) and `potential` (65M) are
// what flags imapc / potentjual; `sumo` (1M) is what *almost* flagged
// Soumyo until we required the match to be high-frequency.

/// Mean-log10-probability floor. Catches truly anomalous strings
/// (`qzqz`-shape) but NOT spatial mangles built from common bigrams.
/// **PLACEHOLDER** — re-pin against real typing.
pub const PLAUSIBILITY_FLOOR: f64 = -4.5;

/// Skip edit-2 proximity for words longer than this. Cost is
/// `O(edit1(word)^2)` ≈ `O((54n)^2)` HashMap lookups; cap at 12 keeps
/// `potentjual` (10 chars) in scope while bounding the worst-case at
/// a few ms per token. Longer mangles are less common in practice.
pub const MAX_PROXIMITY_LEN: usize = 12;

/// Edit-2 matches only count when the matched known word has **at
/// least this much** frequency. Splits `impact` (61M) from `sumo`
/// (1M) — the user's calibration. Without this gate the proximity
/// signal floods on short words: every 5-6 char name has dozens of
/// edit-2 neighbours in a 90k-word dictionary; nearly all are rare.
/// **PLACEHOLDER**.
pub const NEAR_KNOWN_MIN_FREQ: u64 = 10_000_000;

/// Strict-segmentation halves must each be either ≥ 3 chars, or
/// short but very common (`of`, `to`). Without this, short SCOWL
/// interjections (`so`, `um`, `yo`) over-segment names like `Soumyo`
/// into `so` + `um` + `yo`. **PLACEHOLDER**.
pub const SEGMENT_MIN_LEN: usize = 3;
pub const SEGMENT_SHORT_HALF_MIN_FREQ: u64 = 1_000_000_000;

/// Minimum prefix length and frequency required to flag a
/// **prefix-merge** (a softer segmentation signal — known prefix +
/// non-empty unknown suffix). Top-of-corpus frequency threshold;
/// SCOWL's `the`, `and`, `for`, `of`, `to` clear it easily. **PLACEHOLDER**.
pub const PREFIX_MERGE_MIN_LEN: usize = 3;
pub const PREFIX_MERGE_MIN_FREQ: u64 = 10_000_000;
/// Suffix-of-merge must have at least this many chars (otherwise
/// `theresa` → `there` + `sa` would over-flag).
pub const PREFIX_MERGE_MIN_SUFFIX: usize = 3;

// ---- Public types ---------------------------------------------------------

/// What proximity-to-known check decided. Surfaced on the proposal so
/// the LEXICON panel can show *why* a word held (or didn't).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProximityVerdict {
    /// No edit-2 known word, no segmentation, no prefix-merge.
    /// The word looks like genuinely novel vocabulary.
    FarFromKnown,
    /// A known word exists at edit distance ≤ 2. The token is most
    /// likely a mangle of that word, not new vocabulary.
    NearKnownEdit2,
    /// Word splits exactly into 2+ known words (strict).
    Segmentable,
    /// Word starts with a high-frequency known prefix and has a
    /// non-empty suffix. Softer signal — surfaces things like
    /// `themach` where the suffix isn't itself a known word.
    PrefixMerge,
}

/// The full linguistic verdict for one word. Attached to the
/// proposal so the panel can render plausibility + proximity inline
/// with motor verdict.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LinguisticSignal {
    /// Mean log10-probability of the word's character bigrams against
    /// the bundled SCOWL distribution. Negative; higher = more
    /// plausible. Compare to [`PLAUSIBILITY_FLOOR`].
    pub plausibility: f64,
    pub well_formed: bool,
    pub proximity: ProximityVerdict,
}

// ---- Bigram plausibility model --------------------------------------------

/// Sentinel char to mark word boundaries in bigram keys. Picked from
/// the ASCII control range so it can never collide with a real word
/// character.
const BOUNDARY: char = '\u{0001}';

struct BigramModel {
    /// `log10(count(bigram) / total)` for each observed bigram.
    log_probs: HashMap<(char, char), f64>,
    /// Floor for unseen bigrams — `log10(0.5 / total)` (additive
    /// smoothing, not full Laplace; punishes truly novel pairs
    /// without driving the score to `-inf`).
    unseen_log_prob: f64,
}

impl BigramModel {
    fn build(lex: &Lexicon) -> Self {
        let mut counts: HashMap<(char, char), u64> = HashMap::with_capacity(2_500);
        let mut total: u64 = 0;
        for word in lex.iter_clean() {
            // Words are already lowercase from `Lexicon::load`.
            let chars: Vec<char> = std::iter::once(BOUNDARY)
                .chain(word.chars())
                .chain(std::iter::once(BOUNDARY))
                .collect();
            if chars.len() < 2 {
                continue;
            }
            for win in chars.windows(2) {
                let key = (win[0], win[1]);
                *counts.entry(key).or_insert(0) += 1;
                total += 1;
            }
        }
        let total_f = total.max(1) as f64;
        let log_probs: HashMap<_, _> = counts
            .into_iter()
            .map(|(k, v)| (k, ((v as f64) / total_f).log10()))
            .collect();
        // Unseen bigram score: pretend we saw it half a time. Less
        // punitive than `log10(1/total)`, more punitive than the
        // smallest observed bigram.
        let unseen_log_prob = (0.5 / total_f).log10();
        Self {
            log_probs,
            unseen_log_prob,
        }
    }

    fn plausibility(&self, word: &str) -> f64 {
        if word.is_empty() {
            return self.unseen_log_prob;
        }
        let lower = word.to_ascii_lowercase();
        let chars: Vec<char> = std::iter::once(BOUNDARY)
            .chain(lower.chars())
            .chain(std::iter::once(BOUNDARY))
            .collect();
        let mut sum = 0.0;
        let mut n = 0.0;
        for win in chars.windows(2) {
            sum += self
                .log_probs
                .get(&(win[0], win[1]))
                .copied()
                .unwrap_or(self.unseen_log_prob);
            n += 1.0;
        }
        if n == 0.0 {
            self.unseen_log_prob
        } else {
            sum / n
        }
    }
}

fn shared_bigram_model() -> &'static BigramModel {
    static MODEL: OnceLock<BigramModel> = OnceLock::new();
    MODEL.get_or_init(|| BigramModel::build(Lexicon::shared()))
}

// ---- Public API -----------------------------------------------------------

pub fn plausibility(word: &str) -> f64 {
    shared_bigram_model().plausibility(word)
}

pub fn is_well_formed(word: &str) -> bool {
    plausibility(word) >= PLAUSIBILITY_FLOOR
}

/// Decide the proximity verdict for a no-candidate (or any) word.
/// Checks, in order:
///   1. Strict segmentation into 2+ known words.
///   2. Prefix-merge (known high-frequency prefix + non-empty suffix).
///   3. Edit-2 to a known word.
///   4. Otherwise `FarFromKnown`.
///
/// The order is "softer first → harder last" so the panel's hold
/// reason reflects the most specific signal that fired. Edit-2 is
/// computationally heaviest and most permissive.
pub fn proximity_verdict(word: &str, lex: &Lexicon) -> ProximityVerdict {
    if is_segmentable(word, lex) {
        return ProximityVerdict::Segmentable;
    }
    if is_prefix_merge(word, lex) {
        return ProximityVerdict::PrefixMerge;
    }
    if has_known_at_edit_2(word, lex) {
        return ProximityVerdict::NearKnownEdit2;
    }
    ProximityVerdict::FarFromKnown
}

pub fn linguistic_signal(word: &str, lex: &Lexicon) -> LinguisticSignal {
    let plausibility = plausibility(word);
    LinguisticSignal {
        plausibility,
        well_formed: plausibility >= PLAUSIBILITY_FLOOR,
        proximity: proximity_verdict(word, lex),
    }
}

// ---- Proximity helpers ----------------------------------------------------

/// Strict segmentation: word splits into 2+ known words, each
/// half satisfying [`is_valid_segment`] (≥ 3 chars OR very-high
/// frequency). The frequency carve-out keeps `of` / `to` / `the` /
/// `and` valid as halves while excluding shorter, less-common
/// interjections (`so`, `um`, `yo`) that would over-segment names.
pub fn is_segmentable(word: &str, lex: &Lexicon) -> bool {
    let lower = word.to_ascii_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    segmentable_helper(&chars, lex)
}

fn segmentable_helper(chars: &[char], lex: &Lexicon) -> bool {
    let n = chars.len();
    if n < 4 {
        return false;
    }
    for split in 2..=n - 2 {
        let left: String = chars[..split].iter().collect();
        if !is_valid_segment(&left, lex) {
            continue;
        }
        let right: String = chars[split..].iter().collect();
        if is_valid_segment(&right, lex) {
            return true;
        }
        if segmentable_helper(&chars[split..], lex) {
            return true;
        }
    }
    false
}

/// A "segment" is valid iff it's a known word AND either long enough
/// to carry standalone meaning OR among the corpus's most-frequent
/// short words. Carve-out for short high-freq words: `the` (3 chars),
/// `of` / `to` (2 chars) are real functional roots; `so` / `um` / `yo`
/// (also 2 chars but moderate freq) are not.
fn is_valid_segment(word: &str, lex: &Lexicon) -> bool {
    if !lex.is_known(word) {
        return false;
    }
    if word.chars().count() >= SEGMENT_MIN_LEN {
        return true;
    }
    lex.frequency(word) >= SEGMENT_SHORT_HALF_MIN_FREQ
}

/// Prefix-merge: longest known prefix is ≥ [`PREFIX_MERGE_MIN_LEN`],
/// its frequency clears [`PREFIX_MERGE_MIN_FREQ`], and the remaining
/// suffix is ≥ [`PREFIX_MERGE_MIN_SUFFIX`] chars. Softer than strict
/// segmentation — catches the `themach` (`the` + `mach`) class where
/// the suffix isn't itself a recognized word.
pub fn is_prefix_merge(word: &str, lex: &Lexicon) -> bool {
    let lower = word.to_ascii_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let n = chars.len();
    if n < PREFIX_MERGE_MIN_LEN + PREFIX_MERGE_MIN_SUFFIX {
        return false;
    }
    // Longest-first so `them` beats `the` for `themach` and we get
    // the most specific signal.
    let max_prefix = n - PREFIX_MERGE_MIN_SUFFIX;
    for len in (PREFIX_MERGE_MIN_LEN..=max_prefix).rev() {
        let prefix: String = chars[..len].iter().collect();
        if !lex.is_known(&prefix) {
            continue;
        }
        if lex.frequency(&prefix) >= PREFIX_MERGE_MIN_FREQ {
            return true;
        }
    }
    false
}

/// True iff any **high-frequency** known word sits at edit distance
/// ≤ 2 from `word`. Caller should already know `word` is not edit-1
/// of a known word (slow lane would have a candidate); this checks
/// the next ring out, restricted to matches with frequency
/// ≥ [`NEAR_KNOWN_MIN_FREQ`].
///
/// The frequency gate is what splits `imapc` ↔ `impact` (61M, flagged)
/// from `Soumyo` ↔ `sumo` (1M, not flagged): nearly any short word
/// has a few rare edit-2 neighbours in a 90k-word dictionary, but a
/// match against a **common** word is a real "user mangled this"
/// signal. Capped at [`MAX_PROXIMITY_LEN`] to bound the search cost.
pub fn has_known_at_edit_2(word: &str, lex: &Lexicon) -> bool {
    let len = word.chars().count();
    if !(2..=MAX_PROXIMITY_LEN).contains(&len) {
        return false;
    }
    let e1 = edit1(word);
    for w in &e1 {
        for e2_word in edit1(w) {
            // is_known + freq gate: must be a real dictionary word
            // (not just a Norvig typo with a count) AND common
            // enough to plausibly be what the user meant.
            if lex.is_known(&e2_word) && lex.frequency(&e2_word) >= NEAR_KNOWN_MIN_FREQ {
                return true;
            }
        }
    }
    false
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Calibration corpus (diagnostic) -------------------------------

    /// Diagnostic: print which known word(s) match within edit-2 for
    /// a given input. Used to understand false positives in the
    /// proximity gate.
    #[test]
    #[ignore]
    fn dump_edit_2_matches_for_calibration_words() {
        let lex = Lexicon::shared();
        let words = ["Soumyo", "Krutrim", "ZAMS", "imapc", "potentjual"];
        for w in &words {
            let lower = w.to_ascii_lowercase();
            let mut hits: Vec<String> = Vec::new();
            for e1 in edit1(&lower) {
                for e2 in edit1(&e1) {
                    if lex.is_known(&e2) && !hits.contains(&e2) {
                        hits.push(e2);
                        if hits.len() >= 5 {
                            break;
                        }
                    }
                }
                if hits.len() >= 5 {
                    break;
                }
            }
            eprintln!("{:>15} edit-2 known: {:?}", w, hits);
        }
    }

    #[test]
    #[ignore] // Run with --ignored to print scores for tuning.
    fn dump_calibration_scores() {
        let words = [
            // Must pass.
            "Soumyo",
            "Krutrim",
            "ZAMS",
            "ONDC",
            "hello",
            "machine",
            "compiler",
            "lol",
            "youbd",
            // Must fail.
            "imapc",
            "potentjual",
            "themach",
            "okeep",
            "si",
            "ima",
            "qzqz",
            "aduluts",
            "un",
            "ubbncudebce",
        ];
        eprintln!("plausibility scores (placeholder floor {PLAUSIBILITY_FLOOR}):");
        for w in &words {
            eprintln!("  {:>15} = {:>7.3}", w, plausibility(w));
        }
    }

    // ---- Plausibility ---------------------------------------------------

    #[test]
    fn plausibility_is_higher_for_real_words_than_for_garbage() {
        // Common SCOWL words clear the floor; obvious garbage falls
        // well below. Concrete thresholds tune against the panel —
        // these tests check the ORDER, not the absolute values.
        let known = plausibility("hello");
        let mangle = plausibility("imapc");
        let garbage = plausibility("potentjual");
        assert!(
            known > mangle,
            "known word 'hello' must score above mangle 'imapc' \
             (known={known}, mangle={mangle})"
        );
        assert!(
            known > garbage,
            "known word 'hello' must score above garbage 'potentjual' \
             (known={known}, garbage={garbage})"
        );
    }

    #[test]
    fn plausibility_floor_passes_real_unusual_names() {
        // Real names must clear the floor — the bigram model is a
        // backstop, not the primary gate, but it should never flag a
        // legitimate name. (ZAMS / Krutrim / ONDC are seed proper
        // nouns and short-circuit as Known upstream — the proposer
        // never sees them — so we only assert against Soumyo here.)
        let p = plausibility("Soumyo");
        assert!(
            p >= PLAUSIBILITY_FLOOR,
            "Soumyo scored {p} — below the placeholder floor \
             ({PLAUSIBILITY_FLOOR}). Retune the floor or the model."
        );
    }

    #[test]
    fn plausibility_floor_catches_only_truly_anomalous_strings() {
        // Bigram-mean is a weak signal: imapc (-2.92) and Soumyo
        // (-2.93) score IDENTICALLY because they share common
        // English letter pairs. Only strings with genuinely rare
        // bigrams (qz, xq, etc.) drop below the floor. Spatial
        // mangles fall to the proximity gate; this assertion just
        // pins the floor's role as a `qzqz`-class backstop.
        assert!(
            plausibility("qzqz") < PLAUSIBILITY_FLOOR,
            "truly anomalous bigrams must fall below the floor"
        );
        // imapc and potentjual share common bigrams and intentionally
        // pass plausibility — the proximity gate catches them. Pin
        // this so a future bigram model upgrade doesn't silently
        // start gating these by plausibility instead.
        assert!(
            plausibility("imapc") >= PLAUSIBILITY_FLOOR,
            "imapc passes the bigram backstop by design; near-known \
             edit-2 catches it instead"
        );
        assert!(
            plausibility("potentjual") >= PLAUSIBILITY_FLOOR,
            "potentjual passes the bigram backstop by design"
        );
    }

    // ---- Segmentation ---------------------------------------------------

    #[test]
    fn strict_segmentation_catches_dropped_space_merge() {
        // "andthe" = and + the. Both halves in SCOWL → strictly
        // segmentable.
        let lex = Lexicon::shared();
        assert!(is_segmentable("andthe", lex));
        assert!(is_segmentable("inthe", lex));
        assert!(is_segmentable("ofthe", lex));
    }

    #[test]
    fn strict_segmentation_rejects_novel_names() {
        // Soumyo *would* segment greedily as `so` + `um` + `yo` if any
        // 2-char known interjection counted. The segment-validity
        // rule (≥ 3 chars OR very-high freq) rules out `so` / `um` /
        // `yo` and keeps the name intact.
        let lex = Lexicon::shared();
        assert!(!is_segmentable("Soumyo", lex));
    }

    #[test]
    fn prefix_merge_catches_themach() {
        // The user's example: `the` is a known high-frequency prefix
        // and `mach` (4 chars) is the remaining suffix. Strict
        // segmentation misses this (mach isn't in SCOWL), but the
        // prefix-merge heuristic catches it.
        let lex = Lexicon::shared();
        assert!(is_prefix_merge("themach", lex));
    }

    #[test]
    fn prefix_merge_lets_novel_names_through() {
        // Names that don't start with a high-frequency known prefix
        // must NOT be flagged as merges. Soumyo/Krutrim/ZAMS pass;
        // these are the calibration corpus.
        let lex = Lexicon::shared();
        assert!(!is_prefix_merge("Soumyo", lex));
        assert!(!is_prefix_merge("Krutrim", lex));
        assert!(!is_prefix_merge("ZAMS", lex));
    }

    // ---- Edit-2 proximity ----------------------------------------------

    #[test]
    fn near_known_edit_2_catches_spatial_mangles() {
        // `imapc` ↔ `impact`: transpose `ap`↔`pa` (1) + delete `t`
        // (1) = edit-2. Must be flagged.
        let lex = Lexicon::shared();
        assert!(
            has_known_at_edit_2("imapc", lex),
            "imapc must register as near-known (impact is edit-2 away)"
        );
    }

    #[test]
    fn near_known_edit_2_lets_novel_names_through() {
        // Soumyo IS within edit-2 of `sumo` (1M) and `soupy`
        // (low-frequency) — but neither clears the
        // NEAR_KNOWN_MIN_FREQ gate, so Soumyo is not flagged. This
        // is the frequency carve-out the calibration corpus
        // required.
        let lex = Lexicon::shared();
        assert!(
            !has_known_at_edit_2("Soumyo", lex),
            "Soumyo: no edit-2 neighbour clears the {NEAR_KNOWN_MIN_FREQ}-freq gate"
        );
        // Krutrim and ZAMS are seed proper nouns — known upstream and
        // never reach the proposer. The dump test confirms they're
        // case-insensitive-known at edit-0; we don't need a separate
        // assertion here.
    }

    #[test]
    fn near_known_edit_2_caps_long_words() {
        // Past the length cap, returns false even for genuinely-near
        // mangles — bounds the worst-case cost.
        let lex = Lexicon::shared();
        let long_mangle = "a".repeat(MAX_PROXIMITY_LEN + 1);
        assert!(
            !has_known_at_edit_2(&long_mangle, lex),
            "length cap must short-circuit before checking edit-2"
        );
    }

    // ---- Combined verdict ----------------------------------------------

    #[test]
    fn novel_name_lands_far_from_known() {
        let lex = Lexicon::shared();
        let v = proximity_verdict("Soumyo", lex);
        assert_eq!(v, ProximityVerdict::FarFromKnown);
    }

    #[test]
    fn dropped_space_merge_lands_segmentable() {
        let lex = Lexicon::shared();
        let v = proximity_verdict("andthe", lex);
        assert_eq!(v, ProximityVerdict::Segmentable);
    }

    #[test]
    fn themach_lands_prefix_merge() {
        let lex = Lexicon::shared();
        let v = proximity_verdict("themach", lex);
        assert_eq!(v, ProximityVerdict::PrefixMerge);
    }

    #[test]
    fn spatial_mangle_lands_near_known_edit_2() {
        let lex = Lexicon::shared();
        let v = proximity_verdict("imapc", lex);
        assert_eq!(v, ProximityVerdict::NearKnownEdit2);
    }
}
