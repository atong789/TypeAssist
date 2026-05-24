use crate::events::InputEvent;

/// Tracks pairs like "aimed for T, hit Y instead" — inferred from self-corrections.
///
/// Safe no-op for now — fills in when its round lands.
#[derive(Debug, Default)]
pub struct GhostKeyTracker {}

impl GhostKeyTracker {
    pub fn observe(&mut self, _event: &InputEvent) {}
}
