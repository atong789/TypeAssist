//! Per-user motor baseline — **Component 5c Layer A, Phase 1**.
//!
//! Answers one question per keystroke: *does this look anomalous for THIS
//! user?* The downstream `motor_evidence` signal will multiply with
//! `lexicon_evidence` (5b) to form correction confidence — but corrections
//! are sparse (broken feedback loop), so we cannot learn motor evidence
//! from corrections alone. Strategy: **prior-first**, same shape as 5b's
//! Norvig-frequency prior. The user's baseline starts at a population
//! prior and converges to their own typing as samples arrive.
//!
//! ## What we model (this phase)
//!
//! Per-keystroke **dwell** (key-down to key-up duration). Recency-weighted
//! mean + variance at three hierarchical levels:
//!
//!   * `(hand, finger, key)` — the leaf. Sparse for low-frequency keys
//!     (R-pinky `;` may have ≤ 5 samples even after a long session).
//!   * `(hand, finger)`      — pools the finger's keys.
//!   * `(hand,)`             — pools the whole hand.
//!
//! Plus a constant **population prior** above the hand level for absolute
//! cold-start (first few keystrokes have a sensible baseline instead of
//! exploding z-scores).
//!
//! Layer A Phase 2 adds IKI (commit H) and co-activation (I); Phase 2 of
//! the larger 5c wires the resulting `KeystrokeAnomaly` into
//! `correction_engine::measure_token_motor`. **This phase is observe-only**
//! — `MotorBaseline` is constructed but no consumer reads it yet.
//!
//! ## Why shrinkage, not strict backoff
//!
//! Strict backoff ("use the leaf if `n >= 30`, else parent") has a cliff at
//! 30 where the estimate jumps from "wild" to "settled." Empirical-Bayes
//! shrinkage smoothly blends leaf and parent by sample count:
//!
//! ```text
//! w   = n_leaf / (n_leaf + K)        // shrinkage weight ∈ [0, 1)
//! μ̂  = w · μ_leaf + (1 − w) · μ_parent
//! σ̂² = w · σ²_leaf + (1 − w) · σ²_parent
//! ```
//!
//! At `n_leaf = K`, the leaf contributes 50% — well-known partial-pooling.
//! With `K = 30`, an R-pinky-`;` bucket with one sample borrows ~97% from
//! its finger; with 100 samples it's ~77% leaf. Stable from day one,
//! converges as data grows.
//!
//! ## Why per-sample EWMA, not per-time
//!
//! The user is **recovering** — the model must track the typist they're
//! becoming and intervene LESS over time. A frozen historical mean
//! produces growing false-positives as a slow ring finger speeds up.
//!
//! Per-sample EWMA with half-life ≈ [`HALF_LIFE_SAMPLES`] keystrokes
//! (~a few days of moderate typing) is the simplest shape that does the
//! job. Per-time would be more correct (sleep-cycle effects on dwell are
//! real) but adds wall-clock dependencies and barely changes Layer A's
//! outputs at this granularity. Revisit if it becomes a problem.
//!
//! ## Layer B seam — `per_finger_reliability`
//!
//! Layer B (the slip→target map, a future pass) needs to know how
//! *consistent* each finger is, so a slow-recovery finger's typed key
//! weights more toward "this is a slip from an adjacent key" than a
//! fast-recovery finger's. [`MotorBaseline::per_finger_reliability`]
//! exposes that signal:
//!
//!   * `score ∈ [0, 1]` — higher = more consistent (lower coefficient of
//!     variation on dwell). Saturates at low CV.
//!   * `n_eff` — effective sample count, so Layer B can refuse a
//!     reliability that's based on too little data.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use volatility_map::{finger_for, Finger, Hand};

/// **Recency half-life in keystrokes.** A bucket's "effective sample
/// count" converges asymptotically to `1 / (1 - λ)` ≈
/// `HALF_LIFE_SAMPLES / ln(2)` ≈ 7200 — i.e. about 7000 keystrokes of
/// recent history dominate the mean once warmed up. The half-life
/// itself is 5000: a sample 5000 keystrokes ago has 50% the weight of
/// the most recent one.
///
/// Picked as a starting point — tune from real data. Too short: noise
/// dominates and the baseline never settles. Too long: doesn't track
/// recovery and the recovering user gets corrected on a stale profile.
pub const HALF_LIFE_SAMPLES: f64 = 5000.0;

/// Per-sample EWMA decay factor for an arbitrary half-life — `λ =
/// exp(-ln(2) / half_life)`, chosen so a sample `half_life`
/// keystrokes ago has weight `λ^half_life = 0.5`.
fn decay_lambda_for(half_life: f64) -> f64 {
    (-std::f64::consts::LN_2 / half_life).exp()
}

/// Population prior for dwell — a generic "typical typist" used until the
/// user's hand-level data dominates. μ = 100 ms, σ = 40 ms (σ² = 1600).
/// These numbers are loose — wide enough that the first keystrokes give
/// modest z-scores rather than explosions, narrow enough that real
/// outliers still register. The shrinkage formula doesn't care; the
/// user's own bucket takes over within tens of samples regardless of
/// the prior's exact value.
const POP_DWELL_MEAN_MS: f64 = 100.0;
const POP_DWELL_VAR_MS2: f64 = 1600.0;

/// Shrinkage strengths — bigger means the parent prior holds on longer
/// before the user's own data dominates. At `n_eff = K`, the child
/// contributes 50% of the estimate.
///
/// Three K values, one per pooling tier — pop → hand → finger → key.
/// `K_POP` is largest (the population prior is generic; we want the
/// user's hand-level mean to take over comfortably before key-level
/// estimates lean on it). `K_FINGER` is smallest (a key with even a
/// few real samples is more trustworthy than its finger pool, since
/// individual keys differ a lot in mechanical reach).
///
/// Production values are sized against the production `n_eff`
/// asymptote (≈ HALF_LIFE / ln 2 ≈ 7200 at half-life 5000) — the
/// user's data fully dominates each level once it's well-trained.
/// Tests run with a much shorter half-life so the asymptote is
/// smaller; they pass [`ShrinkParams`] explicitly to keep the relative
/// shape correct.
pub const K_POP: f64 = 100.0;
pub const K_HAND: f64 = 50.0;
pub const K_FINGER: f64 = 30.0;

/// Shrinkage strengths for the three pooling tiers. Production callers
/// construct via [`ShrinkParams::production`]; tests use
/// [`ShrinkParams::for_half_life`] to scale them down so the test
/// half-life's smaller asymptote still gives the leaf room to
/// dominate.
#[derive(Debug, Clone, Copy)]
pub struct ShrinkParams {
    pub k_pop: f64,
    pub k_hand: f64,
    pub k_finger: f64,
    pub k_certainty: f64,
}

impl ShrinkParams {
    /// Production values — chosen against the 5000-keystroke half-life.
    pub fn production() -> Self {
        Self {
            k_pop: K_POP,
            k_hand: K_HAND,
            k_finger: K_FINGER,
            k_certainty: K_CERTAINTY,
        }
    }

    /// Scale the K's proportionally with the given half-life so the
    /// shrinkage→user-dominance curve has the same SHAPE regardless of
    /// the absolute time constant. The asymptote of `n_eff` under the
    /// EWMA is `half_life / ln 2`; pinning each K to a fixed fraction
    /// of that keeps "the user takes over at the same fractional sample
    /// count" invariant across half-lives.
    pub fn for_half_life(half_life: f64) -> Self {
        let asymptote = half_life / std::f64::consts::LN_2;
        let production_asymptote = HALF_LIFE_SAMPLES / std::f64::consts::LN_2;
        let scale = asymptote / production_asymptote;
        Self {
            k_pop: K_POP * scale,
            k_hand: K_HAND * scale,
            k_finger: K_FINGER * scale,
            k_certainty: K_CERTAINTY * scale,
        }
    }
}

/// `n_eff` value at which `certainty` reaches 0.5. Same shape as the
/// shrinkage K — at K_CERTAINTY samples the anomaly can be "trusted at
/// 50%", far more at 200 samples, etc. Tuned independently of the
/// pooling Ks because "should we report an anomaly?" is a different
/// question from "how should the pooling weights work."
pub const K_CERTAINTY: f64 = 30.0;

/// Variance floor — a near-degenerate bucket (every observed sample
/// equal) would otherwise yield σ → 0 and z → ∞ on the slightest
/// deviation. σ = 10ms ≈ minimum biological key release time. Below
/// that, real timing differences are dominated by sensor noise; the
/// floor keeps z-scores meaningful.
const MIN_VAR_MS2: f64 = 100.0;

/// Single EWMA cell — recency-weighted mean + variance for one
/// `(hand, finger, key)`, `(hand, finger)`, or `(hand,)` bucket.
/// `n_eff` is the effective sample count under the recency weighting:
/// fresh cell starts at 0, asymptotes to `1 / (1 − λ)` after enough
/// samples. Used both as the shrinkage weight numerator and as the
/// raw input to `certainty`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct EwmaCell {
    pub mean: f64,
    pub var: f64,
    pub n_eff: f64,
}

impl EwmaCell {
    pub fn new() -> Self {
        Self::default()
    }

    /// Welford-style EWMA update with decay factor `λ`. Each call:
    ///
    /// 1. Decays the existing `n_eff` and adds 1 (so a steady stream
    ///    converges to `1 / (1 − λ)`).
    /// 2. Shifts the mean toward the new sample with weight `(1 − λ)`.
    /// 3. Updates the variance using the old mean's delta — West's
    ///    recursion for an exponentially-weighted variance.
    ///
    /// Numerically stable for the timing values we see (dwell 0–2000ms,
    /// IKI 0–1500ms), and the mean/variance shape is what `dwell_anomaly`
    /// reads directly.
    pub fn observe(&mut self, sample: f64, lambda: f64) {
        let alpha = 1.0 - lambda;
        let delta = sample - self.mean;
        self.n_eff = lambda * self.n_eff + 1.0;
        self.mean += alpha * delta;
        // West's exponentially-weighted variance recursion.
        self.var = lambda * (self.var + alpha * delta * delta);
    }
}

/// Pool `child` toward `parent` with shrinkage strength `k`. The child's
/// `n_eff` controls the weight: tiny child + large k → almost entirely
/// the parent; large child or small k → mostly the child. Returns a
/// **new cell** whose mean/variance are the pooled estimates and whose
/// `n_eff` is the **child's** count (so the result's certainty reflects
/// "how much real leaf data backs this," not how much parent prior
/// we've borrowed).
fn shrunk_with(child: EwmaCell, parent: EwmaCell, k: f64) -> EwmaCell {
    // Cold-start guard: zero n_eff → weight strictly 0, so the result is
    // purely the parent. Avoids `0 / (0 + k) = 0` evaluating as NaN under
    // any imaginable edge case.
    let w = if child.n_eff > 0.0 {
        child.n_eff / (child.n_eff + k)
    } else {
        0.0
    };
    EwmaCell {
        mean: w * child.mean + (1.0 - w) * parent.mean,
        var: w * child.var + (1.0 - w) * parent.var,
        n_eff: child.n_eff,
    }
}

/// Per-keystroke dwell anomaly result — returned by
/// [`MotorBaseline::dwell_anomaly`].
///
/// `anomaly` is the headline number consumers will eventually read; it's
/// in `[0, 1]` where 0 = "exactly the baseline mean" and 1 = "extremely
/// unusual for this user." The other fields are diagnostic: the panel
/// shows them, tests assert on them, and Phase 2 of 5c will reach for
/// `certainty` to weight Layer A against Layer B's slip-target prior.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DwellAnomaly {
    /// z-score of the observed dwell against the (pooled) baseline.
    /// Signed: negative = unusually short (graze-shape), positive =
    /// unusually long (late finger).
    pub z: f64,
    /// `1 - exp(-z² / 2)` — bounded `[0, 1]`, symmetric in sign, smooth.
    /// At z=1: ~0.39. At z=2: ~0.86. At z=3: ~0.989.
    pub anomaly: f64,
    /// The pooled mean used to score this keystroke. Diagnostic.
    pub mean_used: f64,
    /// The pooled (variance-floored) sigma. Diagnostic.
    pub sigma_used: f64,
    /// "How much data backs this score" — `n_eff / (n_eff + K_CERTAINTY)`.
    /// `0` cold-start, → `1` saturated. Phase 2 will weight Layer A
    /// against Layer B by this.
    pub certainty: f64,
}

/// Per-finger reliability — the Layer B seam. Higher score = more
/// consistent timing on that finger → spatial slips are LESS likely
/// (when a reliable finger produces a key, trust it). Lower score =
/// noisier finger → spatial slips are MORE likely (an unreliable
/// finger may have slipped to an adjacent key).
///
/// Returned by [`MotorBaseline::per_finger_reliability`]; Layer B
/// reads it to weight its keyboard-adjacency prior.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Reliability {
    /// `[0, 1]` — 1 = perfectly reliable, 0 = wildly variable. Computed
    /// from the coefficient of variation (σ / μ) on dwell: low CV →
    /// high score. Saturates so a cold-start finger reads as low
    /// reliability rather than NaN.
    pub score: f64,
    /// Effective sample count for the finger-level cell. Layer B
    /// should refuse a reliability score with `n_eff < ~20` and fall
    /// back to a neutral prior.
    pub n_eff: f64,
}

/// Per-user motor baseline — Component 5c Layer A. See module docs.
#[derive(Debug)]
pub struct MotorBaseline {
    /// `(hand, finger, key)` → dwell EWMA. Key is lowercased
    /// single-character string (matches the sidecar's `Key` events
    /// after lowercasing); multi-character key names are not modeled.
    keys: HashMap<(Hand, Finger, String), EwmaCell>,
    /// `(hand, finger)` → dwell EWMA.
    fingers: HashMap<(Hand, Finger), EwmaCell>,
    /// `(hand,)` → dwell EWMA.
    hands: HashMap<Hand, EwmaCell>,
    /// EWMA half-life in samples. Production uses
    /// [`HALF_LIFE_SAMPLES`] (≈5000 keystrokes). Tests use a much
    /// smaller value (e.g. 30) so the math converges within hundreds
    /// of samples instead of tens of thousands — same shape, same
    /// asymptotic behaviour, just compressed.
    half_life: f64,
    /// Shrinkage K's for the three pooling tiers plus the certainty
    /// curve. Production defaults to [`ShrinkParams::production`];
    /// tests scale these with [`ShrinkParams::for_half_life`] so the
    /// "user dominates at the same fractional sample count" shape is
    /// invariant under half-life compression.
    shrink: ShrinkParams,
}

impl Default for MotorBaseline {
    fn default() -> Self {
        Self::new()
    }
}

impl MotorBaseline {
    /// Production baseline — half-life is [`HALF_LIFE_SAMPLES`],
    /// shrinkage Ks are [`ShrinkParams::production`].
    pub fn new() -> Self {
        Self::with_half_life(HALF_LIFE_SAMPLES)
    }

    /// Build with a custom EWMA half-life. **Tests use this** to
    /// converge within hundreds of samples; production calls
    /// [`Self::new`]. Same algorithm, same anomaly formula — the
    /// shrinkage Ks scale proportionally so the user-dominates curve
    /// preserves its shape under compression.
    pub fn with_half_life(half_life: f64) -> Self {
        Self::with_params(half_life, ShrinkParams::for_half_life(half_life))
    }

    /// Full-control constructor. Production callers don't need this;
    /// tests use it to pin a known shrinkage configuration.
    pub fn with_params(half_life: f64, shrink: ShrinkParams) -> Self {
        assert!(half_life > 0.0, "half-life must be positive");
        Self {
            keys: HashMap::new(),
            fingers: HashMap::new(),
            hands: HashMap::new(),
            half_life,
            shrink,
        }
    }

    /// Record a keystroke's dwell into all three levels. Keys without a
    /// touch-typing assignment (function keys, arrows, multi-char names)
    /// are silently ignored — `finger_for` returns `None` and the
    /// baseline isn't updated. Shifted variants are normalised to the
    /// lowercase letter: `A` and `a` share a cell (the physical key is
    /// the same; shift overhead is a different signal we don't model
    /// here).
    pub fn observe_key(&mut self, key: &str, dwell_ms: u32) {
        let Some((hand, finger)) = finger_for(key) else { return; };
        let leaf_id = match leaf_key(key) {
            Some(id) => id,
            None => return,
        };
        let sample = f64::from(dwell_ms);
        let lambda = decay_lambda_for(self.half_life);

        self.keys
            .entry((hand, finger, leaf_id))
            .or_default()
            .observe(sample, lambda);
        self.fingers
            .entry((hand, finger))
            .or_default()
            .observe(sample, lambda);
        self.hands.entry(hand).or_default().observe(sample, lambda);
    }

    /// Score a single keystroke's dwell against the pooled baseline.
    /// Returns `None` only for keys outside the touch-typing map —
    /// same shape as `observe_key`. Pools through:
    ///
    /// 1. Population prior → `K_POP` shrinkage → hand cell.
    /// 2. Hand result → `K_HAND` shrinkage → finger cell.
    /// 3. Finger result → `K_FINGER` shrinkage → key cell.
    ///
    /// The final estimate's mean and (variance-floored) σ give the
    /// z-score; `anomaly` is the smooth `[0,1]` mapping;
    /// `certainty` reflects the leaf's `n_eff`.
    pub fn dwell_anomaly(&self, key: &str, dwell_ms: u32) -> Option<DwellAnomaly> {
        let (hand, finger) = finger_for(key)?;
        let leaf_id = leaf_key(key)?;

        let est = self.estimate_dwell(hand, finger, &leaf_id);
        let sigma = est.var.max(MIN_VAR_MS2).sqrt();
        let z = (f64::from(dwell_ms) - est.mean) / sigma;
        let anomaly = 1.0 - (-0.5 * z * z).exp();
        let certainty = est.n_eff / (est.n_eff + self.shrink.k_certainty);

        Some(DwellAnomaly {
            z,
            anomaly,
            mean_used: est.mean,
            sigma_used: sigma,
            certainty,
        })
    }

    /// Pool the three levels into a single estimate for `(hand, finger,
    /// key)`. Population prior sits above the hand level so even a
    /// brand-new user gets a meaningful (if uncertain) score.
    fn estimate_dwell(&self, hand: Hand, finger: Finger, leaf_id: &str) -> EwmaCell {
        let pop = EwmaCell {
            mean: POP_DWELL_MEAN_MS,
            var: POP_DWELL_VAR_MS2,
            n_eff: 0.0,
        };
        let hand_cell = self.hands.get(&hand).copied().unwrap_or_default();
        let hand_est = shrunk_with(hand_cell, pop, self.shrink.k_pop);

        let finger_cell = self
            .fingers
            .get(&(hand, finger))
            .copied()
            .unwrap_or_default();
        let finger_est = shrunk_with(finger_cell, hand_est, self.shrink.k_hand);

        let key_cell = self
            .keys
            .get(&(hand, finger, leaf_id.to_string()))
            .copied()
            .unwrap_or_default();
        shrunk_with(key_cell, finger_est, self.shrink.k_finger)
    }

    /// **Layer B seam.** How consistent is this finger's timing?
    /// Returns a reliability score in `[0, 1]` (saturated) plus the
    /// underlying `n_eff` so callers can refuse low-data signals.
    ///
    /// Reliability = `1 - clamp(2·CV, 0, 1)` where CV is σ / μ on the
    /// **finger-level** cell (not pooled — we want this finger's own
    /// noise, not noise leaked in from its hand pool). CV = 0.5
    /// (σ = half the mean) is the cliff at zero; CV = 0 is perfect.
    /// Cold-start finger with zero `n_eff` returns score = 0.
    pub fn per_finger_reliability(&self, hand: Hand, finger: Finger) -> Reliability {
        let cell = self
            .fingers
            .get(&(hand, finger))
            .copied()
            .unwrap_or_default();
        if cell.n_eff <= 0.0 || cell.mean <= 0.0 {
            return Reliability {
                score: 0.0,
                n_eff: 0.0,
            };
        }
        let sigma = cell.var.max(MIN_VAR_MS2).sqrt();
        let cv = sigma / cell.mean;
        let score = (1.0 - (cv * 2.0).min(1.0)).max(0.0);
        Reliability {
            score,
            n_eff: cell.n_eff,
        }
    }

    /// Read-only access to the per-(hand, finger) cell — used by tests
    /// and (later) by the panel snapshot. Not exposed in the public
    /// API beyond diagnostics; consumers should ask `dwell_anomaly`
    /// for the pooled estimate.
    pub fn finger_cell(&self, hand: Hand, finger: Finger) -> Option<EwmaCell> {
        self.fingers.get(&(hand, finger)).copied()
    }

    /// Read-only access to the per-hand cell. Same diagnostic role.
    pub fn hand_cell(&self, hand: Hand) -> Option<EwmaCell> {
        self.hands.get(&hand).copied()
    }
}

/// Lowercased single-character "leaf key" used to index the per-key
/// cell. Returns `None` for multi-character key names so we don't index
/// `Escape`/`ArrowLeft`/etc. as if they were touch-typing letters.
/// Same canonicalisation as `finger_for` uses internally.
fn leaf_key(key: &str) -> Option<String> {
    let lower = key.to_lowercase();
    let mut chars = lower.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(c.to_string())
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Approximate-equals helper. The EWMA arithmetic accumulates
    /// floating-point error over thousands of samples; for the
    /// shapes we're checking, 1e-6 is more than tight enough.
    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    /// Tests use a short half-life (30 samples) so the EWMA converges
    /// within hundreds of samples instead of tens of thousands. The
    /// math, shrinkage, and anomaly formulas are identical to the
    /// production HALF_LIFE_SAMPLES = 5000 — just compressed onto a
    /// faster axis so deterministic tests can pin them.
    const TEST_HALF_LIFE: f64 = 30.0;

    fn fresh() -> MotorBaseline {
        MotorBaseline::with_half_life(TEST_HALF_LIFE)
    }

    // ---- EWMA cell — the core math ---------------------------------------

    #[test]
    fn fresh_cell_is_zero_everywhere() {
        let c = EwmaCell::new();
        assert_eq!(c.mean, 0.0);
        assert_eq!(c.var, 0.0);
        assert_eq!(c.n_eff, 0.0);
    }

    #[test]
    fn first_sample_pulls_mean_partway_toward_it() {
        // EWMA on a fresh cell with sample x and decay λ:
        //   mean ← 0 + (1 - λ) · (x - 0) = (1 - λ) · x
        //   n_eff ← 0 · λ + 1 = 1
        // So the mean is NOT the sample — it's pulled (1 - λ)·x of the
        // way toward it. For HALF_LIFE = 30 (test value), (1 - λ) ≈
        // 0.0228 — the cell barely moves on a single sample, which is
        // the point of a long-ish half-life relative to one step.
        let lambda = decay_lambda_for(TEST_HALF_LIFE);
        let alpha = 1.0 - lambda;
        let mut c = EwmaCell::new();
        c.observe(100.0, lambda);
        assert!(approx(c.mean, alpha * 100.0, 1e-9));
        assert!(approx(c.n_eff, 1.0, 1e-9));
    }

    #[test]
    fn steady_stream_converges_to_constant_sample_value() {
        // Feed the same value enough times that the mean settles. For
        // half-life 30, 1000 samples = 33 half-lives → convergence
        // tighter than 1e-9. var → 0.
        let lambda = decay_lambda_for(TEST_HALF_LIFE);
        let mut c = EwmaCell::new();
        for _ in 0..1_000 {
            c.observe(120.0, lambda);
        }
        assert!(approx(c.mean, 120.0, 0.01));
        assert!(c.var < 0.01);
        // n_eff asymptotes to 1/(1-λ) ≈ TEST_HALF_LIFE / ln(2) ≈ 43.3.
        let asymptote = 1.0 / (1.0 - lambda);
        assert!(approx(c.n_eff, asymptote, 0.5));
    }

    #[test]
    fn alternating_samples_grow_variance_around_the_mean() {
        // Alternate 80ms and 120ms after warm-up. Mean settles on 100;
        // variance settles around the population variance ≈ 400. The
        // exponentially-weighted form is biased toward the per-step
        // delta around the running mean — but it should be in the
        // right ballpark (50 ≤ var ≤ 800).
        let lambda = decay_lambda_for(TEST_HALF_LIFE);
        let mut c = EwmaCell::new();
        for i in 0..2_000 {
            let s = if i % 2 == 0 { 80.0 } else { 120.0 };
            c.observe(s, lambda);
        }
        // The mean oscillates within ~(1-λ)·delta ≈ 0.5ms either side
        // of 100 at equilibrium — the test snapshots one step of that
        // oscillation, so a tight tolerance would be fragile.
        assert!(approx(c.mean, 100.0, 1.0), "mean = {}", c.mean);
        assert!(
            (50.0..=800.0).contains(&c.var),
            "expected var ≈ population variance 400, got {}",
            c.var
        );
    }

    // ---- shrunk_with — the partial-pooling primitive ---------------------

    #[test]
    fn shrinkage_of_empty_child_is_purely_parent() {
        let child = EwmaCell::new();
        let parent = EwmaCell {
            mean: 100.0,
            var: 400.0,
            n_eff: 50.0,
        };
        let shrunk = shrunk_with(child, parent, 30.0);
        assert_eq!(shrunk.mean, 100.0);
        assert_eq!(shrunk.var, 400.0);
        // The returned n_eff reflects the LEAF (the certainty of the
        // shrunk estimate is "how much real leaf data do we have"),
        // so it stays zero here.
        assert_eq!(shrunk.n_eff, 0.0);
    }

    #[test]
    fn shrinkage_at_k_equals_half_and_half() {
        // With child.n_eff = K, the formula gives w = K/(K+K) = 0.5.
        let child = EwmaCell {
            mean: 80.0,
            var: 100.0,
            n_eff: 30.0,
        };
        let parent = EwmaCell {
            mean: 100.0,
            var: 400.0,
            n_eff: 100.0,
        };
        let shrunk = shrunk_with(child, parent, 30.0);
        assert!(approx(shrunk.mean, 90.0, 1e-9));
        assert!(approx(shrunk.var, 250.0, 1e-9));
    }

    #[test]
    fn shrinkage_with_rich_child_mostly_uses_child() {
        // With child.n_eff = 10 × K, weight is 10/11 ≈ 0.909 — child
        // dominates.
        let child = EwmaCell {
            mean: 60.0,
            var: 100.0,
            n_eff: 300.0,
        };
        let parent = EwmaCell {
            mean: 100.0,
            var: 400.0,
            n_eff: 1000.0,
        };
        let shrunk = shrunk_with(child, parent, 30.0);
        // 10/11 of 60 + 1/11 of 100 = (600 + 100) / 11 ≈ 63.6
        assert!(approx(shrunk.mean, 700.0 / 11.0, 1e-9));
    }

    // ---- Full pipeline: observe_key → dwell_anomaly ----------------------

    #[test]
    fn cold_start_anomaly_uses_population_prior() {
        // No prior keystrokes — every level is empty. Dwell anomaly
        // should be computable (population prior bootstraps the
        // estimate) but the certainty should be near zero.
        let mb = fresh();
        let a = mb.dwell_anomaly("a", 100).expect("single-char key has a finger");
        // mean_used should equal the population prior (no user data
        // anywhere).
        assert!(approx(a.mean_used, POP_DWELL_MEAN_MS, 1e-9));
        // certainty: leaf n_eff = 0 → 0 / (0 + K) = 0.
        assert_eq!(a.certainty, 0.0);
        // Sample equals the prior mean → z = 0 → anomaly = 0.
        assert!(approx(a.anomaly, 0.0, 1e-12));
    }

    #[test]
    fn cold_start_outlier_still_registers_as_anomalous() {
        // A 500ms dwell against the 100±40ms population prior is z = 10
        // (saturating). anomaly → 1.
        let mb = fresh();
        let a = mb.dwell_anomaly("a", 500).unwrap();
        assert!(
            a.anomaly > 0.99,
            "expected ~1 anomaly for 500ms cold-start, got {}",
            a.anomaly
        );
    }

    #[test]
    fn user_data_takes_over_population_prior_after_enough_samples() {
        // Train the baseline to expect long dwells (200ms) on `a`. A
        // 200ms keystroke should then score AS NORMAL (anomaly ~0),
        // not anomalous against the 100ms population prior.
        let mut mb = fresh();
        for _ in 0..1000 {
            mb.observe_key("a", 200);
        }
        let a = mb.dwell_anomaly("a", 200).unwrap();
        // 1000 samples = ~33 test-half-lives → the user's mean should
        // dominate completely.
        assert!(
            approx(a.mean_used, 200.0, 5.0),
            "after 1000 samples at 200ms, mean_used should be ≈200, got {}",
            a.mean_used
        );
        assert!(
            a.anomaly < 0.05,
            "200ms keystroke against a 200ms-trained baseline should be ≈ normal, got {}",
            a.anomaly
        );
        // And a 100ms keystroke against the same trained baseline
        // should now READ AS anomalous (it was perfectly normal pre-
        // training).
        let a2 = mb.dwell_anomaly("a", 100).unwrap();
        assert!(
            a2.anomaly > 0.1,
            "100ms against a 200ms-trained baseline should be anomalous, got {}",
            a2.anomaly
        );
    }

    #[test]
    fn certainty_grows_monotonically_with_samples() {
        let mut prev_certainty = -1.0;
        for n in [1, 5, 30, 100, 1000].iter() {
            let mut local = fresh();
            for _ in 0..*n {
                local.observe_key("a", 100);
            }
            let c = local.dwell_anomaly("a", 100).unwrap().certainty;
            assert!(
                c > prev_certainty,
                "certainty must grow monotonically with sample count; n={} got {} (prev {})",
                n,
                c,
                prev_certainty
            );
            prev_certainty = c;
        }
    }

    #[test]
    fn sparse_key_borrows_from_finger_estimate() {
        // Train heavily on `a` (left pinky) so the finger-level cell
        // is rich. Then ask the anomaly for `q` (also left pinky) —
        // the cell for `q` itself has ZERO samples, but the finger
        // pool says "left pinky dwells around 200ms." A 200ms `q`
        // should therefore read as nearly-normal, NOT anomalous.
        let mut mb = fresh();
        for _ in 0..1000 {
            mb.observe_key("a", 200);
        }
        // `q` has no per-key cell, but it borrows from the finger.
        let q = mb.dwell_anomaly("q", 200).unwrap();
        assert!(
            q.anomaly < 0.5,
            "sparse `q` should borrow from rich left-pinky pool; anomaly = {}",
            q.anomaly
        );
        // certainty for `q` is still low (leaf n_eff = 0).
        assert_eq!(q.certainty, 0.0);
    }

    #[test]
    fn opposite_hand_does_not_pool_in() {
        // Heavy training on left pinky should NOT pull right-pinky's
        // baseline along — the hand-level pool is per-hand.
        let mut mb = fresh();
        for _ in 0..1000 {
            mb.observe_key("a", 200);
        }
        // Right pinky has zero samples. Anomaly for `;` (right pinky)
        // at the population mean (100ms) should be ≈ 0, NOT
        // ≈ "anomalous because the LEFT-pinky baseline is 200ms".
        let semicolon = mb.dwell_anomaly(";", 100).unwrap();
        assert!(
            semicolon.anomaly < 0.05,
            "right pinky shouldn't inherit left-pinky baseline; anomaly={}",
            semicolon.anomaly
        );
    }

    #[test]
    fn non_letter_keys_silently_skip() {
        let mut mb = fresh();
        mb.observe_key("Escape", 50);
        mb.observe_key("ArrowLeft", 50);
        // Nothing was recorded — no cells exist.
        assert!(mb.hand_cell(Hand::Left).is_none());
        assert!(mb.hand_cell(Hand::Right).is_none());
        assert!(mb.dwell_anomaly("Escape", 50).is_none());
    }

    #[test]
    fn shifted_letter_pools_with_lowercase() {
        // `A` (shifted) and `a` should share the same per-key cell
        // because they're the same physical key.
        let mut mb = fresh();
        for _ in 0..200 {
            mb.observe_key("a", 200);
        }
        // Now ask about `A` — it should see the cell trained on `a`.
        let a_lower = mb.dwell_anomaly("a", 200).unwrap();
        let a_upper = mb.dwell_anomaly("A", 200).unwrap();
        assert!(
            approx(a_lower.mean_used, a_upper.mean_used, 1e-9),
            "lower/upper case should share the per-key cell"
        );
    }

    // ---- per_finger_reliability — the Layer B seam -----------------------

    #[test]
    fn reliability_is_zero_for_untrained_finger() {
        let mb = fresh();
        let r = mb.per_finger_reliability(Hand::Left, Finger::Pinky);
        assert_eq!(r.score, 0.0);
        assert_eq!(r.n_eff, 0.0);
    }

    #[test]
    fn reliability_is_high_for_consistent_finger() {
        // Same dwell every time → variance → 0 → CV → 0 → score → 1.
        let mut mb = fresh();
        for _ in 0..1000 {
            mb.observe_key("a", 120);
        }
        let r = mb.per_finger_reliability(Hand::Left, Finger::Pinky);
        assert!(
            r.score > 0.7,
            "consistent finger should be high-reliability; got {}",
            r.score
        );
        assert!(r.n_eff > 30.0);
    }

    #[test]
    fn reliability_is_low_for_jittery_finger() {
        // Alternating 60ms and 180ms → high variance → CV ≈ 0.5 → score low.
        let mut mb = fresh();
        for i in 0..2000 {
            let dwell = if i % 2 == 0 { 60 } else { 180 };
            mb.observe_key("a", dwell);
        }
        let r = mb.per_finger_reliability(Hand::Left, Finger::Pinky);
        assert!(
            r.score < 0.3,
            "jittery finger should be low-reliability; got {}",
            r.score
        );
    }
}
