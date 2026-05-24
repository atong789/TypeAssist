use crate::events::InputEvent;

/// Tracks left- vs right-hand reliability differences.
///
/// Safe no-op for now — fills in when its round lands. `observe` accepts
/// every event so `BehaviouralModel::ingest` can dispatch uniformly without
/// special-casing which aggregators are real yet.
#[derive(Debug, Default)]
pub struct AsymmetryTracker {}

impl AsymmetryTracker {
    pub fn observe(&mut self, _event: &InputEvent) {}
}
