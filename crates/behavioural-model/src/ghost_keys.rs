use crate::events::InputEvent;

/// Tracks pairs like "aimed for T, hit Y instead" — inferred from self-corrections.
#[derive(Debug, Default)]
pub struct GhostKeyTracker {}

impl GhostKeyTracker {
    pub fn observe(&mut self, _event: &InputEvent) {
        todo!("infer aimed-for vs hit-instead pairs from backspace patterns")
    }
}
