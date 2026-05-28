//! Layer 2 — the behavioural model.
//!
//! Consumes a stream of [`InputEvent`]s from a Layer-1 adapter, dispatches
//! each event to five sub-aggregators, and exposes a `snapshot()` of the
//! current state for diagnostics and (eventually) L3 projection.
//!
//! Right now `timing` is the only aggregator with real logic; the other four
//! are safe no-ops, so L2 observes everything without affecting behaviour.
//! The correction decision still lives in L4's walking-skeleton lookup.

pub mod asymmetry;
pub mod events;
pub mod fatigue;
pub mod ghost_keys;
pub mod motor_baseline;
pub mod slip_detector;
pub mod temporal;
pub mod timing;

pub use events::{InputEvent, Modifiers, OutboundCommand};
pub use slip_detector::{PairSlipRow, SlipDetector, SlipEvent, SlipsSnapshot};

use serde::{Deserialize, Serialize};

/// Inter-key intervals longer than this are treated as **pauses** (thinking,
/// reading, glancing away) rather than typing motor speed, and excluded from
/// the running aggregates. The anchor timestamp still advances, so the next
/// genuinely-fast interval after a pause is counted normally.
///
/// Picked at the slow edge of active typing: 1500 ms ≈ 40 WPM minimum (5
/// chars × 60 / 1.5 = 200 chars/min). Real typing is faster than this even
/// for slow typists; anything above it is almost certainly a pause.
///
/// Shared by `timing` and `asymmetry` so the two aggregators apply the same
/// rule. If we ever need a tighter cap for a specific surface, give it its
/// own constant rather than relaxing this one.
pub const MAX_TYPING_INTERVAL_MS: u64 = 1500;

/// The aggregate state Layer 2 maintains. Layer 3 will eventually project this
/// into a `VolatilityMap`; for now `snapshot()` returns a diagnostic
/// [`ModelSnapshot`] for the debug view.
#[derive(Debug, Default)]
pub struct BehaviouralModel {
    pub timing: timing::TimingAggregator,
    pub ghost_keys: ghost_keys::GhostKeyTracker,
    pub asymmetry: asymmetry::AsymmetryTracker,
    pub fatigue: fatigue::FatigueCurve,
    pub temporal: temporal::TemporalProfiles,
    /// First L3 learning loop: detects backspace-correction slips and
    /// writes them into a live `VolatilityMap`.
    pub slip_detector: slip_detector::SlipDetector,
    /// **C5c Layer A.** Per-user motor baseline — recency-weighted
    /// dwell + IKI per (hand, finger, key) with hierarchical
    /// shrinkage and a population prior. Observe-only this phase;
    /// L4's `measure_token_motor` does NOT consume it yet (that's
    /// Phase 2 of 5c).
    pub motor_baseline: motor_baseline::MotorBaseline,
}

impl BehaviouralModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fan an event out to every sub-aggregator. The four that aren't real yet
    /// no-op, so adding them to this dispatch costs nothing and means every
    /// aggregator goes live the moment its module is filled in.
    pub fn ingest(&mut self, event: &InputEvent) {
        self.timing.observe(event);
        self.ghost_keys.observe(event);
        self.asymmetry.observe(event);
        self.fatigue.observe(event);
        self.temporal.observe(event);
        self.slip_detector.observe(event);
        self.motor_baseline.observe(event);
    }

    /// Drain slips detected during ingest since the last call. The engine
    /// host emits these as `engine://slip` Tauri events for the debug view.
    pub fn take_new_slips(&mut self) -> Vec<slip_detector::SlipEvent> {
        self.slip_detector.take_new_slips()
    }

    /// Diagnostic snapshot of the live aggregator state.
    ///
    /// Separate from the future L3 `VolatilityMap` projection — this is what
    /// the builder's debug view consumes, not what L4 will read.
    pub fn snapshot(&self) -> ModelSnapshot {
        ModelSnapshot {
            timing: self.timing.snapshot(),
            asymmetry: self.asymmetry.snapshot(),
            ghost_keys: self.ghost_keys.snapshot(),
            slips: self.slip_detector.snapshot(),
            motor_baseline: self.motor_baseline.snapshot(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSnapshot {
    pub timing: timing::TimingSnapshot,
    pub asymmetry: asymmetry::AsymmetrySnapshot,
    pub ghost_keys: ghost_keys::GhostKeysSnapshot,
    pub slips: slip_detector::SlipsSnapshot,
    pub motor_baseline: motor_baseline::MotorBaselineSnapshot,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Modifiers;

    #[test]
    fn ingest_routes_key_events_to_timing() {
        let mut model = BehaviouralModel::new();
        model.ingest(&InputEvent::Key {
            key: "a".into(),
            timestamp_ms: 100,
            modifiers: Modifiers::default(),
            dwell_ms: 80,
        });
        let snap = model.snapshot();
        assert_eq!(snap.timing.per_key.len(), 1);
        assert_eq!(snap.timing.per_key[0].key, "a");
        assert_eq!(snap.timing.per_key[0].count, 1);
    }

    #[test]
    fn ingest_does_not_panic_on_any_event_variant() {
        // The four placeholder aggregators must accept every variant without
        // panicking — observe-only contract for this round.
        let mut model = BehaviouralModel::new();
        for ev in [
            InputEvent::Ready,
            InputEvent::PermissionRequired,
            InputEvent::Shutdown,
            InputEvent::Backspace { timestamp_ms: 1 },
            InputEvent::Key {
                key: "x".into(),
                timestamp_ms: 2,
                modifiers: Modifiers::default(),
                dwell_ms: 50,
            },
        ] {
            model.ingest(&ev);
        }
    }

    #[test]
    fn ingest_routes_key_events_to_motor_baseline() {
        // C5c Layer A wiring — same shape as the timing routing test
        // but checks the new MotorBaseline. Snapshot must surface a
        // per-finger row for left-pinky (`a`), with a positive n_eff
        // after one keystroke.
        let mut model = BehaviouralModel::new();
        model.ingest(&InputEvent::Key {
            key: "a".into(),
            timestamp_ms: 100,
            modifiers: Modifiers::default(),
            dwell_ms: 80,
        });
        let snap = model.snapshot();
        // Snapshot always includes all 10 fingers in anatomical order.
        assert_eq!(snap.motor_baseline.per_finger.len(), 10);
        let lpinky = snap
            .motor_baseline
            .per_finger
            .iter()
            .find(|r| {
                r.hand == volatility_map::Hand::Left
                    && r.finger == volatility_map::Finger::Pinky
            })
            .expect("left pinky row must exist");
        assert!(lpinky.n_eff > 0.0, "n_eff = {}", lpinky.n_eff);
        assert!(lpinky.dwell_mean_ms.is_some());
        // First keystroke → no prior → IKI is None.
        assert!(lpinky.iki_mean_ms.is_none());
    }

    #[test]
    fn motor_baseline_per_finger_rows_in_anatomical_order() {
        let model = BehaviouralModel::new();
        let snap = model.snapshot();
        // Anatomical order: left pinky first, right pinky last. We just
        // verify the ordering invariant — the panel relies on it.
        let order: Vec<_> = snap
            .motor_baseline
            .per_finger
            .iter()
            .map(|r| (r.hand, r.finger))
            .collect();
        use volatility_map::Finger::{Index, Middle, Pinky, Ring, Thumb};
        use volatility_map::Hand::{Left, Right};
        assert_eq!(
            order,
            vec![
                (Left, Pinky),
                (Left, Ring),
                (Left, Middle),
                (Left, Index),
                (Left, Thumb),
                (Right, Thumb),
                (Right, Index),
                (Right, Middle),
                (Right, Ring),
                (Right, Pinky),
            ]
        );
    }
}
