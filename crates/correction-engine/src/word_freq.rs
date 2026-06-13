//! Component 5e — **local vocabulary tally** (observe-only).
//!
//! A privacy-gated, on-device count of the words this user types *correctly*
//! and leaves in place. Its purpose is a future experiment: rank correction
//! candidates by the user's **personal** word frequency instead of a global
//! web-frequency table (see `examples/backtest_motor_map.rs`). Until that
//! backtest runs and a kill-switch is flipped, this module only **observes and
//! stores** — nothing here feeds a correction back into the engine.
//!
//! ## What it stores — and what it deliberately does NOT
//!
//! A flat `HashMap<String, f32>` of `word → count`. **Counts only.** No
//! sentences, no word order, no surrounding context, no timestamps per word,
//! no co-occurrence. The store cannot reconstruct anything the user wrote; it
//! is a personal unigram histogram, nothing more.
//!
//! ## Privacy gate (load-bearing)
//!
//! A word is tallied **only if [`Lexicon::is_known`]** — i.e. it is in the
//! bundled ~81k clean dictionary **or** the user's learned-and-confirmed
//! vocabulary. This makes the store *structurally incapable* of holding a
//! password, a name, an ID, a URL fragment, or any out-of-dictionary string:
//! such tokens never pass the gate, so they are never written to disk. The
//! gate is the privacy contract, not a heuristic — keep it on every write
//! path. (This is also exactly the universe correction candidates are drawn
//! from, so nothing useful for ranking is lost.)
//!
//! ## Normalization
//!
//! Reuses the motor map / word-pattern [`crate::normalize_word`] (lowercase +
//! ASCII-graphic), then trims **surrounding** ASCII punctuation so a
//! sentence-final `word.` or a quoted `'word'` folds onto `word`. Interior
//! punctuation is kept, so contractions (`don't`) survive — and stay tallied
//! only because they are `is_known`.
//!
//! ## No decay (v0)
//!
//! Unlike the motor map, the tally does **not** decay. Vocabulary is far more
//! stationary than motor skill, and a frequency *prior* wants the user's
//! long-run usage, not a 30-day window. Revisit if the backtest shows recency
//! matters.
//!
//! ## Persistence
//!
//! Path-agnostic (L4 stays portable; the host supplies `~/.typeassist/...`).
//! [`WordFreq::save_to`] writes the live file durably (temp → fsync → rename);
//! [`WordFreq::write_snapshot`] is the same write without resetting the dirty
//! flag, for the dated daily archive (Principle #6). A corrupt file is
//! quarantined aside (never wiped) and the tally comes up empty rather than
//! erroring engine startup (Principle #6).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::lexicon::Lexicon;
use crate::normalize_word;

/// Wall-clock timestamp, milliseconds since the UNIX epoch. Matches the
/// engine's `now_ms()` and the motor map's clock.
pub type Timestamp = u64;

/// On-disk shape version. Bump alongside any change to [`WordFreq`].
pub const WORD_FREQ_VERSION: u32 = 1;

/// The local vocabulary tally. Owned by the engine; serialized to JSON.
///
/// On-disk shape is exactly `{ version, counts: { word: f32 }, last_update }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordFreq {
    /// Shape version of this store (see [`WORD_FREQ_VERSION`]).
    pub version: u32,
    /// `word → count`. Counts are `f32` (not `u64`) so a future decay or
    /// fractional weighting can be added without a format change.
    counts: HashMap<String, f32>,
    /// When the tally was last updated (ms since epoch). Reflective only.
    last_update: Timestamp,
    /// Observations since the last [`Self::save_to`]. Drives [`Self::has_unsaved`];
    /// not persisted (a freshly loaded tally is "clean").
    #[serde(skip)]
    obs_since_persist: u64,
}

impl Default for WordFreq {
    fn default() -> Self {
        Self {
            version: WORD_FREQ_VERSION,
            counts: HashMap::new(),
            last_update: 0,
            obs_since_persist: 0,
        }
    }
}

impl WordFreq {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct words tallied.
    pub fn len(&self) -> usize {
        self.counts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// Total of all word counts (token count). Reflective; for logging only.
    pub fn total_count(&self) -> f64 {
        self.counts.values().map(|&c| c as f64).sum()
    }

    /// When the tally was last updated.
    pub fn last_update(&self) -> Timestamp {
        self.last_update
    }

    /// Tally one **Kept** (clean, left-in) word.
    ///
    /// PRIVACY GATE: the word is counted **only if [`Lexicon::is_known`]** after
    /// normalization. Out-of-dictionary tokens (names, passwords, IDs, junk) are
    /// silently ignored — they never touch the store. Returns `true` iff the
    /// word passed the gate and was counted.
    ///
    /// Call this **only** for `Outcome::Kept` words — never corrected, never
    /// abandoned. A correction means the typed form was a slip, not vocabulary.
    pub fn observe_kept(&mut self, word: &str, lexicon: &Lexicon, now: Timestamp) -> bool {
        let w = normalize_for_tally(word);
        if w.is_empty() || !lexicon.is_known(&w) {
            return false;
        }
        *self.counts.entry(w).or_insert(0.0) += 1.0;
        self.last_update = now.max(self.last_update);
        self.obs_since_persist += 1;
        true
    }

    /// This user's personal count for `word` (normalized the same way as
    /// [`Self::observe_kept`]); `0.0` if never tallied. The future personal-
    /// frequency scorer reads this.
    pub fn frequency(&self, word: &str) -> f32 {
        let w = normalize_for_tally(word);
        self.counts.get(&w).copied().unwrap_or(0.0)
    }

    /// Whether any observations have accrued since the last [`Self::save_to`]
    /// — i.e. the live file on disk is stale. The host's periodic flush polls
    /// this so the live tally is maintained on a time cadence.
    pub fn has_unsaved(&self) -> bool {
        self.obs_since_persist > 0
    }

    /// Serialize to `path` durably (temp → fsync → rename), creating parent
    /// directories as needed, and reset the dirty flag.
    pub fn save_to(&mut self, path: &Path) -> io::Result<()> {
        self.write_json(path)?;
        self.obs_since_persist = 0;
        Ok(())
    }

    /// Write a dated snapshot (the daily archive) without resetting the dirty
    /// flag. Same durable write as [`Self::save_to`].
    pub fn write_snapshot(&self, path: &Path) -> io::Result<()> {
        self.write_json(path)?;
        tracing::info!(target: "word_freq", "WORD_FREQ_SNAPSHOT_WRITTEN path={:?}", path);
        Ok(())
    }

    fn write_json(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        crate::persist::durable_write(path, json.as_bytes())
    }

    /// Load a tally from `path`. A freshly loaded tally starts "clean". A
    /// **corrupt** file is quarantined aside (never wiped) and the tally comes
    /// up empty rather than erroring startup (Principle #6); a missing file
    /// still propagates `NotFound` for the caller's `path.exists()` guard.
    pub fn load_from(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        match serde_json::from_slice::<WordFreq>(&bytes) {
            Ok(mut store) => {
                store.obs_since_persist = 0;
                Ok(store)
            }
            Err(e) => {
                crate::persist::quarantine_corrupt(path, e);
                Ok(Self::new())
            }
        }
    }
}

/// Lowercase + ASCII-graphic (reusing [`normalize_word`]), then trim
/// **surrounding** ASCII punctuation. Interior punctuation is preserved so
/// contractions survive; the `is_known` gate decides whether the result counts.
fn normalize_for_tally(s: &str) -> String {
    let lowered = normalize_word(s);
    lowered
        .trim_matches(|c: char| c.is_ascii_punctuation())
        .to_string()
}

// ---- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Timestamp = 1_700_000_000_000;

    fn lex() -> &'static Lexicon {
        Lexicon::shared()
    }

    #[test]
    fn counts_known_words_and_rejects_unknown() {
        let mut wf = WordFreq::new();
        // "the" / "word" / "cat" are in the bundled clean dict.
        assert!(wf.observe_kept("the", lex(), T0));
        assert!(wf.observe_kept("the", lex(), T0));
        assert!(wf.observe_kept("word", lex(), T0));
        // Out-of-dictionary junk / would-be secret: NEVER tallied.
        assert!(!wf.observe_kept("asdfqwerty", lex(), T0));
        assert!(!wf.observe_kept("hunter2pass", lex(), T0));

        assert_eq!(wf.frequency("the"), 2.0);
        assert_eq!(wf.frequency("word"), 1.0);
        assert_eq!(wf.frequency("asdfqwerty"), 0.0);
        // Only the two distinct known words were stored.
        assert_eq!(wf.len(), 2);
    }

    #[test]
    fn privacy_gate_blocks_out_of_dictionary_strings() {
        let mut wf = WordFreq::new();
        // A plausible name, an ID, an email-ish fragment — all out-of-dict.
        for junk in ["zzyzx", "user1234", "k8s-prod-07", "a1b2c3"] {
            assert!(!wf.observe_kept(junk, lex(), T0), "{junk} must be blocked");
        }
        assert!(wf.is_empty(), "store must be structurally empty of non-words");
    }

    #[test]
    fn normalization_folds_case_and_surrounding_punctuation() {
        let mut wf = WordFreq::new();
        wf.observe_kept("The", lex(), T0); // case
        wf.observe_kept("the.", lex(), T0); // trailing punctuation
        wf.observe_kept("'the'", lex(), T0); // surrounding quotes
        wf.observe_kept("the", lex(), T0);
        // All four fold onto one entry.
        assert_eq!(wf.len(), 1);
        assert_eq!(wf.frequency("the"), 4.0);
    }

    #[test]
    fn contraction_with_interior_apostrophe_is_kept() {
        let mut wf = WordFreq::new();
        // "don't" is in the clean dict (contractions included); the interior
        // apostrophe must NOT be stripped.
        assert!(wf.observe_kept("don't", lex(), T0));
        assert_eq!(wf.frequency("don't"), 1.0);
    }

    #[test]
    fn has_unsaved_tracks_dirty_state_across_save() {
        let mut wf = WordFreq::new();
        assert!(!wf.has_unsaved(), "fresh tally is clean");
        wf.observe_kept("cat", lex(), T0);
        assert!(wf.has_unsaved());

        let path = std::env::temp_dir().join("typeassist_word_freq_test_unsaved.json");
        wf.save_to(&path).unwrap();
        assert!(!wf.has_unsaved(), "save clears the dirty flag");
        let _ = fs::remove_file(&path);

        // A rejected (out-of-dict) word must NOT dirty the store.
        assert!(!wf.observe_kept("zzyzx", lex(), T0));
        assert!(!wf.has_unsaved());
    }

    #[test]
    fn json_round_trip_preserves_counts() {
        let mut wf = WordFreq::new();
        wf.observe_kept("cat", lex(), T0);
        wf.observe_kept("cat", lex(), T0);
        wf.observe_kept("word", lex(), T0);

        let path = std::env::temp_dir().join("typeassist_word_freq_test_roundtrip.json");
        wf.save_to(&path).unwrap();
        let loaded = WordFreq::load_from(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(loaded.version, WORD_FREQ_VERSION);
        assert_eq!(loaded.frequency("cat"), 2.0);
        assert_eq!(loaded.frequency("word"), 1.0);
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn corrupt_file_is_quarantined_and_loads_empty() {
        let path = std::env::temp_dir()
            .join(format!("ta_word_freq_corrupt_{}.json", std::process::id()));
        fs::write(&path, b"{ not valid word_freq json").unwrap();

        let wf = WordFreq::load_from(&path).expect("corrupt load must not error");
        assert!(wf.is_empty(), "comes up empty after quarantine");
        assert!(!path.exists(), "corrupt file was moved aside");

        // Clean up the quarantine sibling.
        let dir = path.parent().unwrap();
        for e in fs::read_dir(dir).unwrap().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with(&format!("ta_word_freq_corrupt_{}", std::process::id()))
                && n.contains(".corrupt-")
            {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}
