//! Component 5c — **Motor Map v0**.
//!
//! The first thing in the engine that *learns the user*. Everything before
//! it (lexicon, candidates, score, decision) is content-driven — it knows
//! about words. The motor map knows about *fingers*: which keys this
//! particular hand produces cleanly and which keys it slips, and what the
//! slip lands on. It is the seed of the L3 spatial volatility map, built
//! up passively from the outcomes Component 5's resolver already produces.
//!
//! ## What it consumes
//!
//! Resolved per-word [`Outcome`]s. v0 reads **two** of the five states and
//! deliberately ignores the rest:
//!
//! * [`Outcome::Kept`] — the user typed a word and left it. Every character
//!   is a *positive* observation: this finger produced this key correctly.
//! * [`Outcome::CorrectedToOther`] — the user typed a word and then fixed it
//!   to something else. We Levenshtein-align typed → corrected; matched
//!   positions are positive observations, substitutions are *slips*
//!   (`incorrect[intended][typed] += 1`). Inserts/deletes are skipped in v0
//!   — they're length changes, not key-for-key slips, and attributing them
//!   to a finger is a v1 problem.
//!
//! [`Outcome::CorrectedToSuggestion`] (the engine was right) and
//! [`Outcome::Abandoned`] (the user walked away — no read-back of intent)
//! are skipped in v0. So is [`Outcome::Pending`], which carries no signal.
//!
//! ## Shape
//!
//! [`MotorMap`] is a `HashMap<char, SlipDistribution>` **keyed by the
//! intended character**. Each [`SlipDistribution`] holds how often that
//! intended key was produced correctly (`correct_count`) and, for the times
//! it wasn't, what it slipped *to* (`incorrect[typed] += 1`). So
//! `map[intended].incorrect[typed]` reads "how often `intended` came out as
//! `typed`."
//!
//! ## Forgetting — exponential decay, lazy
//!
//! Recovery is non-stationary: a finger that slipped last month may be
//! steady now (and a bad week shouldn't haunt a good one — *the good day
//! and the bad day both belong*). Counts decay with a **30-day half-life**
//! so the map tracks the hand as it is, not as it was. Decay is **lazy**:
//! we multiply a distribution by its decay factor only when we touch it
//! (on observe, and on read against [`MotorMap::last_now`]) — never a cron
//! sweep. An untouched key simply carries an older `last_update`; its value
//! materialises correctly the next time it's read or written.
//!
//! ## Normalization
//!
//! Lowercase only; all printable ASCII is in scope (letters, digits,
//! punctuation, the apostrophe). Control characters and modifiers are
//! skipped — Shift is a chord, not a key the finger "slips."
//!
//! ## Persistence
//!
//! Plain JSON. The module is path-agnostic on purpose — L4 stays portable,
//! so the *host* (the Tauri engine) supplies the `~/.typeassist/...` path.
//! [`MotorMap::persist_due`] tells the host when 100 observations have
//! accrued since the last save; [`MotorMap::save_to`] writes atomically and
//! resets that counter; [`MotorMap::write_snapshot`] is the same write
//! without the reset, for the weekly dated snapshot.
//!
//! ## L2→L3 kill-switch
//!
//! **Stays OFF in v0.** This module only *observes and stores*; nothing here
//! feeds a correction back into the engine. [`MotorMap::is_stable`] exists
//! to inform the eventual manual flip — "does the map look sensible yet?" —
//! it does not gate anything on its own.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Outcome;

/// Wall-clock timestamp, milliseconds since the UNIX epoch. Matches the
/// engine's `now_ms()` and [`crate::log::LogRecord::timestamp_ms`].
pub type Timestamp = u64;

/// On-disk shape version. Bump alongside any change to [`MotorMap`] /
/// [`SlipDistribution`] so a loader can refuse or migrate stale files.
pub const MOTOR_MAP_VERSION: u32 = 1;

/// Decay half-life: 30 days, in milliseconds. A count left untouched for
/// 30 days is worth half; for 60 days, a quarter.
pub const HALF_LIFE_MS: f32 = 30.0 * 24.0 * 60.0 * 60.0 * 1000.0;

/// Save cadence: the host should persist once this many observations have
/// accrued since the last save. See [`MotorMap::persist_due`].
pub const PERSIST_EVERY: u64 = 100;

/// Minimum decayed sample weight before a key earns a slip-rate / weakest
/// ranking, and the per-key bar [`MotorMap::is_stable`] counts against. A
/// build-time tuning knob — below this we have too little to say anything
/// honest (mirrors Progress's "still getting to know this one").
pub const MIN_SAMPLES: f32 = 20.0;

/// Decayed weights below this are pruned on touch, so a long-lived map
/// doesn't accumulate a tail of effectively-zero slip entries.
const PRUNE_EPSILON: f32 = 1.0e-4;

/// Edit-distance guard for `CorrectedToOther` slip recording. A correction
/// is treated as a **motor fix** (and its substitutions recorded as slips)
/// only when the typed→corrected Levenshtein distance is `≤ this` AND the
/// length difference is `≤ MAX_SLIP_LENGTH_DIFF`. Larger corrections are
/// semantic rewrites and are skipped **entirely** (no matches, no slips) so
/// they can't pollute the slip distribution that drives
/// [`MotorMap::query_weakest_keys`]. **Tunable** — conservative starting
/// point (2 / 1); loosen only if real data shows genuine larger motor slips.
pub const MAX_SLIP_EDIT_DISTANCE: usize = 2;
/// Companion to [`MAX_SLIP_EDIT_DISTANCE`]: max `|typed - corrected|` length
/// difference for a correction to count as a motor fix.
pub const MAX_SLIP_LENGTH_DIFF: usize = 1;

/// What a single [`MotorMap::observe_outcome`] recorded, so the host can
/// reconcile the capture funnel (CLAUDE.md Principle #8 — capture integrity
/// is observable). Char-level: one `Kept` word of N chars reports
/// `correct = N`; a `CorrectedToOther` reports matched chars as `correct`
/// and substitutions as `slips`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObserveReport {
    /// Matched / kept character observations recorded this call.
    pub correct: u32,
    /// Slip (substitution) observations recorded this call.
    pub slips: u32,
}

/// Read-only snapshot of the map's **kill-switch inputs** — the data a
/// future L2→L3 flip decision needs, without making the decision here (the
/// switch stays unbuilt). Also carries the `weakest` preview that Practice
/// mode consumes. Serializable so the host can surface it to the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StabilityReport {
    /// Lifetime observations folded in (reflective; never a target).
    pub total_observations: u64,
    /// Distinct intended keys with any data.
    pub keys_tracked: usize,
    /// Keys whose decayed sample weight clears [`MIN_SAMPLES`] — i.e. keys
    /// that have earned a trustworthy slip rate.
    pub keys_well_sampled: usize,
    /// The sample bar (so the consumer knows what `well_sampled` means).
    pub min_samples: f32,
    /// Decayed slip rate across well-sampled keys (`incorrect / total`),
    /// `[0,1]`. A spread/severity signal for the flip decision.
    pub overall_slip_rate: f32,
    /// Weakest keys preview — `(key, slip_rate)`, weakest first (same as
    /// [`MotorMap::query_weakest_keys`]). Practice mode's curriculum source.
    pub weakest: Vec<(char, f32)>,
    /// Read reference time (ms since epoch); decay is computed against this.
    pub generated_at: Timestamp,
}

/// Per-intended-character record: how reliably this key is produced, and
/// where it goes when it isn't.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlipDistribution {
    /// Decayed count of times this intended key was produced correctly —
    /// kept as-is, or matched in a typed→corrected alignment.
    pub correct_count: f32,
    /// Decayed counts of slips, keyed by the character actually typed:
    /// `incorrect[typed]` is how often this intended key came out as `typed`.
    pub incorrect: HashMap<char, f32>,
    /// When this distribution was last decayed (and so the reference point
    /// for the next lazy decay). Milliseconds since epoch.
    pub last_update: Timestamp,
}

impl SlipDistribution {
    fn new(now: Timestamp) -> Self {
        Self {
            correct_count: 0.0,
            incorrect: HashMap::new(),
            last_update: now,
        }
    }

    /// Decay factor for the elapsed time since `last_update`. `0.5` per
    /// half-life; clamped to `1.0` if `now` runs backwards (test fixtures,
    /// clock skew) so time never *adds* weight.
    fn decay_factor(&self, now: Timestamp) -> f32 {
        let elapsed = now.saturating_sub(self.last_update) as f32;
        if elapsed <= 0.0 {
            return 1.0;
        }
        0.5_f32.powf(elapsed / HALF_LIFE_MS)
    }

    /// Apply decay in place and advance `last_update` to `now`. Prunes slip
    /// entries that have decayed below [`PRUNE_EPSILON`]. Idempotent within
    /// the same `now` (a zero-elapsed call is a no-op).
    fn decay_in_place(&mut self, now: Timestamp) {
        let f = self.decay_factor(now);
        if f < 1.0 {
            self.correct_count *= f;
            self.incorrect.retain(|_, v| {
                *v *= f;
                *v >= PRUNE_EPSILON
            });
        }
        self.last_update = now;
    }

    /// Total decayed observations (correct + all slips) as of `now`, without
    /// mutating. Used by the read paths.
    fn decayed_total(&self, now: Timestamp) -> f32 {
        let f = self.decay_factor(now);
        (self.correct_count + self.incorrect.values().sum::<f32>()) * f
    }

    /// Decayed correct count as of `now`, without mutating.
    fn decayed_correct(&self, now: Timestamp) -> f32 {
        self.correct_count * self.decay_factor(now)
    }

    /// Decayed weight of the slip `intended → typed` as of `now`.
    fn decayed_incorrect(&self, typed: char, now: Timestamp) -> f32 {
        self.incorrect.get(&typed).copied().unwrap_or(0.0) * self.decay_factor(now)
    }
}

/// The motor map. Owned by the engine; serialized to JSON for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MotorMap {
    /// Shape version of this map (see [`MOTOR_MAP_VERSION`]).
    pub version: u32,
    /// Per-intended-character slip distributions.
    dists: HashMap<char, SlipDistribution>,
    /// Most recent observation time. Reads decay relative to this so a
    /// query reflects forgetting up to the last activity without needing a
    /// `now` argument of its own. Persisted so a reload keeps the clock.
    last_now: Timestamp,
    /// Lifetime count of observations folded in (Kept words + corrected
    /// words). Reflective, never a target — surfaced for diagnostics only.
    total_observations: u64,
    /// Observations since the last [`Self::save_to`]. Drives
    /// [`Self::persist_due`]; not persisted (a freshly loaded map is "clean").
    #[serde(skip)]
    obs_since_persist: u64,
}

impl Default for MotorMap {
    fn default() -> Self {
        Self {
            version: MOTOR_MAP_VERSION,
            dists: HashMap::new(),
            last_now: 0,
            total_observations: 0,
            obs_since_persist: 0,
        }
    }
}

impl MotorMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of intended keys with at least one observation.
    pub fn len(&self) -> usize {
        self.dists.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dists.is_empty()
    }

    /// Lifetime observations folded in. Reflective; never a goal.
    pub fn total_observations(&self) -> u64 {
        self.total_observations
    }

    /// The map's most recent observation time — the reference point reads
    /// decay against.
    pub fn last_now(&self) -> Timestamp {
        self.last_now
    }

    /// Fold one resolved outcome into the map.
    ///
    /// * `typed` — the word as the user originally typed it
    ///   ([`crate::log::LogRecord::original_text`]).
    /// * `corrected` — for [`Outcome::CorrectedToOther`], the word the user
    ///   landed on (the resolver's post-edit text). Ignored for
    ///   [`Outcome::Kept`]; `None` is tolerated and turns a `CorrectedToOther`
    ///   into a no-op (defensive — the caller should always supply it).
    /// * `now` — the resolution time, used for lazy decay and as the new
    ///   read reference point.
    ///
    /// Outcomes other than `Kept` / `CorrectedToOther` are skipped in v0.
    ///
    /// Returns an [`ObserveReport`] of how many char-level observations were
    /// recorded (`correct` = matched/kept chars, `slips` = substitutions) so
    /// the host can reconcile the capture funnel (Principle #8). A skipped or
    /// no-op outcome returns [`ObserveReport::default`] (zeros).
    pub fn observe_outcome(
        &mut self,
        outcome: Outcome,
        typed: &str,
        corrected: Option<&str>,
        now: Timestamp,
    ) -> ObserveReport {
        // Advance the read clock for every outcome we're handed, even the
        // skipped ones — keeps decay-on-read fresh. Guard against a
        // backwards clock so `last_now` is monotonic.
        self.last_now = now.max(self.last_now);

        match outcome {
            Outcome::Kept => {
                let mut chars = 0u32;
                for c in normalize(typed) {
                    self.bump_correct(c, now);
                    chars += 1;
                }
                if chars > 0 {
                    self.total_observations += 1;
                    self.obs_since_persist += 1;
                    tracing::info!(
                        target: "motor_map",
                        "MOTOR_OBSERVE_KEPT typed={:?} chars={}",
                        typed,
                        chars
                    );
                }
                ObserveReport {
                    correct: chars,
                    slips: 0,
                }
            }
            Outcome::CorrectedToOther => {
                let Some(corrected) = corrected else {
                    return ObserveReport::default();
                };
                let typed_chars = normalize(typed);
                let corrected_chars = normalize(corrected);
                let ops = align(&typed_chars, &corrected_chars);

                // Edit-distance guard. A `CorrectedToOther` conflates a motor
                // fix (`teh→the`) with a semantic rewrite (`cat→elephant`).
                // Only the former is motor signal; a rewrite's "slips" are
                // noise that would make `query_weakest_keys` recommend the
                // wrong keys to Practice — a high-cost error for the target
                // user. So we record ONLY when the edit looks like a fix:
                // small distance AND similar length. Larger → skip entirely
                // (no matches, no slips), not matches-only. Thresholds are a
                // conservative starting point; loosen if real motor patterns
                // at scale show genuine 3-edit slips.
                let edit_distance = ops
                    .iter()
                    .filter(|op| !matches!(op, AlignOp::Match(_)))
                    .count();
                let length_diff = typed_chars.len().abs_diff(corrected_chars.len());
                if edit_distance > MAX_SLIP_EDIT_DISTANCE || length_diff > MAX_SLIP_LENGTH_DIFF {
                    tracing::info!(
                        target: "motor_map",
                        "MOTOR_OBSERVE_SKIP_REWRITE typed={:?} corrected={:?} edit_distance={} length_diff={}",
                        typed,
                        corrected,
                        edit_distance,
                        length_diff
                    );
                    return ObserveReport::default();
                }

                let mut correct = 0u32;
                let mut slips = 0u32;
                for op in ops {
                    match op {
                        AlignOp::Match(c) => {
                            self.bump_correct(c, now);
                            correct += 1;
                        }
                        AlignOp::Sub { typed, intended } => {
                            self.bump_incorrect(intended, typed, now);
                            slips += 1;
                            tracing::info!(
                                target: "motor_map",
                                "MOTOR_OBSERVE_SLIP intended={:?} typed={:?}",
                                intended,
                                typed
                            );
                        }
                        // Inserts / deletes are length changes, not key-for-key
                        // slips — skipped in v0.
                        AlignOp::Del(_) | AlignOp::Ins(_) => {}
                    }
                }
                self.total_observations += 1;
                self.obs_since_persist += 1;
                if slips == 0 {
                    // All matches (e.g. a pure capitalisation fix that
                    // normalises away) — recorded as positive, no slip line.
                    tracing::info!(
                        target: "motor_map",
                        "MOTOR_OBSERVE_KEPT typed={:?} chars={} (corrected, no slip)",
                        typed,
                        correct
                    );
                }
                ObserveReport { correct, slips }
            }
            // v0: the engine being right (CorrectedToSuggestion) and the
            // user walking away (Abandoned) carry no key-for-key intent we
            // act on yet; Pending carries nothing at all.
            Outcome::CorrectedToSuggestion | Outcome::Abandoned | Outcome::Pending => {
                ObserveReport::default()
            }
        }
    }

    fn bump_correct(&mut self, c: char, now: Timestamp) {
        let dist = self
            .dists
            .entry(c)
            .or_insert_with(|| SlipDistribution::new(now));
        dist.decay_in_place(now);
        dist.correct_count += 1.0;
    }

    fn bump_incorrect(&mut self, intended: char, typed: char, now: Timestamp) {
        let dist = self
            .dists
            .entry(intended)
            .or_insert_with(|| SlipDistribution::new(now));
        dist.decay_in_place(now);
        *dist.incorrect.entry(typed).or_insert(0.0) += 1.0;
    }

    /// Given a character the user *typed*, what they probably *intended* —
    /// ranked by likelihood, decayed to [`Self::last_now`].
    ///
    /// Returns `(intended, p)` pairs where `p` is `incorrect[intended][typed]`
    /// normalized across every intended key that has slipped to `typed`, so
    /// the values sum to 1.0. Sorted most-probable first (ties broken by
    /// character for determinism). Empty if `typed` isn't observable or no
    /// slip has ever produced it.
    pub fn query_probable_intent(&self, typed: char) -> Vec<(char, f32)> {
        let Some(typed) = normalize_char(typed) else {
            return Vec::new();
        };
        let now = self.last_now;
        let mut candidates: Vec<(char, f32)> = self
            .dists
            .iter()
            .filter_map(|(&intended, dist)| {
                let w = dist.decayed_incorrect(typed, now);
                (w > 0.0).then_some((intended, w))
            })
            .collect();

        let total: f32 = candidates.iter().map(|&(_, w)| w).sum();
        if total <= 0.0 {
            return Vec::new();
        }
        for (_, w) in candidates.iter_mut() {
            *w /= total;
        }
        candidates.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        candidates
    }

    /// The `top_n` weakest keys — those with the highest slip rate among keys
    /// that clear [`MIN_SAMPLES`]. Returns `(intended, slip_rate)` where
    /// `slip_rate = incorrect_total / total ∈ [0, 1]`, weakest first. Keys
    /// below the sample bar are excluded (no number invented from too little).
    pub fn query_weakest_keys(&self, top_n: usize) -> Vec<(char, f32)> {
        let now = self.last_now;
        let mut ranked: Vec<(char, f32, f32)> = self
            .dists
            .iter()
            .filter_map(|(&intended, dist)| {
                let total = dist.decayed_total(now);
                if total < MIN_SAMPLES {
                    return None;
                }
                let correct = dist.decayed_correct(now);
                let slip_rate = ((total - correct) / total).clamp(0.0, 1.0);
                Some((intended, slip_rate, total))
            })
            .collect();

        // Weakest (highest slip rate) first; ties broken by more data, then
        // by character, so the ordering is deterministic.
        ranked.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.0.cmp(&b.0))
        });
        ranked
            .into_iter()
            .take(top_n)
            .map(|(c, slip_rate, _)| (c, slip_rate))
            .collect()
    }

    /// How reliably the user produces `intended` — `correct / total ∈ [0, 1]`,
    /// decayed to [`Self::last_now`]. `0.0` when there's no data for the key
    /// (rather than NaN), so callers can treat unknown keys as "no evidence."
    pub fn confidence(&self, intended: char) -> f32 {
        let Some(intended) = normalize_char(intended) else {
            return 0.0;
        };
        match self.dists.get(&intended) {
            Some(dist) => {
                let total = dist.decayed_total(self.last_now);
                if total <= 0.0 {
                    0.0
                } else {
                    (dist.decayed_correct(self.last_now) / total).clamp(0.0, 1.0)
                }
            }
            None => 0.0,
        }
    }

    /// v0 readiness heuristic for the (still-manual) L2→L3 kill-switch:
    /// `true` once at least `top_n` keys have cleared [`MIN_SAMPLES`]. This
    /// is "does the map have enough to say something about the weakest
    /// keys?" — it gates *nothing* on its own; the flip stays manual.
    pub fn is_stable(&self, top_n: usize) -> bool {
        let now = self.last_now;
        let well_sampled = self
            .dists
            .values()
            .filter(|d| d.decayed_total(now) >= MIN_SAMPLES)
            .count();
        well_sampled >= top_n
    }

    /// Snapshot the kill-switch inputs + the Practice weakest-keys preview
    /// as a [`StabilityReport`]. Read-only; decides nothing. `top_n` caps the
    /// `weakest` preview.
    pub fn stability_report(&self, top_n: usize) -> StabilityReport {
        let now = self.last_now;
        let mut keys_well_sampled = 0usize;
        let mut correct_sum = 0.0f32;
        let mut total_sum = 0.0f32;
        for dist in self.dists.values() {
            let total = dist.decayed_total(now);
            if total >= MIN_SAMPLES {
                keys_well_sampled += 1;
                correct_sum += dist.decayed_correct(now);
                total_sum += total;
            }
        }
        let overall_slip_rate = if total_sum > 0.0 {
            ((total_sum - correct_sum) / total_sum).clamp(0.0, 1.0)
        } else {
            0.0
        };
        StabilityReport {
            total_observations: self.total_observations,
            keys_tracked: self.dists.len(),
            keys_well_sampled,
            min_samples: MIN_SAMPLES,
            overall_slip_rate,
            weakest: self.query_weakest_keys(top_n),
            generated_at: now,
        }
    }

    /// Whether [`PERSIST_EVERY`] observations have accrued since the last
    /// [`Self::save_to`]. The host polls this after each observe.
    pub fn persist_due(&self) -> bool {
        self.obs_since_persist >= PERSIST_EVERY
    }

    /// Whether any observations have been folded in since the last
    /// [`Self::save_to`] — i.e. the live file on disk is stale. The host's
    /// periodic flush polls this so the live map is maintained on a time
    /// cadence, not only at the 100-observation mark or on shutdown.
    pub fn has_unsaved(&self) -> bool {
        self.obs_since_persist > 0
    }

    /// Serialize to `path` atomically (temp file + rename), creating parent
    /// directories as needed, and reset the persist counter. The host owns
    /// the path (e.g. `~/.typeassist/motor_map.json`) so L4 stays portable.
    pub fn save_to(&mut self, path: &Path) -> io::Result<()> {
        self.write_json(path)?;
        self.obs_since_persist = 0;
        Ok(())
    }

    /// Write a dated snapshot (the weekly archive) without resetting the
    /// persist counter. Same atomic write as [`Self::save_to`].
    pub fn write_snapshot(&self, path: &Path) -> io::Result<()> {
        self.write_json(path)?;
        tracing::info!(target: "motor_map", "MOTOR_SNAPSHOT_WRITTEN path={:?}", path);
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

    /// Load a map from `path`. A freshly loaded map starts "clean" (persist
    /// counter zero). Returns the deserialized map; a future version bump can
    /// migrate here.
    pub fn load_from(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        let mut map: MotorMap = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        map.obs_since_persist = 0;
        Ok(map)
    }
}

// --- Normalization ----------------------------------------------------------

/// Lowercase + keep only observable characters (printable ASCII: letters,
/// digits, punctuation, apostrophe). Control characters and non-ASCII are
/// dropped. Returns the surviving characters in order.
fn normalize(s: &str) -> Vec<char> {
    s.chars().filter_map(normalize_char).collect()
}

/// Lowercase a single character and keep it iff it's observable; else `None`.
fn normalize_char(c: char) -> Option<char> {
    let c = c.to_ascii_lowercase();
    c.is_ascii_graphic().then_some(c)
}

// --- Alignment --------------------------------------------------------------

/// One step of a typed→corrected alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlignOp {
    /// Same character in both — a correct production.
    Match(char),
    /// Substitution: `typed` came out where `intended` belonged.
    Sub { typed: char, intended: char },
    /// Character present in `typed` but not `corrected` (skipped in v0).
    Del(char),
    /// Character present in `corrected` but not `typed` (skipped in v0).
    Ins(char),
}

/// Levenshtein alignment of `typed` → `corrected` with operation backtrace.
/// Unit costs (match 0, sub/ins/del 1). Returns the optimal edit script in
/// forward order. Both inputs are already normalized.
fn align(typed: &[char], corrected: &[char]) -> Vec<AlignOp> {
    let n = typed.len();
    let m = corrected.len();
    // dp[i][j] = edit distance between typed[..i] and corrected[..j].
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i as u32;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub_cost = if typed[i - 1] == corrected[j - 1] {
                0
            } else {
                1
            };
            dp[i][j] = (dp[i - 1][j - 1] + sub_cost)
                .min(dp[i - 1][j] + 1) // delete from typed
                .min(dp[i][j - 1] + 1); // insert into typed
        }
    }

    // Backtrace from (n, m) to (0, 0), collecting ops reversed.
    //
    // **Tie-break:** among equal-cost optimal paths, prefer Del/Ins over
    // Sub. Because the walk runs end→start, this pushes length-difference
    // indels to the trailing boundary and pulls substitutions to the
    // *leftmost* divergence — the human reading of a slip. e.g.
    // `worxc`→`word` resolves to Sub{x→d} + Del(c) ("d mistyped as x, extra
    // trailing c"), not the equal-cost Del(x) + Sub{c→d}. Distance is
    // identical either way; only attribution differs, and this attribution
    // is the one slip detection wants. The final `else` is always a valid
    // Sub: row 0 / column 0 are caught by the Ins / Del arms.
    let mut ops = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && typed[i - 1] == corrected[j - 1] && dp[i][j] == dp[i - 1][j - 1] {
            ops.push(AlignOp::Match(typed[i - 1]));
            i -= 1;
            j -= 1;
        } else if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
            ops.push(AlignOp::Del(typed[i - 1]));
            i -= 1;
        } else if j > 0 && dp[i][j] == dp[i][j - 1] + 1 {
            ops.push(AlignOp::Ins(corrected[j - 1]));
            j -= 1;
        } else {
            ops.push(AlignOp::Sub {
                typed: typed[i - 1],
                intended: corrected[j - 1],
            });
            i -= 1;
            j -= 1;
        }
    }
    ops.reverse();
    ops
}

// --- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const T0: Timestamp = 1_700_000_000_000;
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;

    #[test]
    fn kept_word_records_each_char_as_correct() {
        let mut map = MotorMap::new();
        map.observe_outcome(Outcome::Kept, "cat", None, T0);
        assert_eq!(map.confidence('c'), 1.0);
        assert_eq!(map.confidence('a'), 1.0);
        assert_eq!(map.confidence('t'), 1.0);
        assert_eq!(map.total_observations(), 1);
    }

    #[test]
    fn kept_is_case_insensitive_and_skips_non_ascii() {
        let mut map = MotorMap::new();
        // "Café!" → c, a, f, '!' observable; 'é' dropped.
        map.observe_outcome(Outcome::Kept, "Café!", None, T0);
        assert_eq!(map.confidence('c'), 1.0);
        assert_eq!(map.confidence('f'), 1.0);
        assert_eq!(map.confidence('!'), 1.0);
        assert_eq!(map.len(), 4); // c, a, f, !  (no é)
    }

    #[test]
    fn corrected_to_other_records_substitution_as_slip() {
        let mut map = MotorMap::new();
        // typed "cst" → corrected "cat": position 1 is a slip a←s.
        map.observe_outcome(Outcome::CorrectedToOther, "cst", Some("cat"), T0);
        // 'c' and 't' matched → correct.
        assert_eq!(map.confidence('c'), 1.0);
        assert_eq!(map.confidence('t'), 1.0);
        // intended 'a' was typed as 's' → confidence 0, slip recorded.
        assert_eq!(map.confidence('a'), 0.0);
        let intent = map.query_probable_intent('s');
        assert_eq!(intent, vec![('a', 1.0)]);
    }

    #[test]
    fn edit_distance_guard_records_small_fix_skips_rewrite() {
        // worxc → word: edit distance 2 (sub x→d, del c), length diff 1 →
        // within the guard → slip recorded.
        let mut map = MotorMap::new();
        let rep = map.observe_outcome(Outcome::CorrectedToOther, "worxc", Some("word"), T0);
        assert_eq!(rep.slips, 1, "small fix records the slip");
        assert_eq!(map.query_probable_intent('x'), vec![('d', 1.0)]);

        // cat → dog: edit distance 3 (three subs) → a rewrite → skipped
        // ENTIRELY (no slips AND no matches), so it can't pollute the map.
        let mut map2 = MotorMap::new();
        let rep = map2.observe_outcome(Outcome::CorrectedToOther, "cat", Some("dog"), T0);
        assert_eq!(rep, ObserveReport::default(), "rewrite recorded nothing");
        assert!(
            map2.is_empty(),
            "rewrite left the slip distribution untouched"
        );

        // word → elephant: huge length diff → skipped.
        let mut map3 = MotorMap::new();
        let rep = map3.observe_outcome(Outcome::CorrectedToOther, "word", Some("elephant"), T0);
        assert_eq!(rep, ObserveReport::default());
        assert!(map3.is_empty());

        // word → words: distance 1 (insert), length diff 1 → within guard;
        // matches recorded, no slip (insert skipped in v0).
        let mut map4 = MotorMap::new();
        let rep = map4.observe_outcome(Outcome::CorrectedToOther, "word", Some("words"), T0);
        assert_eq!((rep.correct, rep.slips), (4, 0));
    }

    #[test]
    fn stability_report_exposes_kill_switch_inputs() {
        let mut map = MotorMap::new();
        // 'a': 25 clean obs → well-sampled, no slips.
        for _ in 0..25 {
            map.observe_outcome(Outcome::Kept, "a", None, T0);
        }
        // 'b': 18 obs → below MIN_SAMPLES, not well-sampled.
        for _ in 0..18 {
            map.observe_outcome(Outcome::Kept, "b", None, T0);
        }
        let r = map.stability_report(4);
        assert_eq!(r.keys_tracked, 2);
        assert_eq!(r.keys_well_sampled, 1, "only 'a' clears MIN_SAMPLES");
        assert_eq!(r.min_samples, MIN_SAMPLES);
        assert_eq!(r.total_observations, 43);
        assert!(r.overall_slip_rate < 1e-6, "no slips → ~0 slip rate");
    }

    #[test]
    fn worxc_to_word_records_d_from_x_slip() {
        // The 5c seal-then-correct validation: typed "worxc", corrected to
        // "word". Alignment: w,o,r match; x→d substitution; trailing c
        // deleted (skipped v0). The slip the motor map must learn is
        // intended 'd', typed 'x' (the MOTOR_OBSERVE_SLIP probe).
        let mut map = MotorMap::new();
        map.observe_outcome(Outcome::CorrectedToOther, "worxc", Some("word"), T0);
        // intended 'd' came out as 'x' → that exact slip is recorded.
        assert_eq!(map.query_probable_intent('x'), vec![('d', 1.0)]);
        // 'd' was only ever a slip target → zero confidence, never "correct".
        assert_eq!(map.confidence('d'), 0.0);
        // w, o, r matched → positive observations.
        assert_eq!(map.confidence('w'), 1.0);
        assert_eq!(map.confidence('o'), 1.0);
        assert_eq!(map.confidence('r'), 1.0);
        // 'c' was a delete → skipped in v0, no distribution.
        assert!(map.query_probable_intent('c').is_empty());
    }

    #[test]
    fn corrected_inserts_and_deletes_are_skipped() {
        let mut map = MotorMap::new();
        // typed "ct" → corrected "cat": pure insert of 'a'. No slip.
        map.observe_outcome(Outcome::CorrectedToOther, "ct", Some("cat"), T0);
        assert!(map.query_probable_intent('a').is_empty());
        // 'a' was inserted, never typed → no distribution for it.
        assert_eq!(map.confidence('a'), 0.0);
        // 'c' and 't' matched.
        assert_eq!(map.confidence('c'), 1.0);
        assert_eq!(map.confidence('t'), 1.0);

        // typed "cart" → corrected "cat": pure delete of 'r'. No slip.
        let mut map2 = MotorMap::new();
        map2.observe_outcome(Outcome::CorrectedToOther, "cart", Some("cat"), T0);
        // 'r' deleted → not recorded as a slip in v0.
        assert!(map2.query_probable_intent('r').is_empty());
    }

    #[test]
    fn corrected_with_no_text_is_a_noop() {
        let mut map = MotorMap::new();
        map.observe_outcome(Outcome::CorrectedToOther, "cst", None, T0);
        assert!(map.is_empty());
        assert_eq!(map.total_observations(), 0);
    }

    #[test]
    fn skipped_outcomes_do_not_record() {
        let mut map = MotorMap::new();
        map.observe_outcome(Outcome::CorrectedToSuggestion, "cat", Some("cat"), T0);
        map.observe_outcome(Outcome::Abandoned, "cat", None, T0);
        map.observe_outcome(Outcome::Pending, "cat", None, T0);
        assert!(map.is_empty());
        assert_eq!(map.total_observations(), 0);
    }

    #[test]
    fn confidence_blends_correct_and_slips() {
        let mut map = MotorMap::new();
        // 'a' produced correctly 3 times, slipped once.
        for _ in 0..3 {
            map.observe_outcome(Outcome::Kept, "a", None, T0);
        }
        map.observe_outcome(Outcome::CorrectedToOther, "s", Some("a"), T0);
        // correct 3, total 4 → 0.75.
        assert!((map.confidence('a') - 0.75).abs() < 1e-6);
    }

    #[test]
    fn decay_halves_a_count_after_one_half_life_on_write() {
        let mut map = MotorMap::new();
        map.observe_outcome(Outcome::Kept, "a", None, T0);
        // 30 days later, observe 'a' again: prior 1.0 decays to 0.5, then +1.
        map.observe_outcome(Outcome::Kept, "a", None, T0 + 30 * DAY_MS);
        let dist = &map.dists[&'a'];
        assert!(
            (dist.correct_count - 1.5).abs() < 1e-3,
            "got {}",
            dist.correct_count
        );
    }

    #[test]
    fn decay_applies_on_read_against_last_now() {
        let mut map = MotorMap::new();
        // 'a' observed at T0; total 1.0.
        map.observe_outcome(Outcome::Kept, "a", None, T0);
        // A later observation of a *different* key advances last_now by 30d.
        map.observe_outcome(Outcome::Kept, "b", None, T0 + 30 * DAY_MS);
        // 'a' was never touched after T0, but reads decay it to last_now:
        // correct 0.5 / total 0.5 → confidence is still 1.0 (ratio), but
        // its weight has halved (matters for sample gating).
        let dist = &map.dists[&'a'];
        assert!((dist.decayed_total(map.last_now()) - 0.5).abs() < 1e-3);
        assert_eq!(map.confidence('a'), 1.0); // ratio unaffected by decay
    }

    #[test]
    fn weakest_keys_gated_by_min_samples_and_ranked() {
        let mut map = MotorMap::new();
        // 'a': 30 obs, 6 slips → slip rate 0.2.
        for _ in 0..24 {
            map.observe_outcome(Outcome::Kept, "a", None, T0);
        }
        for _ in 0..6 {
            map.observe_outcome(Outcome::CorrectedToOther, "q", Some("a"), T0);
        }
        // 'b': 25 obs, 10 slips → slip rate 0.4 (weaker).
        for _ in 0..15 {
            map.observe_outcome(Outcome::Kept, "b", None, T0);
        }
        for _ in 0..10 {
            map.observe_outcome(Outcome::CorrectedToOther, "v", Some("b"), T0);
        }
        // 'c': only 3 obs → below MIN_SAMPLES, excluded.
        for _ in 0..3 {
            map.observe_outcome(Outcome::Kept, "c", None, T0);
        }

        let weak = map.query_weakest_keys(5);
        assert_eq!(weak.len(), 2, "c is below the sample bar");
        assert_eq!(weak[0].0, 'b'); // weakest first
        assert!((weak[0].1 - 0.4).abs() < 1e-6);
        assert_eq!(weak[1].0, 'a');
        assert!((weak[1].1 - 0.2).abs() < 1e-6);
    }

    #[test]
    fn probable_intent_normalizes_across_competing_intendeds() {
        let mut map = MotorMap::new();
        // 's' typed for intended 'a' 3 times, for intended 'd' once.
        for _ in 0..3 {
            map.observe_outcome(Outcome::CorrectedToOther, "s", Some("a"), T0);
        }
        map.observe_outcome(Outcome::CorrectedToOther, "s", Some("d"), T0);
        let intent = map.query_probable_intent('s');
        assert_eq!(intent.len(), 2);
        assert_eq!(intent[0].0, 'a');
        assert!((intent[0].1 - 0.75).abs() < 1e-6);
        assert_eq!(intent[1].0, 'd');
        assert!((intent[1].1 - 0.25).abs() < 1e-6);
    }

    #[test]
    fn is_stable_counts_well_sampled_keys() {
        let mut map = MotorMap::new();
        assert!(!map.is_stable(2));
        for key in ["a", "b"] {
            for _ in 0..(MIN_SAMPLES as usize) {
                map.observe_outcome(Outcome::Kept, key, None, T0);
            }
        }
        assert!(map.is_stable(2));
        assert!(!map.is_stable(3));
    }

    #[test]
    fn persist_due_trips_every_hundred_and_resets_on_save() {
        let mut map = MotorMap::new();
        for _ in 0..(PERSIST_EVERY - 1) {
            map.observe_outcome(Outcome::Kept, "a", None, T0);
        }
        assert!(!map.persist_due());
        map.observe_outcome(Outcome::Kept, "a", None, T0);
        assert!(map.persist_due());

        let path = std::env::temp_dir().join("typeassist_motor_map_test_persist.json");
        map.save_to(&path).unwrap();
        assert!(!map.persist_due());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn has_unsaved_tracks_dirty_state_across_save() {
        let mut map = MotorMap::new();
        assert!(!map.has_unsaved(), "fresh map is clean");
        map.observe_outcome(Outcome::Kept, "a", None, T0);
        assert!(map.has_unsaved(), "an observation marks the map dirty");

        let path = std::env::temp_dir().join("typeassist_motor_map_test_has_unsaved.json");
        map.save_to(&path).unwrap();
        assert!(!map.has_unsaved(), "save clears the dirty flag");
        let _ = fs::remove_file(&path);

        map.observe_outcome(Outcome::Kept, "b", None, T0);
        assert!(map.has_unsaved(), "a later observation re-dirties");
    }

    #[test]
    fn json_round_trip_preserves_observations() {
        let mut map = MotorMap::new();
        map.observe_outcome(Outcome::Kept, "cat", None, T0);
        map.observe_outcome(Outcome::CorrectedToOther, "cst", Some("cat"), T0);

        let path = std::env::temp_dir().join("typeassist_motor_map_test_roundtrip.json");
        map.save_to(&path).unwrap();
        let loaded = MotorMap::load_from(&path).unwrap();
        let _ = fs::remove_file(&path);

        assert_eq!(loaded.version, MOTOR_MAP_VERSION);
        assert_eq!(loaded.total_observations(), map.total_observations());
        assert_eq!(loaded.confidence('c'), map.confidence('c'));
        assert_eq!(
            loaded.query_probable_intent('s'),
            map.query_probable_intent('s')
        );
    }

    #[test]
    fn align_produces_expected_ops() {
        // substitution
        let ops = align(&['c', 's', 't'], &['c', 'a', 't']);
        assert_eq!(
            ops,
            vec![
                AlignOp::Match('c'),
                AlignOp::Sub {
                    typed: 's',
                    intended: 'a'
                },
                AlignOp::Match('t'),
            ]
        );
        // insert
        let ops = align(&['c', 't'], &['c', 'a', 't']);
        assert!(ops.contains(&AlignOp::Ins('a')));
        // delete
        let ops = align(&['c', 'a', 'r', 't'], &['c', 'a', 't']);
        assert!(ops.contains(&AlignOp::Del('r')));
    }
}
