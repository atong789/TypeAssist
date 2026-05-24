use crate::events::InputEvent;

/// Within-session degradation: accuracy and dwell drift over a continuous typing session.
///
/// Safe no-op for now — fills in when its round lands.
#[derive(Debug, Default)]
pub struct FatigueCurve {}

impl FatigueCurve {
    pub fn observe(&mut self, _event: &InputEvent) {}
}
