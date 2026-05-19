use crate::events::InputEvent;

/// Within-session degradation: accuracy and dwell drift over a continuous typing session.
#[derive(Debug, Default)]
pub struct FatigueCurve {}

impl FatigueCurve {
    pub fn observe(&mut self, _event: &InputEvent) {
        todo!("update session fatigue estimate")
    }
}
