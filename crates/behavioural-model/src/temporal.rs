use crate::events::InputEvent;
use volatility_map::TimeOfDay;

/// Distinct profiles for morning / afternoon / evening / night.
#[derive(Debug, Default)]
pub struct TemporalProfiles {}

impl TemporalProfiles {
    pub fn observe(&mut self, _event: &InputEvent) {
        todo!("update the profile bucket for the current time-of-day")
    }

    pub fn current_bucket(&self, _now_ms: u64) -> TimeOfDay {
        todo!("derive the active time-of-day bucket")
    }
}
