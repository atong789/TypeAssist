//! Layer 2 — the behavioural model.
//!
//! Consumes a stream of [`InputEvent`]s from a Layer-1 adapter and produces
//! aggregates that L3 (`volatility-map`) snapshots into a `VolatilityMap`.

pub mod asymmetry;
pub mod events;
pub mod fatigue;
pub mod ghost_keys;
pub mod temporal;
pub mod timing;

pub use events::{InputEvent, Modifiers, OutboundCommand};

use volatility_map::VolatilityMap;

/// The aggregate state Layer 2 maintains. Layer 3 reads from this to snapshot a `VolatilityMap`.
#[derive(Debug, Default)]
pub struct BehaviouralModel {
    pub timing: timing::TimingAggregator,
    pub ghost_keys: ghost_keys::GhostKeyTracker,
    pub asymmetry: asymmetry::AsymmetryTracker,
    pub fatigue: fatigue::FatigueCurve,
    pub temporal: temporal::TemporalProfiles,
}

impl BehaviouralModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ingest(&mut self, _event: InputEvent) {
        todo!("ingest event into the relevant sub-aggregators")
    }

    pub fn snapshot(&self, _now_ms: u64) -> VolatilityMap {
        todo!("project aggregates into a versioned VolatilityMap")
    }
}
