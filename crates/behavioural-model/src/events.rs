//! L1 ↔ L2 wire format.
//!
//! Layer-1 adapters emit `InputEvent` values as line-delimited JSON on stdout
//! and read `OutboundCommand` values as line-delimited JSON on stdin.
//! See `docs/contracts/input-events.md`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    /// A character-producing key event. `dwell_ms` is the press-to-release duration.
    Key {
        key: String,
        timestamp_ms: u64,
        modifiers: Modifiers,
        dwell_ms: u32,
    },
    /// Backspace is broken out: self-corrections are signal, not noise.
    Backspace { timestamp_ms: u64 },
    /// The adapter is up but lacks the OS permissions it needs to capture events.
    PermissionRequired,
    /// The adapter has finished initial setup and is now emitting key events.
    Ready,
    /// The adapter is shutting down cleanly.
    Shutdown,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
    pub caps_lock: bool,
    pub function: bool,
}

/// Commands the core sends back to a Layer-1 adapter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboundCommand {
    /// Replace the currently-being-typed word with `word`.
    InjectCorrection { word: String },
    /// Shut down the adapter.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_event_round_trips() {
        let e = InputEvent::Key {
            key: "a".into(),
            timestamp_ms: 123,
            modifiers: Modifiers::default(),
            dwell_ms: 80,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn permission_required_serializes_as_documented() {
        let e = InputEvent::PermissionRequired;
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"permission_required"}"#);
    }
}
