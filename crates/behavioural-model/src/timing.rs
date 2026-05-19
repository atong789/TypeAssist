use crate::events::InputEvent;

#[derive(Debug, Default)]
pub struct TimingAggregator {
    // Per-key rolling mean + variance of dwell, flight times between keys, etc.
}

impl TimingAggregator {
    pub fn observe(&mut self, _event: &InputEvent) {
        todo!("update per-key timing statistics")
    }
}
