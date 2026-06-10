//! Component 5d — **Word-pattern store** (M3 Phase 1).
//!
//! The motor map ([`crate::motor_map`]) learns the user's hand at the level
//! of *keys* — which character this finger slips and what it lands on. This
//! module learns one level up: the *word* the hand keeps producing wrong and
//! the word it meant. Where the motor map answers "how reliable is the `e`
//! key," this answers "every time they type `teh` they fix it to `the`" —
//! the unit M3's per-pattern kill-switch decides on (CLAUDE.md → M3 decision
//! 2: **pattern unit = word-level**).
//!
//! ## What it consumes
//!
//! Only [`Outcome::CorrectedToOther`] resolutions — a word typed, then fixed
//! to something else. That `typed → corrected` pair *is* the pattern. A
//! [`Outcome::Kept`] word forms no pair (nothing was corrected), so unlike
//! the motor map this store ignores `Kept` entirely; it is fed from the same
//! resolver loop, in the same arm where the host already recovers the
//! post-edit text ([`crate::resolver::post_edit_text`]).
//!
//! Like the motor map, a `CorrectedToOther` is recorded **only when it looks
//! like a typo fix** — small edit distance and similar length
//! ([`MAX_PATTERN_EDIT_DISTANCE`] / [`MAX_PATTERN_LENGTH_DIFF`]). A semantic
//! rewrite (`cat → elephant`) is not a pattern we would ever auto-fix toward,
//! so it is skipped entirely rather than stored as noise.
//!
//! ## Shape
//!
//! A list of [`PatternStat`], each a `typed → target` pair with a **decayed
//! occurrence weight** (same 30-day half-life as the motor map, reusing
//! [`crate::motor_map::HALF_LIFE_MS`] so the two layers forget in lockstep)
//! and the **consecutive-undo counter** that drives the 3-strike safety brake
//! (CLAUDE.md → M3: 3 undos in a row demote Tier-1 → Tier-2 until the pattern
//! rebuilds). Words are normalized lowercase, so `Teh→The` and `teh→the` fold
//! into one pattern.
//!
//! ## What it does NOT do (Phase 1)
//!
//! This store **observes and stores only**. It holds no [`crate::Lexicon`]
//! reference and makes no decision: the 4-gate Tier-1/Tier-2 classifier reads
//! this store but lives separately (`kill_switch`, the next slice), and
//! nothing feeds a correction back into the engine until Phase 2. The
//! **per-pattern kill-switch stays OFF.** [`WordPatternStore::note_undo`] /
//! [`WordPatternStore::note_accept`] exist and are tested now so the brake is
//! ready, but the host won't call them until corrections actually fire.
//!
//! ## Persistence
//!
//! Its **own** JSON file (`~/.typeassist/word_patterns.json`), separate from
//! the motor ledger and motor map — path supplied by the host so L4 stays
//! portable. Same atomic write + persist-counter cadence as the motor map.
//! Low-weight patterns are pruned on save so a long-lived store can't grow an
//! unbounded tail of one-off corrections that have decayed to nothing.
//!
//! ## Conscious v0 simplifications (intended behaviour, not bugs)
//!
//! 1. **Reads decay against [`WordPatternStore::last_now`]**, not wall-clock —
//!    same choice (and rationale) as the motor map: a query long after the
//!    last keystroke shows slightly stale weights. Fine live.
//! 2. **Linear scan over patterns** on observe/lookup. A user's distinct
//!    typo→target pairs number in the low hundreds, observes are rare
//!    (edit-gated corrections only), so a `HashMap` index isn't worth its
//!    serialization complexity yet. Revisit if a store ever grows large.
//! 3. **Undo counter is not decayed.** Three undos demote regardless of how
//!    far apart; a successful keep ([`note_accept`]) is what resets it. The
//!    brake is about *recent consecutive* rejection, and [`note_accept`]
//!    already breaks the streak — a time decay would only blur that signal.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::motor_map::{Timestamp, HALF_LIFE_MS};
use crate::Outcome;

/// On-disk shape version. Bump alongside any change to [`WordPatternStore`] /
/// [`PatternStat`] so a loader can refuse or migrate stale files.
pub const WORD_PATTERN_VERSION: u32 = 1;

/// Save cadence: the host should persist once this many corrections have been
/// folded in since the last [`WordPatternStore::save_to`]. Lower than the
/// motor map's 100 because word patterns accrue far more slowly (only
/// edit-gated `CorrectedToOther`, not every kept character). **Tunable.**
pub const PATTERN_PERSIST_EVERY: u64 = 20;

/// **EASILY-FLIPPED CONSTANT.** Decayed occurrence weight a pattern must reach
/// before it is even *eligible* to be Tier-1-ready (the classifier applies the
/// remaining lexicon gates on top). The brief says "≈10–15 recent
/// decay-adjusted observations"; 12 is the conservative midpoint. Tune from
/// real data once the test suite is green.
pub const TIER1_MIN_OBSERVATIONS: f32 = 12.0;

/// Consecutive user-undos of the same pattern that trip the safety brake
/// (CLAUDE.md → M3: demote Tier-1 → Tier-2 until it rebuilds). Reset by
/// [`WordPatternStore::note_accept`].
pub const UNDO_BRAKE_STRIKES: u32 = 3;

/// Edit-distance guard for recording a correction as a pattern. A
/// `CorrectedToOther` is stored only when `typed → target` is `≤ this`
/// Levenshtein distance AND the length difference is `≤
/// MAX_PATTERN_LENGTH_DIFF`; larger edits are semantic rewrites, not typo
/// fixes, and are skipped entirely. Mirrors the motor map's slip guard.
pub const MAX_PATTERN_EDIT_DISTANCE: usize = 2;
/// Companion to [`MAX_PATTERN_EDIT_DISTANCE`]: max `|typed - target|` length
/// difference for a correction to count as a typo fix.
pub const MAX_PATTERN_LENGTH_DIFF: usize = 1;

/// Decayed weights below this are pruned on save, so a long-lived store
/// doesn't accumulate a tail of effectively-zero one-off corrections.
const PRUNE_EPSILON: f32 = 1.0e-4;

/// What a single [`WordPatternStore::observe_correction`] recorded, so the
/// host can log it and (later) reconcile a capture counter (Principle #7). A
/// skipped correction (rewrite / no-op / empty) returns `recorded: false`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PatternObserveReport {
    /// Whether the correction was folded into the store (vs filtered out).
    pub recorded: bool,
    /// The pattern's decayed weight *after* recording (0.0 if not recorded).
    pub weight: f32,
}

/// One learned `typed → target` correction pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternStat {
    /// The word as the user types it (normalized lowercase), e.g. `teh`.
    pub typed: String,
    /// The word they correct it to (normalized lowercase), e.g. `the`.
    pub target: String,
    /// Decayed count of times this correction has been observed.
    count: f32,
    /// When `count` was last decayed (and so the reference point for the next
    /// lazy decay). Milliseconds since epoch.
    last_update: Timestamp,
    /// Consecutive user-undos of this pattern's Tier-1 fix, for the safety
    /// brake. Bumped by [`WordPatternStore::note_undo`], reset to 0 by
    /// [`WordPatternStore::note_accept`]. Not decayed (see module note 3).
    consecutive_undos: u32,
}

impl PatternStat {
    fn new(typed: String, target: String, now: Timestamp) -> Self {
        Self {
            typed,
            target,
            count: 0.0,
            last_update: now,
            consecutive_undos: 0,
        }
    }

    /// Decay factor for the elapsed time since `last_update`. `0.5` per
    /// half-life; clamped to `1.0` if `now` runs backwards so time never
    /// *adds* weight. Mirrors [`crate::motor_map`]'s lazy decay.
    fn decay_factor(&self, now: Timestamp) -> f32 {
        let elapsed = now.saturating_sub(self.last_update) as f32;
        if elapsed <= 0.0 {
            return 1.0;
        }
        0.5_f32.powf(elapsed / HALF_LIFE_MS)
    }

    /// Apply decay in place and advance `last_update` to `now`. Idempotent
    /// within the same `now`.
    fn decay_in_place(&mut self, now: Timestamp) {
        let f = self.decay_factor(now);
        if f < 1.0 {
            self.count *= f;
        }
        self.last_update = now;
    }

    /// Decayed weight as of `now`, without mutating.
    fn decayed_count(&self, now: Timestamp) -> f32 {
        self.count * self.decay_factor(now)
    }
}

/// A read-only view of one pattern, decayed to the store's [`last_now`]. The
/// classifier (`kill_switch`) reads these; nothing here makes a decision.
///
/// [`last_now`]: WordPatternStore::last_now
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternSnapshot {
    pub typed: String,
    pub target: String,
    /// Decayed occurrence weight as of the store's last observation time.
    pub weight: f32,
    /// Consecutive-undo count (≥ [`UNDO_BRAKE_STRIKES`] means the brake is
    /// tripped — the classifier demotes Tier-1 → Tier-2).
    pub consecutive_undos: u32,
    /// Last time this pattern was observed (ms since epoch). The classifier's
    /// decay-window gate compares this against the read time.
    pub last_update: Timestamp,
}

/// The word-pattern store. Owned by the engine; serialized to JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordPatternStore {
    /// Shape version of this store (see [`WORD_PATTERN_VERSION`]).
    pub version: u32,
    /// Learned patterns. Linear-scanned (see module note 2).
    patterns: Vec<PatternStat>,
    /// Most recent observation time. Reads decay relative to this so a query
    /// reflects forgetting up to the last activity without a `now` of its own.
    last_now: Timestamp,
    /// Lifetime corrections folded in. Reflective; never a target.
    total_observations: u64,
    /// Corrections since the last [`Self::save_to`]. Drives
    /// [`Self::persist_due`]; not persisted (a loaded store is "clean").
    #[serde(skip)]
    obs_since_persist: u64,
}

impl Default for WordPatternStore {
    fn default() -> Self {
        Self {
            version: WORD_PATTERN_VERSION,
            patterns: Vec::new(),
            last_now: 0,
            total_observations: 0,
            obs_since_persist: 0,
        }
    }
}

impl WordPatternStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct `typed → target` patterns held.
    pub fn len(&self) -> usize {
        self.patterns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    /// Lifetime corrections folded in. Reflective; never a goal.
    pub fn total_observations(&self) -> u64 {
        self.total_observations
    }

    /// The store's most recent observation time — the reference point reads
    /// decay against.
    pub fn last_now(&self) -> Timestamp {
        self.last_now
    }

    /// Fold one `CorrectedToOther` resolution into the store.
    ///
    /// * `typed` — the word as originally typed (the resolver record's
    ///   `original_text`).
    /// * `corrected` — the word the user landed on
    ///   ([`crate::resolver::post_edit_text`]).
    /// * `now` — resolution time, used for lazy decay and as the new read
    ///   reference point.
    ///
    /// Records only when the pair looks like a typo fix (edit distance ≤
    /// [`MAX_PATTERN_EDIT_DISTANCE`], length diff ≤ [`MAX_PATTERN_LENGTH_DIFF`])
    /// and isn't a no-op after normalization. Returns a [`PatternObserveReport`].
    ///
    /// Intended to be called from the host's `CorrectedToOther` arm only — a
    /// [`Outcome::Kept`] forms no pattern. The `outcome` is accepted so the
    /// call site reads symmetrically with [`crate::motor_map::MotorMap::observe_outcome`]
    /// and a stray non-`CorrectedToOther` is a safe no-op.
    pub fn observe_correction(
        &mut self,
        outcome: Outcome,
        typed: &str,
        corrected: &str,
        now: Timestamp,
    ) -> PatternObserveReport {
        // Advance the read clock for every outcome handed in (keeps
        // decay-on-read fresh); guard a backwards clock so last_now is
        // monotonic.
        self.last_now = now.max(self.last_now);

        if outcome != Outcome::CorrectedToOther {
            return PatternObserveReport::default();
        }

        let typed = normalize_word(typed);
        let target = normalize_word(corrected);

        // No-op / empty: nothing to learn (a pure case fix folds away here).
        if typed.is_empty() || target.is_empty() || typed == target {
            return PatternObserveReport::default();
        }

        // Typo-fix guard — same shape as the motor map's slip guard.
        let distance = edit_distance(&typed, &target);
        let length_diff = typed.chars().count().abs_diff(target.chars().count());
        if distance > MAX_PATTERN_EDIT_DISTANCE || length_diff > MAX_PATTERN_LENGTH_DIFF {
            // debug!, not info!: prints raw typed + target words (privacy).
            tracing::debug!(
                target: "word_pattern",
                "WORD_PATTERN_SKIP_REWRITE typed={:?} target={:?} distance={} length_diff={}",
                typed, target, distance, length_diff
            );
            return PatternObserveReport::default();
        }

        let stat = self.entry_mut(&typed, &target, now);
        stat.count += 1.0;
        let weight = stat.count;

        self.total_observations += 1;
        self.obs_since_persist += 1;
        // debug!, not info!: prints raw typed + target words (privacy).
        tracing::debug!(
            target: "word_pattern",
            "WORD_PATTERN_OBSERVE typed={:?} target={:?} weight={:.2}",
            typed, target, weight
        );
        PatternObserveReport {
            recorded: true,
            weight,
        }
    }

    /// Record that the user **undid** a Tier-1 fix for `typed → target` —
    /// drives the 3-strike safety brake. Bumps the consecutive-undo counter;
    /// the classifier demotes the pattern once it reaches
    /// [`UNDO_BRAKE_STRIKES`]. No-op if the pattern isn't known. Returns the
    /// new consecutive-undo count.
    ///
    /// Phase 2 hook — observe-only Phase 1 never calls this, but it's built
    /// and tested now so the brake is ready when corrections fire.
    pub fn note_undo(&mut self, typed: &str, target: &str) -> u32 {
        let typed = normalize_word(typed);
        let target = normalize_word(target);
        match self.find_mut(&typed, &target) {
            Some(stat) => {
                stat.consecutive_undos = stat.consecutive_undos.saturating_add(1);
                let n = stat.consecutive_undos;
                // debug!, not info!: prints raw typed + target words (privacy).
                tracing::debug!(
                    target: "word_pattern",
                    "WORD_PATTERN_UNDO typed={:?} target={:?} consecutive={}",
                    typed, target, n
                );
                n
            }
            None => 0,
        }
    }

    /// Record that the user **kept** a Tier-1 fix for `typed → target` — the
    /// streak of rejections is broken, so reset the brake counter. No-op if
    /// the pattern isn't known. Phase 2 hook (see [`Self::note_undo`]).
    pub fn note_accept(&mut self, typed: &str, target: &str) {
        let typed = normalize_word(typed);
        let target = normalize_word(target);
        if let Some(stat) = self.find_mut(&typed, &target) {
            stat.consecutive_undos = 0;
        }
    }

    /// Read one pattern's view, decayed to [`Self::last_now`]. `None` if the
    /// pattern has never been observed. The classifier's entry point.
    pub fn snapshot(&self, typed: &str, target: &str) -> Option<PatternSnapshot> {
        let typed = normalize_word(typed);
        let target = normalize_word(target);
        self.find(&typed, &target).map(|stat| self.view(stat))
    }

    /// All patterns as views, decayed to [`Self::last_now`], strongest first.
    /// For diagnostics and a classifier sweep over everything learned.
    pub fn snapshots(&self) -> Vec<PatternSnapshot> {
        let mut out: Vec<PatternSnapshot> = self.patterns.iter().map(|s| self.view(s)).collect();
        out.sort_by(|a, b| {
            b.weight
                .partial_cmp(&a.weight)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.typed.cmp(&b.typed))
                .then_with(|| a.target.cmp(&b.target))
        });
        out
    }

    fn view(&self, stat: &PatternStat) -> PatternSnapshot {
        PatternSnapshot {
            typed: stat.typed.clone(),
            target: stat.target.clone(),
            weight: stat.decayed_count(self.last_now),
            consecutive_undos: stat.consecutive_undos,
            last_update: stat.last_update,
        }
    }

    fn find(&self, typed: &str, target: &str) -> Option<&PatternStat> {
        self.patterns
            .iter()
            .find(|s| s.typed == typed && s.target == target)
    }

    fn find_mut(&mut self, typed: &str, target: &str) -> Option<&mut PatternStat> {
        self.patterns
            .iter_mut()
            .find(|s| s.typed == typed && s.target == target)
    }

    /// Find the pattern (decaying it in place to `now`) or create it.
    fn entry_mut(&mut self, typed: &str, target: &str, now: Timestamp) -> &mut PatternStat {
        let idx = self
            .patterns
            .iter()
            .position(|s| s.typed == typed && s.target == target);
        let i = match idx {
            Some(i) => i,
            None => {
                self.patterns
                    .push(PatternStat::new(typed.to_string(), target.to_string(), now));
                self.patterns.len() - 1
            }
        };
        let stat = &mut self.patterns[i];
        stat.decay_in_place(now);
        stat
    }

    // --- Persistence (mirrors motor_map) ------------------------------------

    /// Whether [`PATTERN_PERSIST_EVERY`] corrections have accrued since the
    /// last [`Self::save_to`]. The host polls this after each observe.
    pub fn persist_due(&self) -> bool {
        self.obs_since_persist >= PATTERN_PERSIST_EVERY
    }

    /// Whether any corrections have been folded in since the last save — i.e.
    /// the live file on disk is stale. The host's periodic flush polls this.
    pub fn has_unsaved(&self) -> bool {
        self.obs_since_persist > 0
    }

    /// Serialize to `path` atomically, pruning decayed-to-nothing patterns
    /// first, and reset the persist counter. Host owns the path.
    pub fn save_to(&mut self, path: &Path) -> io::Result<()> {
        self.prune();
        self.write_json(path)?;
        self.obs_since_persist = 0;
        Ok(())
    }

    /// Write a dated snapshot (the daily archive) without pruning or resetting
    /// the persist counter — the durable history must preserve as-captured
    /// state (Principle #6). Same atomic write as [`Self::save_to`].
    pub fn write_snapshot(&self, path: &Path) -> io::Result<()> {
        self.write_json(path)?;
        tracing::info!(target: "word_pattern", "WORD_PATTERN_SNAPSHOT_WRITTEN path={:?}", path);
        Ok(())
    }

    /// Drop patterns whose decayed weight (as of [`Self::last_now`]) has
    /// fallen below [`PRUNE_EPSILON`] — the intended forgetting policy.
    fn prune(&mut self) {
        let now = self.last_now;
        self.patterns
            .retain(|s| s.decayed_count(now) >= PRUNE_EPSILON);
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

    /// Load a store from `path`. A freshly loaded store starts "clean"
    /// (persist counter zero). A future version bump can migrate here.
    pub fn load_from(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        let mut store: WordPatternStore =
            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        store.obs_since_persist = 0;
        Ok(store)
    }
}

// --- Helpers ----------------------------------------------------------------

/// Lowercase + keep only observable characters (printable ASCII). Matches the
/// motor map's normalization so `Teh→The` and `teh→the` fold into one pattern.
///
/// `pub` so the observe-only guesser scoreboard ([`crate::guess_ledger`]) keys
/// its per-pattern accuracy by the *same* normalized form this store uses —
/// otherwise `Teh` and `teh` would split into two scoreboard rows but one
/// pattern row. Read-only helper; does not touch store state or semantics.
pub fn normalize_word(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            let c = c.to_ascii_lowercase();
            c.is_ascii_graphic().then_some(c)
        })
        .collect()
}

/// Levenshtein distance (unit costs) between two normalized words. `pub` so the
/// observe-only guesser scoreboard ([`crate::guess_ledger`]) can tag whether a
/// scored `typed → target` pair falls within the typo-fix guard, using the same
/// metric this store's capture guard uses.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    // Two-row DP (we only need the distance, not the backtrace).
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let sub_cost = usize::from(a[i - 1] != b[j - 1]);
            curr[j] = (prev[j - 1] + sub_cost)
                .min(prev[j] + 1)
                .min(curr[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

// Sanity at compile time.
const _: () = assert!(WORD_PATTERN_VERSION >= 1);

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Timestamp = 1_000_000;
    // One 30-day half-life in ms, as a Timestamp delta.
    const HALF_LIFE: Timestamp = 30u64 * 24 * 60 * 60 * 1000;

    fn observe_n(store: &mut WordPatternStore, typed: &str, target: &str, n: u32, now: Timestamp) {
        for _ in 0..n {
            store.observe_correction(Outcome::CorrectedToOther, typed, target, now);
        }
    }

    // ---- Recording ------------------------------------------------------

    #[test]
    fn observe_records_a_pattern_and_increments_weight() {
        let mut store = WordPatternStore::new();
        let r = store.observe_correction(Outcome::CorrectedToOther, "teh", "the", T0);
        assert!(r.recorded);
        assert!((r.weight - 1.0).abs() < 1e-6);
        assert_eq!(store.len(), 1);
        assert_eq!(store.total_observations(), 1);

        let r2 = store.observe_correction(Outcome::CorrectedToOther, "teh", "the", T0);
        assert!((r2.weight - 2.0).abs() < 1e-6);
        // Same pair → no new distinct pattern.
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn only_corrected_to_other_is_recorded() {
        let mut store = WordPatternStore::new();
        for outcome in [
            Outcome::Kept,
            Outcome::CorrectedToSuggestion,
            Outcome::Abandoned,
            Outcome::Pending,
        ] {
            let r = store.observe_correction(outcome, "teh", "the", T0);
            assert!(!r.recorded, "{outcome:?} must not record a word pattern");
        }
        assert!(store.is_empty());
        // …but last_now still advances for clock freshness.
        assert_eq!(store.last_now(), T0);
    }

    #[test]
    fn semantic_rewrite_is_skipped() {
        let mut store = WordPatternStore::new();
        // distance 7, length diff 5 — well past the typo-fix guard.
        let r = store.observe_correction(Outcome::CorrectedToOther, "cat", "elephant", T0);
        assert!(!r.recorded);
        assert!(store.is_empty());
    }

    #[test]
    fn distance_two_within_length_one_is_recorded() {
        let mut store = WordPatternStore::new();
        // shoud → should: one insertion (distance 1, length diff 1).
        assert!(
            store
                .observe_correction(Outcome::CorrectedToOther, "shoud", "should", T0)
                .recorded
        );
        // recieve → receive: one transposition = two subs (distance 2, length diff 0).
        assert!(
            store
                .observe_correction(Outcome::CorrectedToOther, "recieve", "receive", T0)
                .recorded
        );
    }

    #[test]
    fn no_op_and_empty_are_skipped() {
        let mut store = WordPatternStore::new();
        assert!(
            !store
                .observe_correction(Outcome::CorrectedToOther, "the", "the", T0)
                .recorded
        );
        assert!(
            !store
                .observe_correction(Outcome::CorrectedToOther, "", "the", T0)
                .recorded
        );
        assert!(
            !store
                .observe_correction(Outcome::CorrectedToOther, "the", "", T0)
                .recorded
        );
        assert!(store.is_empty());
    }

    #[test]
    fn normalization_folds_case() {
        let mut store = WordPatternStore::new();
        store.observe_correction(Outcome::CorrectedToOther, "Teh", "The", T0);
        store.observe_correction(Outcome::CorrectedToOther, "teh", "the", T0);
        // Both fold to teh→the.
        assert_eq!(store.len(), 1);
        let snap = store.snapshot("teh", "the").unwrap();
        assert!((snap.weight - 2.0).abs() < 1e-6);
    }

    // ---- Decay ----------------------------------------------------------

    #[test]
    fn weight_halves_over_one_half_life() {
        let mut store = WordPatternStore::new();
        observe_n(&mut store, "teh", "the", 8, T0);
        // Read at T0: weight 8.
        assert!((store.snapshot("teh", "the").unwrap().weight - 8.0).abs() < 1e-3);
        // Touch the store one half-life later with an unrelated pattern so
        // last_now advances; teh→the should now read ~4.
        store.observe_correction(Outcome::CorrectedToOther, "adn", "and", T0 + HALF_LIFE);
        let snap = store.snapshot("teh", "the").unwrap();
        assert!(
            (snap.weight - 4.0).abs() < 0.05,
            "expected ~4, got {}",
            snap.weight
        );
    }

    #[test]
    fn crossing_tier1_min_observations() {
        let mut store = WordPatternStore::new();
        // Just under the bar.
        observe_n(&mut store, "teh", "the", 11, T0);
        assert!(store.snapshot("teh", "the").unwrap().weight < TIER1_MIN_OBSERVATIONS);
        // One more clears it (12 ≥ 12).
        store.observe_correction(Outcome::CorrectedToOther, "teh", "the", T0);
        assert!(store.snapshot("teh", "the").unwrap().weight >= TIER1_MIN_OBSERVATIONS);
    }

    // ---- 3-strike brake -------------------------------------------------

    #[test]
    fn three_undos_trip_the_brake_and_accept_resets() {
        let mut store = WordPatternStore::new();
        observe_n(&mut store, "teh", "the", 12, T0);

        assert_eq!(store.note_undo("teh", "the"), 1);
        assert_eq!(store.note_undo("teh", "the"), 2);
        assert_eq!(store.note_undo("teh", "the"), 3);
        assert_eq!(
            store.snapshot("teh", "the").unwrap().consecutive_undos,
            UNDO_BRAKE_STRIKES
        );

        // A kept correction breaks the streak.
        store.note_accept("teh", "the");
        assert_eq!(store.snapshot("teh", "the").unwrap().consecutive_undos, 0);
    }

    #[test]
    fn brake_methods_are_noops_for_unknown_patterns() {
        let mut store = WordPatternStore::new();
        assert_eq!(store.note_undo("teh", "the"), 0);
        store.note_accept("teh", "the"); // must not panic / create anything
        assert!(store.is_empty());
    }

    // ---- Snapshots ------------------------------------------------------

    #[test]
    fn snapshots_are_sorted_strongest_first() {
        let mut store = WordPatternStore::new();
        observe_n(&mut store, "teh", "the", 3, T0);
        observe_n(&mut store, "adn", "and", 7, T0);
        observe_n(&mut store, "thsi", "this", 5, T0);
        let snaps = store.snapshots();
        let order: Vec<&str> = snaps.iter().map(|s| s.typed.as_str()).collect();
        assert_eq!(order, vec!["adn", "thsi", "teh"]);
    }

    #[test]
    fn snapshot_is_none_for_unseen_pattern() {
        let store = WordPatternStore::new();
        assert!(store.snapshot("teh", "the").is_none());
    }

    // ---- Persistence ----------------------------------------------------

    #[test]
    fn persist_due_tracks_unsaved_corrections() {
        let mut store = WordPatternStore::new();
        assert!(!store.has_unsaved());
        observe_n(&mut store, "teh", "the", PATTERN_PERSIST_EVERY as u32, T0);
        assert!(store.has_unsaved());
        assert!(store.persist_due());
    }

    #[test]
    fn save_and_load_round_trips() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("ta_word_patterns_test_{}.json", std::process::id()));

        let mut store = WordPatternStore::new();
        observe_n(&mut store, "teh", "the", 5, T0);
        store.note_undo("teh", "the");
        store.save_to(&path).unwrap();
        // Saving resets the persist counter.
        assert!(!store.has_unsaved());

        let loaded = WordPatternStore::load_from(&path).unwrap();
        assert_eq!(loaded.version, WORD_PATTERN_VERSION);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.total_observations(), 5);
        let snap = loaded.snapshot("teh", "the").unwrap();
        assert!((snap.weight - 5.0).abs() < 1e-3);
        assert_eq!(snap.consecutive_undos, 1);
        assert!(!loaded.has_unsaved()); // a loaded store is clean

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn prune_drops_fully_decayed_patterns_on_save() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "ta_word_patterns_prune_{}.json",
            std::process::id()
        ));

        let mut store = WordPatternStore::new();
        store.observe_correction(Outcome::CorrectedToOther, "teh", "the", T0);
        // Advance ~20 half-lives via a fresh observation; the old single-obs
        // pattern decays below PRUNE_EPSILON and should be dropped on save.
        store.observe_correction(Outcome::CorrectedToOther, "adn", "and", T0 + HALF_LIFE * 20);
        store.save_to(&path).unwrap();

        assert!(
            store.snapshot("teh", "the").is_none(),
            "teh→the should be pruned"
        );
        assert!(
            store.snapshot("adn", "and").is_some(),
            "fresh pattern survives"
        );

        let _ = fs::remove_file(&path);
    }

    // ---- edit_distance helper ------------------------------------------

    #[test]
    fn edit_distance_is_correct() {
        assert_eq!(edit_distance("teh", "the"), 2); // transposition = 2 subs
        assert_eq!(edit_distance("shoud", "should"), 1); // insertion
        assert_eq!(edit_distance("the", "the"), 0);
        assert_eq!(edit_distance("", "the"), 3);
        assert_eq!(edit_distance("cat", "elephant"), 6); // match a+t, sub c, insert 5
    }
}
