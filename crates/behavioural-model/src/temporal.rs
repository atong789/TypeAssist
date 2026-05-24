use crate::events::InputEvent;
use volatility_map::TimeOfDay;

/// Distinct profiles for morning / afternoon / evening / night.
///
/// `observe` is a safe no-op for now — fills in when its round lands.
/// `current_bucket` is left as `todo!()` because no live caller derives the
/// bucket yet; the next round that needs it will own that work.
#[derive(Debug, Default)]
pub struct TemporalProfiles {}

impl TemporalProfiles {
    pub fn observe(&mut self, _event: &InputEvent) {}

    pub fn current_bucket(&self, _now_ms: u64) -> TimeOfDay {
        todo!("derive the active time-of-day bucket")
    }
}
