//! **Guesser accuracy scoreboard** (M3 accuracy-gated suggestion work, Phase 1).
//!
//! Measures how often the [`crate::guesser`]'s top guess matches the word the
//! user *actually* fixed to — **observe-only, nothing fires.** On each detected
//! self-correction the engine asks the guesser for its top guess of the typed
//! token using the model **as it stands before that correction is folded in**
//! (genuinely out-of-sample), then records the predicted-vs-actual outcome here.
//!
//! ## What it keeps
//!
//! * An [`OverallTally`] — lifetime tries / hits, the same split for the
//!   **within-guard** subset (edit distance ≤ [`crate::MAX_PATTERN_EDIT_DISTANCE`]
//!   / length diff ≤ [`crate::MAX_PATTERN_LENGTH_DIFF`], i.e. the population the
//!   word-pattern store and the offline benchmark run on), and a **τ-sweep**:
//!   fired / hits at each confidence cut-off in [`crate::guesser::TAU_BUCKETS`].
//!   The τ table is the artifact the *next* phase (gating suggestions on a
//!   confidence threshold) needs.
//! * Per-pattern [`PatternAccuracy`], keyed by the normalized typed token, plus
//!   the guesser's most recent prediction (word + confidence) for that pattern,
//!   so the Impact view can later show "predicted vs. your fix."
//!
//! ## What it is NOT
//!
//! Not a capture stage and not a learner the engine reads back: it never feeds
//! a correction, never touches the master gate, and is **fully additive** to
//! the existing stores (whose semantics are unchanged). Its own JSON file
//! (`~/.typeassist/guess_accuracy.json`), path supplied by the host so L4 stays
//! portable. Same atomic-write + persist-counter cadence as the word-pattern
//! store. Keys are raw normalized words, on-device only — consistent with
//! `word_patterns.json` (Principle #8: nothing leaves the device).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::guesser::{Guess, TAU_BUCKETS};
use crate::motor_map::Timestamp;

/// On-disk shape version. Bump alongside any change to [`GuessLedger`] so a
/// loader can refuse or migrate stale files.
pub const GUESS_LEDGER_VERSION: u32 = 1;

/// Save cadence: persist once this many scored corrections have accrued since
/// the last [`GuessLedger::save_to`]. Matches the word-pattern store — scoring
/// happens on the same (rare, edit-gated) `CorrectedToOther` events.
pub const GUESS_PERSIST_EVERY: u64 = 20;

/// One confidence operating point: of the guesses whose confidence cleared
/// `tau`, how many fired and how many were correct. `tau` is stored for
/// readability; the table is kept aligned to [`TAU_BUCKETS`] by index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TauBucket {
    pub tau: f64,
    pub fired: u64,
    pub hits: u64,
}

/// Lifetime accuracy across every scored self-correction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverallTally {
    /// Every scored self-correction (a `None` guess counts as a try with no
    /// hit, matching the offline LOO convention so the numbers are comparable).
    pub tries: u64,
    pub hits: u64,
    /// The subset whose `typed → target` is within the typo-fix guard — the
    /// population the word-pattern store learns and the offline benchmark runs
    /// on. Lets both populations be read from one file.
    pub within_guard_tries: u64,
    pub within_guard_hits: u64,
    /// Operating-point sweep, one entry per [`TAU_BUCKETS`] value.
    pub by_tau: Vec<TauBucket>,
}

impl Default for OverallTally {
    fn default() -> Self {
        Self {
            tries: 0,
            hits: 0,
            within_guard_tries: 0,
            within_guard_hits: 0,
            by_tau: TAU_BUCKETS
                .iter()
                .map(|&tau| TauBucket {
                    tau,
                    fired: 0,
                    hits: 0,
                })
                .collect(),
        }
    }
}

impl OverallTally {
    /// Re-seed the τ table if a loaded file predates a change to
    /// [`TAU_BUCKETS`] (length mismatch) — never silently score against a stale
    /// set of cut-offs.
    fn ensure_tau_shape(&mut self) {
        if self.by_tau.len() != TAU_BUCKETS.len() {
            self.by_tau = OverallTally::default().by_tau;
        }
    }
}

/// Per-pattern accuracy, keyed by the normalized typed token.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PatternAccuracy {
    pub tries: u64,
    pub hits: u64,
    pub within_guard_tries: u64,
    pub within_guard_hits: u64,
    /// The guesser's most recent top guess for this pattern (`None` if it had
    /// no real-word candidate that time) — for a future "predicted vs. fix".
    pub last_guess: Option<String>,
    /// Confidence (softmax-share) of [`Self::last_guess`]; 0.0 when none.
    pub last_confidence: f64,
    /// The word the user actually landed on, most recently.
    pub last_target: String,
    /// When this pattern was last scored (ms since epoch).
    pub last_ms: Timestamp,
}

/// The accuracy scoreboard. Owned by the engine; serialized to its own JSON
/// file. Observe-only — see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuessLedger {
    /// Shape version (see [`GUESS_LEDGER_VERSION`]).
    pub version: u32,
    overall: OverallTally,
    /// Keyed by normalized typed token.
    patterns: HashMap<String, PatternAccuracy>,
    /// Scored corrections since the last [`Self::save_to`]. Drives
    /// [`Self::persist_due`]; not persisted (a loaded ledger is "clean").
    #[serde(skip)]
    obs_since_persist: u64,
}

impl Default for GuessLedger {
    fn default() -> Self {
        Self {
            version: GUESS_LEDGER_VERSION,
            overall: OverallTally::default(),
            patterns: HashMap::new(),
            obs_since_persist: 0,
        }
    }
}

impl GuessLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lifetime tally across every scored self-correction.
    pub fn overall(&self) -> &OverallTally {
        &self.overall
    }

    /// Number of distinct typed patterns scored.
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// All per-pattern rows as `(typed, accuracy)`, strongest-tried first then
    /// alphabetical — for the observe-only diagnostic dump and (later) the
    /// Impact view. Borrowed; the caller decides log level (raw words → debug).
    pub fn rows(&self) -> Vec<(&str, &PatternAccuracy)> {
        let mut out: Vec<(&str, &PatternAccuracy)> =
            self.patterns.iter().map(|(k, v)| (k.as_str(), v)).collect();
        out.sort_by(|a, b| b.1.tries.cmp(&a.1.tries).then_with(|| a.0.cmp(b.0)));
        out
    }

    /// Record one predicted-vs-actual outcome.
    ///
    /// * `typed` — the typed token, **already normalized** (the caller uses
    ///   [`crate::word_pattern::normalize_word`] so this keys identically to the
    ///   word-pattern store).
    /// * `guess` — the guesser's verdict for `typed`, or `None` if it had no
    ///   real-word candidate.
    /// * `target` — the normalized word the user actually landed on.
    /// * `within_guard` — whether `typed → target` is within the typo-fix guard.
    /// * `now` — score time.
    pub fn record(
        &mut self,
        typed: &str,
        guess: Option<&Guess>,
        target: &str,
        within_guard: bool,
        now: Timestamp,
    ) {
        let hit = guess.map(|g| g.word == target).unwrap_or(false);
        let confidence = guess.map(|g| g.confidence).unwrap_or(0.0);

        // Overall.
        self.overall.tries += 1;
        self.overall.hits += u64::from(hit);
        if within_guard {
            self.overall.within_guard_tries += 1;
            self.overall.within_guard_hits += u64::from(hit);
        }
        // τ-sweep: a guess "fires" at every cut-off its confidence clears.
        self.overall.ensure_tau_shape();
        for bucket in &mut self.overall.by_tau {
            if confidence >= bucket.tau {
                bucket.fired += 1;
                bucket.hits += u64::from(hit);
            }
        }

        // Per-pattern.
        let row = self.patterns.entry(typed.to_string()).or_default();
        row.tries += 1;
        row.hits += u64::from(hit);
        if within_guard {
            row.within_guard_tries += 1;
            row.within_guard_hits += u64::from(hit);
        }
        row.last_guess = guess.map(|g| g.word.clone());
        row.last_confidence = confidence;
        row.last_target = target.to_string();
        row.last_ms = now;

        self.obs_since_persist += 1;
    }

    // --- Persistence (mirrors the word-pattern store) -----------------------

    /// Whether [`GUESS_PERSIST_EVERY`] outcomes have accrued since the last
    /// [`Self::save_to`].
    pub fn persist_due(&self) -> bool {
        self.obs_since_persist >= GUESS_PERSIST_EVERY
    }

    /// Whether any outcomes have been scored since the last save — i.e. the
    /// live file on disk is stale.
    pub fn has_unsaved(&self) -> bool {
        self.obs_since_persist > 0
    }

    /// Serialize to `path` atomically and reset the persist counter. Host owns
    /// the path. Unlike the word-pattern store there is no pruning: an accuracy
    /// row is a measurement, not a decayed learner — it must not be forgotten.
    pub fn save_to(&mut self, path: &Path) -> io::Result<()> {
        self.write_json(path)?;
        self.obs_since_persist = 0;
        Ok(())
    }

    fn write_json(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Load a ledger from `path`. A freshly loaded ledger starts "clean"
    /// (persist counter zero); the τ table is re-seeded if its shape predates a
    /// [`TAU_BUCKETS`] change.
    pub fn load_from(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        let mut ledger: GuessLedger = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        ledger.obs_since_persist = 0;
        ledger.overall.ensure_tau_shape();
        Ok(ledger)
    }
}

// Sanity at compile time.
const _: () = assert!(GUESS_LEDGER_VERSION >= 1);

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Timestamp = 1_000_000;

    fn g(word: &str, confidence: f64) -> Guess {
        Guess {
            word: word.to_string(),
            confidence,
            offset_only: false,
        }
    }

    #[test]
    fn hit_and_miss_tally_overall_and_per_pattern() {
        let mut l = GuessLedger::new();
        // Correct guess, within guard, confident.
        l.record("teh", Some(&g("the", 0.9)), "the", true, T0);
        // Wrong guess, within guard, less confident.
        l.record("wprd", Some(&g("ward", 0.55)), "word", true, T0);

        assert_eq!(l.overall().tries, 2);
        assert_eq!(l.overall().hits, 1);
        assert_eq!(l.overall().within_guard_tries, 2);
        assert_eq!(l.overall().within_guard_hits, 1);

        let rows: HashMap<&str, &PatternAccuracy> = l.rows().into_iter().collect();
        assert_eq!(rows["teh"].hits, 1);
        assert_eq!(rows["wprd"].hits, 0);
        assert_eq!(rows["wprd"].last_guess.as_deref(), Some("ward"));
        assert_eq!(rows["wprd"].last_target, "word");
    }

    #[test]
    fn none_guess_is_a_try_but_never_a_hit() {
        let mut l = GuessLedger::new();
        l.record("xqz", None, "the", true, T0);
        assert_eq!(l.overall().tries, 1);
        assert_eq!(l.overall().hits, 0);
        for bucket in &l.overall().by_tau {
            assert_eq!(bucket.fired, 0, "a None guess fires at no τ");
        }
    }

    #[test]
    fn tau_sweep_fires_only_above_cutoff() {
        let mut l = GuessLedger::new();
        // confidence 0.65 → fires at τ=0.5,0.6 (hit); not 0.7/0.8/0.9.
        l.record("teh", Some(&g("the", 0.65)), "the", true, T0);
        for bucket in &l.overall().by_tau {
            if bucket.tau <= 0.65 {
                assert_eq!(bucket.fired, 1);
                assert_eq!(bucket.hits, 1);
            } else {
                assert_eq!(bucket.fired, 0);
            }
        }
    }

    #[test]
    fn outside_guard_counts_overall_not_within_guard() {
        let mut l = GuessLedger::new();
        l.record("cat", Some(&g("cot", 0.8)), "elephant", false, T0);
        assert_eq!(l.overall().tries, 1);
        assert_eq!(l.overall().within_guard_tries, 0);
    }

    #[test]
    fn save_and_load_round_trips() {
        let dir = std::env::temp_dir().join(format!("ta_guess_ledger_{}", std::process::id()));
        let path = dir.join("guess_accuracy.json");
        let mut l = GuessLedger::new();
        l.record("teh", Some(&g("the", 0.9)), "the", true, T0);
        l.save_to(&path).unwrap();
        assert!(!l.has_unsaved());

        let loaded = GuessLedger::load_from(&path).unwrap();
        assert_eq!(loaded.overall().tries, 1);
        assert_eq!(loaded.overall().hits, 1);
        assert_eq!(loaded.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
