//! **Early space-drift (Build 3a)** — the fast-thumb slip where the space lands
//! one slot early, so the last letter of the left word jumps past the space and
//! glues onto the next word: intended `not think`, typed `no tthink`.
//!
//! The fix is to slide the space ONE slot right (`no tthink` → `not think`).
//! **Offer only, never auto-applied** (Principle #9 — the user confirms with
//! Shift, exactly like every other bubble).
//!
//! ## Detection is structural — timing is NOT used
//!
//! Everything here is decided from the two sealed tokens and the lexicon. No
//! timestamp, dwell or inter-key gap is read or stored. (A timing signal is a
//! later build; this one must stand on structure alone.)
//!
//! ## The pair rule — the re-split must beat the single-word fix
//!
//! For nearly every Early case a competing single-word fix also exists (the bold
//! lane turns `tthink → think`, giving `no think` — also two valid words). So the
//! re-split only wins when ALL of these hold:
//!
//! 1. **Structure** — the right token is a non-word, both slid halves are known.
//! 2. **Floor** — each half is a *valid segment* and at least
//!    [`HALF_MIN_FREQ`] frequent (not a rare word that happens to fit).
//! 3. **Left gain** — the slid left word is more frequent than the original left
//!    word by the user's current bar `R` (`not` 2.6B vs `no` 0.9B).
//! 4. **Per-user bar** — `R` starts strict and moves with this user's own
//!    taken/ignored record ([`DriftStats::bar`]).
//!
//! Fail any gate and [`detect_early`] returns a [`DriftReject`]; the engine then
//! runs the learned and bold lanes exactly as before. A word-bigram table (a real
//! pair model) is deliberately NOT built — it is the fallback if live data shows
//! wrong offers getting through.
//!
//! ## What is persisted
//!
//! Only [`DriftStats`] — three aggregate counts (`shown` / `taken` / `ignored`),
//! no words, no timing. They are the *confidence* signal for the bar, not a
//! direction predictor.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::linguistic::{is_valid_segment, NEAR_KNOWN_MIN_FREQ};
use crate::tokenizer::{Token, TokenKind};
use crate::Lexicon;

/// File (under the data dir) holding [`DriftStats`].
pub const SPACE_DRIFT_FILE: &str = "space_drift.json";
/// Schema version of [`SPACE_DRIFT_FILE`].
pub const SPACE_DRIFT_FILE_VERSION: u32 = 1;

/// Each re-split half must be at least this frequent — "not a rare word that
/// happens to fit". Reuses the proximity floor. Words missing from the bundled
/// frequency list read as ~0 and fail, which is the intent. **Tunable.**
pub const HALF_MIN_FREQ: u64 = NEAR_KNOWN_MIN_FREQ;
/// The left-gain bar a brand-new user starts at: the slid left word must be this
/// many times more frequent than the original left word. **Tunable placeholder.**
pub const BAR_START: f64 = 2.0;
/// The most relaxed the bar ever gets (the design's "more frequent" rule).
pub const BAR_FLOOR: f64 = 1.0;
/// The strictest the bar ever gets before the check stands down.
pub const BAR_CAP: f64 = 4.0;
/// Decided offers (taken + ignored) needed before the user's record moves the bar.
pub const MIN_DECIDED_FOR_BAR: u32 = 5;
/// Take rate at or above which the bar relaxes to [`BAR_FLOOR`].
pub const TAKE_RATE_RELAX: f64 = 0.70;
/// Take rate at or below which the bar tightens to [`BAR_CAP`].
pub const TAKE_RATE_TIGHTEN: f64 = 0.30;
/// Decided offers needed before a very low take rate silences the check.
pub const STAND_DOWN_MIN_DECIDED: u32 = 10;
/// Take rate under which (with enough decided offers) the check stands down.
pub const STAND_DOWN_TAKE_RATE: f64 = 0.15;

/// The per-user bar for the left-gain gate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bar {
    /// The slid left word must beat the original by this ratio.
    Ratio(f64),
    /// This user keeps ignoring these offers — stay quiet (still counted).
    StandDown,
}

/// Aggregate per-user record of Early-drift offers. Counts only — no words, no
/// timing. Persisted as [`SPACE_DRIFT_FILE`]; the engine task is its sole writer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftStats {
    /// Offers put on screen.
    #[serde(default)]
    pub shown: u32,
    /// Offers the user accepted.
    #[serde(default)]
    pub taken: u32,
    /// Offers that ended without an accept.
    #[serde(default)]
    pub ignored: u32,
}

impl DriftStats {
    /// Decided offers (an offer still on screen is not yet decided).
    pub fn decided(&self) -> u32 {
        self.taken + self.ignored
    }

    /// Fraction of decided offers that were taken, or `None` with nothing decided.
    pub fn take_rate(&self) -> Option<f64> {
        let d = self.decided();
        (d > 0).then(|| f64::from(self.taken) / f64::from(d))
    }

    /// The bar for the next offer. Starts strict; the user's own record relaxes it
    /// (they keep taking these) or tightens it / silences it (they keep ignoring).
    pub fn bar(&self) -> Bar {
        let Some(rate) = self.take_rate() else {
            return Bar::Ratio(BAR_START);
        };
        let d = self.decided();
        if d >= STAND_DOWN_MIN_DECIDED && rate < STAND_DOWN_TAKE_RATE {
            return Bar::StandDown;
        }
        if d < MIN_DECIDED_FOR_BAR {
            return Bar::Ratio(BAR_START);
        }
        if rate >= TAKE_RATE_RELAX {
            Bar::Ratio(BAR_FLOOR)
        } else if rate <= TAKE_RATE_TIGHTEN {
            Bar::Ratio(BAR_CAP)
        } else {
            Bar::Ratio(BAR_START)
        }
    }
}

/// On-disk envelope for [`DriftStats`].
#[derive(Debug, Serialize, Deserialize)]
struct DriftFile {
    version: u32,
    #[serde(default)]
    early: DriftStats,
}

/// Load the saved stats. A missing file is a fresh start; a corrupt or
/// wrong-version file is quarantined (never deleted) and the stats start empty.
pub fn load_stats(path: &Path) -> DriftStats {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return DriftStats::default(),
    };
    match serde_json::from_slice::<DriftFile>(&bytes) {
        Ok(f) if f.version == SPACE_DRIFT_FILE_VERSION => f.early,
        Ok(f) => {
            crate::persist::quarantine_corrupt(
                path,
                format!("space_drift version {} unsupported", f.version),
            );
            DriftStats::default()
        }
        Err(e) => {
            crate::persist::quarantine_corrupt(path, e);
            DriftStats::default()
        }
    }
}

/// Durably write the stats.
pub fn save_stats(path: &Path, stats: &DriftStats) -> std::io::Result<()> {
    let file = DriftFile {
        version: SPACE_DRIFT_FILE_VERSION,
        early: *stats,
    };
    let bytes = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
    crate::persist::durable_write(path, &bytes)
}

/// Why the Early-drift check did not offer. Every variant is counted by the
/// engine (Principle #7) so a silent drop is impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftReject {
    /// No left token on the line (a line reset, or the first word).
    NoLeft,
    /// The left token is not a plain, correctable, letters-only Word followed by
    /// exactly one space — or has trailing punctuation.
    LeftShape,
    /// The left token is a single letter (`I`, `a`) — too often intentional
    /// (`I tthink` → `It think` would pass every frequency test).
    LeftTooShort,
    /// The tokens are not exactly one space apart on the same line.
    NotAdjacent,
    /// The right token is not a plain, correctable, letters-only Word, or its
    /// first letter is not a lowercase ASCII letter, or it carries punctuation.
    RightShape,
    /// The right token is already a valid word — stay quiet (`no there`).
    RightAlreadyValid,
    /// The left token is itself not a word — the break is on the left (Late
    /// drift), not this check's case.
    LeftNotWord,
    /// A slid half would be too short (the right half under two letters).
    HalfTooShort,
    /// A slid half is not a known word.
    HalfUnknown,
    /// A slid half is a rare word that merely happens to fit.
    BelowFloor,
    /// The slid left word is not enough more frequent than the original.
    LeftGain,
    /// This user's own record has silenced the check.
    StandDown,
    /// The focused field is a web / contenteditable host (Google Docs, Gmail, any
    /// browser tab, Electron web views). The re-split edit deletes an interior
    /// space, and in those hosts the delete burst has been observed to land one
    /// character long (QA-15) — so Early drift is **native-only**. Applied by the
    /// engine join (`early_drift_hit`), not the pure detector.
    WebHost,
    /// L1 has not yet reported what kind of surface is focused, so it cannot be
    /// shown to be a native field — fail closed (garbling text is the one outcome
    /// we refuse). Applied by the engine join.
    SurfaceUnknown,
}

impl DriftReject {
    /// Stable snake_case tag for logs and counters.
    pub fn tag(self) -> &'static str {
        match self {
            Self::NoLeft => "no_left",
            Self::LeftShape => "left_shape",
            Self::LeftTooShort => "left_too_short",
            Self::NotAdjacent => "not_adjacent",
            Self::RightShape => "right_shape",
            Self::RightAlreadyValid => "right_already_valid",
            Self::LeftNotWord => "left_not_word",
            Self::HalfTooShort => "half_too_short",
            Self::HalfUnknown => "half_unknown",
            Self::BelowFloor => "below_floor",
            Self::LeftGain => "left_gain",
            Self::StandDown => "stand_down",
            Self::WebHost => "web_host",
            Self::SurfaceUnknown => "surface_unknown",
        }
    }
}

/// A detected Early drift — the proposed re-split. Offer only.
#[derive(Debug, Clone, PartialEq)]
pub struct EarlyDrift {
    /// The original left word as typed (`no`).
    pub left: String,
    /// The original right fragment as typed (`tthink`).
    pub right: String,
    /// The character that jumped the space (`t`).
    pub moved: char,
    /// The re-split left word, left's casing kept (`not`).
    pub left_fixed: String,
    /// The re-split right word (`think`).
    pub right_fixed: String,
    /// Bundled frequency of the re-split left word (for the shadow log).
    pub left_fixed_freq: u64,
    /// Bundled frequency of the original left word.
    pub left_freq: u64,
    /// Bundled frequency of the re-split right word.
    pub right_fixed_freq: u64,
}

impl EarlyDrift {
    /// What the user typed across the span: `no tthink`.
    pub fn typed_span(&self) -> String {
        format!("{} {}", self.left, self.right)
    }
    /// What the re-split reads: `not think`.
    pub fn target_span(&self) -> String {
        format!("{} {}", self.left_fixed, self.right_fixed)
    }
}

fn letters_only(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic())
}

/// The Early-drift check. `left` is the token sealed just before `right` on the
/// same line (`tokenizer.tokens()[len-2]`), or `None` when there isn't one.
///
/// Pure and structural: reads only the two tokens' text and spans plus the
/// lexicon. **Timing is not an input.**
pub fn detect_early(
    left: Option<&Token>,
    right: &Token,
    lexicon: &Lexicon,
    bar: Bar,
) -> Result<EarlyDrift, DriftReject> {
    let left = left.ok_or(DriftReject::NoLeft)?;

    // ---- shapes ----
    if !matches!(right.kind, TokenKind::Word)
        || !right.correctable
        || !right.trailing.is_empty()
        || !letters_only(&right.core)
    {
        return Err(DriftReject::RightShape);
    }
    if !matches!(left.kind, TokenKind::Word)
        || !left.correctable
        || !left.trailing.is_empty()
        || left.terminator != Some(' ')
        || !letters_only(&left.core)
    {
        return Err(DriftReject::LeftShape);
    }
    if left.core.chars().count() < 2 {
        return Err(DriftReject::LeftTooShort);
    }
    // Exactly one space between them: the left token's end, one space, then the
    // right token's start.
    if left.end + 1 != right.start {
        return Err(DriftReject::NotAdjacent);
    }
    let moved = right.core.chars().next().ok_or(DriftReject::RightShape)?;
    if !moved.is_ascii_lowercase() {
        return Err(DriftReject::RightShape);
    }

    // ---- quiet cases ----
    if lexicon.is_known(&right.core) {
        return Err(DriftReject::RightAlreadyValid);
    }
    if !lexicon.is_known(&left.core) {
        return Err(DriftReject::LeftNotWord);
    }

    // ---- the slide ----
    let left_fixed = format!("{}{}", left.core, moved);
    let right_fixed: String = right.core.chars().skip(1).collect();
    if right_fixed.chars().count() < 2 {
        return Err(DriftReject::HalfTooShort);
    }
    if !lexicon.is_known(&left_fixed) || !lexicon.is_known(&right_fixed) {
        return Err(DriftReject::HalfUnknown);
    }

    // ---- floor ----
    let left_fixed_freq = lexicon.frequency(&left_fixed);
    let right_fixed_freq = lexicon.frequency(&right_fixed);
    if !is_valid_segment(&left_fixed.to_ascii_lowercase(), lexicon)
        || !is_valid_segment(&right_fixed.to_ascii_lowercase(), lexicon)
        || left_fixed_freq < HALF_MIN_FREQ
        || right_fixed_freq < HALF_MIN_FREQ
    {
        return Err(DriftReject::BelowFloor);
    }

    // ---- per-user bar + left gain ----
    let ratio = match bar {
        Bar::StandDown => return Err(DriftReject::StandDown),
        Bar::Ratio(r) => r,
    };
    let left_freq = lexicon.frequency(&left.core);
    if (left_fixed_freq as f64) <= ratio * (left_freq as f64) {
        return Err(DriftReject::LeftGain);
    }

    Ok(EarlyDrift {
        left: left.core.clone(),
        right: right.core.clone(),
        moved,
        left_fixed,
        right_fixed,
        left_fixed_freq,
        left_freq,
        right_fixed_freq,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::Tokenizer;

    fn lex() -> &'static Lexicon {
        Lexicon::shared()
    }

    /// Run a line through the real tokenizer and return the last two sealed
    /// tokens, exactly as the engine sees them at the seal.
    fn seal(line: &str) -> (Option<Token>, Token) {
        let mut t = Tokenizer::new();
        let mut last = None;
        for c in line.chars() {
            if let Some(tok) = t.observe_char(c) {
                last = Some(tok);
            }
        }
        let toks = t.tokens();
        let right = last.expect("a token sealed");
        let left = (toks.len() >= 2).then(|| toks[toks.len() - 2].clone());
        (left, right)
    }

    fn detect(line: &str, bar: Bar) -> Result<EarlyDrift, DriftReject> {
        let (l, r) = seal(line);
        detect_early(l.as_ref(), &r, lex(), bar)
    }

    const START: Bar = Bar::Ratio(BAR_START);

    #[test]
    fn canonical_case_resplits() {
        let d = detect("no tthink ", START).expect("no tthink → not think");
        assert_eq!(d.typed_span(), "no tthink");
        assert_eq!(d.target_span(), "not think");
        assert_eq!(d.moved, 't');
        assert!(d.left_fixed_freq > d.left_freq);
    }

    #[test]
    fn the_left_token_really_is_in_tokens_with_its_span() {
        // The detection's premise: at the seal of the right token, the left word is
        // already in `tokenizer.tokens()` with a span exactly one space before it.
        let (l, r) = seal("no tthink ");
        let l = l.expect("left present");
        assert_eq!(l.core, "no");
        assert_eq!(l.terminator, Some(' '));
        assert_eq!(l.end + 1, r.start);
    }

    #[test]
    fn both_sides_already_valid_stays_quiet() {
        assert_eq!(
            detect("no there ", START),
            Err(DriftReject::RightAlreadyValid)
        );
    }

    #[test]
    fn single_letter_left_is_excluded() {
        // `I tthink` → `It think` would pass every frequency test.
        assert_eq!(detect("I tthink ", START), Err(DriftReject::LeftTooShort));
        assert_eq!(detect("a tthink ", START), Err(DriftReject::LeftTooShort));
    }

    #[test]
    fn rarer_slid_left_word_is_refused() {
        // The slid left word is rarer than the original, so the left-gain gate (or
        // the floor) refuses it: so→sot, he→het, we→wet, do→dot.
        for line in ["so tthink ", "he tthink ", "we tthink ", "do tthink "] {
            let r = detect(line, START);
            assert!(
                matches!(
                    r,
                    Err(DriftReject::LeftGain | DriftReject::BelowFloor | DriftReject::HalfUnknown)
                ),
                "{line:?} must not offer a re-split, got {r:?}"
            );
        }
    }

    #[test]
    fn no_left_token_abstains() {
        let (_, r) = seal("tthink ");
        assert_eq!(
            detect_early(None, &r, lex(), START),
            Err(DriftReject::NoLeft)
        );
    }

    #[test]
    fn punctuation_and_nonadjacent_abstain() {
        // Two spaces → not exactly one slot.
        assert!(detect("no  tthink ", START).is_err());
        // Trailing punctuation on the left word.
        assert_eq!(detect("no, tthink ", START), Err(DriftReject::LeftShape));
    }

    #[test]
    fn capitalised_left_keeps_its_casing() {
        let d = detect("No tthink ", START).expect("No tthink → Not think");
        assert_eq!(d.target_span(), "Not think");
    }

    #[test]
    fn uppercase_stray_letter_is_refused() {
        assert_eq!(detect("no Tthink ", START), Err(DriftReject::RightShape));
    }

    #[test]
    fn a_rare_half_that_merely_fits_is_refused_by_the_floor() {
        // `no tgrazing`: not/no clears the left-gain bar, and `grazing` IS a word —
        // but a rare one (≈2.5M, under HALF_MIN_FREQ), so the floor must refuse it.
        // Left-gain alone (a floor of 0) would have offered `not grazing`.
        assert_eq!(detect("no tgrazing ", START), Err(DriftReject::BelowFloor));
        assert!(Lexicon::shared().frequency("grazing") < HALF_MIN_FREQ);
        assert!(
            Lexicon::shared().frequency("not") as f64
                > BAR_START * Lexicon::shared().frequency("no") as f64
        );
    }

    #[test]
    fn left_gain_bar_is_load_bearing() {
        // not/no is ~2.8x. At the strict cap (4.0) the canonical case is refused;
        // at the floor (1.0) it passes — the gate really consults the ratio.
        assert_eq!(
            detect("no tthink ", Bar::Ratio(BAR_CAP)),
            Err(DriftReject::LeftGain)
        );
        assert!(detect("no tthink ", Bar::Ratio(BAR_FLOOR)).is_ok());
    }

    #[test]
    fn stand_down_silences_the_check() {
        assert_eq!(
            detect("no tthink ", Bar::StandDown),
            Err(DriftReject::StandDown)
        );
    }

    #[test]
    fn bar_starts_strict_then_follows_the_users_record() {
        let mut s = DriftStats::default();
        assert_eq!(s.bar(), Bar::Ratio(BAR_START), "no record → strict start");
        // 4 decided: not enough evidence to move yet.
        s.taken = 4;
        assert_eq!(s.bar(), Bar::Ratio(BAR_START));
        // 5 decided, 100% taken → relaxes to the floor.
        s.taken = 5;
        assert_eq!(s.bar(), Bar::Ratio(BAR_FLOOR));
        // 5 decided, 1 taken (20%) → tightens to the cap.
        s = DriftStats {
            shown: 5,
            taken: 1,
            ignored: 4,
        };
        assert_eq!(s.bar(), Bar::Ratio(BAR_CAP));
        // Middle take rate → stays at the start.
        s = DriftStats {
            shown: 6,
            taken: 3,
            ignored: 3,
        };
        assert_eq!(s.bar(), Bar::Ratio(BAR_START));
        // 10+ decided and a take rate under 15% → stand down.
        s = DriftStats {
            shown: 12,
            taken: 1,
            ignored: 11,
        };
        assert_eq!(s.bar(), Bar::StandDown);
    }

    #[test]
    fn stats_round_trip_and_corrupt_file_is_quarantined() {
        let dir = std::env::temp_dir().join(format!("tcd-drift-{:?}", std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(SPACE_DRIFT_FILE);
        assert_eq!(
            load_stats(&p),
            DriftStats::default(),
            "missing file = fresh"
        );
        let s = DriftStats {
            shown: 3,
            taken: 2,
            ignored: 1,
        };
        save_stats(&p, &s).unwrap();
        assert_eq!(load_stats(&p), s);
        std::fs::write(&p, b"{ not json").unwrap();
        assert_eq!(load_stats(&p), DriftStats::default());
        assert!(
            !p.exists(),
            "the bad file was moved aside, not left in place"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
