use crate::events::InputEvent;

/// Tracks left- vs right-hand reliability differences.
#[derive(Debug, Default)]
pub struct AsymmetryTracker {}

impl AsymmetryTracker {
    pub fn observe(&mut self, _event: &InputEvent) {
        todo!("update per-hand reliability aggregates")
    }
}
