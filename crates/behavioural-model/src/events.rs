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
    /// **Capture-health heartbeat.** Emitted by the adapter on a
    /// periodic timer (every ~2s) regardless of whether the user is
    /// typing, so the engine has positive proof of life independent
    /// of keystrokes. `tap_enabled` reports the adapter's view of its
    /// own event-tap state — true when capture is active, false when
    /// the OS disabled the tap (e.g. by timeout) and the adapter
    /// hasn't yet re-armed it. The engine's watchdog uses this plus
    /// the heartbeat timestamp to drive the `capture-health` state.
    Heartbeat { timestamp_ms: u64, tap_enabled: bool },
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
    /// Replace the just-typed word: delete `delete_count` characters from the
    /// focused field, then type `replacement`. `replacement` includes the
    /// trailing word-boundary character (e.g. the space) so the adapter stays
    /// dumb — it does no word logic of its own.
    InjectCorrection { delete_count: u32, replacement: String },
    /// Shut down the adapter.
    Shutdown,
    /// **Soft capture restart.** Ask the adapter to tear down its
    /// current event tap and create a fresh one. Used when the
    /// sidecar is alive (heartbeats still arriving) but its tap is
    /// disabled and auto-re-enable hasn't recovered — gives the user
    /// a manual recovery path that doesn't require a process restart.
    /// On success, the adapter resumes emitting heartbeats with
    /// `tap_enabled: true` and key events; on failure the heartbeat
    /// stays `tap_enabled: false` and the engine escalates to a
    /// hard restart (respawn the adapter process).
    RestartTap,
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

    #[test]
    fn heartbeat_event_round_trips() {
        // The Swift sidecar emits this exact shape every 2s. The
        // engine watchdog parses it without losing precision —
        // timestamp_ms is u64 (millisecond-since-epoch), tap_enabled
        // is the sidecar's own view of the CGEvent tap state.
        let e = InputEvent::Heartbeat {
            timestamp_ms: 1_700_000_000_000,
            tap_enabled: true,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(
            json,
            r#"{"type":"heartbeat","timestamp_ms":1700000000000,"tap_enabled":true}"#
        );
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);

        // tap_enabled: false is what the sidecar sends when its tap
        // has been disabled (by timeout or user-input signal) AND
        // auto-re-enable hasn't recovered it. The engine reads this
        // as "Unhealthy" capture state.
        let down = InputEvent::Heartbeat {
            timestamp_ms: 1_700_000_000_000,
            tap_enabled: false,
        };
        let json = serde_json::to_string(&down).unwrap();
        assert!(json.contains("\"tap_enabled\":false"));
    }

    #[test]
    fn restart_tap_command_round_trips() {
        // The engine writes this exact shape to the sidecar's stdin
        // when the user clicks "Restart capture" (and as part of the
        // hard-restart escalation path).
        let c = OutboundCommand::RestartTap;
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"type":"restart_tap"}"#);
        let back: OutboundCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
