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
    /// **Isolated Shift tap** — Shift pressed and released with no other key in
    /// between (either Shift, reachable one-handed). The accept gesture for a
    /// pending correction suggestion (the M3 bubble). The L1 adapter detects it
    /// from `.flagsChanged` transitions and cancels the in-progress tap on any
    /// real key or mouse-down, so this fires ONLY for a clean isolated tap — a
    /// `Shift+key` chord never produces it. Carries no content (Principle #9).
    ShiftTap { timestamp_ms: u64 },
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
    /// **Per-grant permission snapshot.** Reports the two *independent* macOS
    /// grants the capture pipeline needs: `accessibility` (the AX API — focus +
    /// correction injection) and `input_monitoring` (the CGEventTap that captures
    /// keystrokes). Both are read-only, no-prompt checks (`AXIsProcessTrusted`
    /// and `IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)`), safe to poll.
    ///
    /// Distinct from the aggregate `Heartbeat.tap_enabled` / capture-health
    /// `live` flag, which can only be observed once BOTH grants are in effect:
    /// the adapter EXITS at its permission gates before its first heartbeat if
    /// either grant is missing. This snapshot is emitted *before* those gates (on
    /// every spawn) and on each heartbeat, so a PARTIAL grant (e.g. Accessibility
    /// on, Input Monitoring still pending) is observable — first-run onboarding
    /// uses it to tick each permission row the moment its own grant lands.
    PermissionStatus { accessibility: bool, input_monitoring: bool },
    /// **Caret may have moved somewhere the engine can't dead-reckon.**
    /// Emitted by the L1 adapter on a mouse / trackpad click or a focus / app
    /// change — gestures that reposition the caret with no key the engine can
    /// follow. Content-free by construction (Principle #9): no coordinates, no
    /// text, only an optional `reason` tag for logs (`"mouse"` / `"focus"`).
    /// The engine treats it as an immediate line reset so a live correction
    /// can't fire backspaces against a stale line model.
    CaretMoved {
        #[serde(default)]
        reason: Option<String>,
    },
    /// **An auto-repeated keystroke the L1 adapter did NOT forward.** Holding a
    /// (non-backspace) key fires repeated OS keyDowns, but the adapter emits a
    /// text key once, on keyUp, so the repeats are lost. The engine counts these
    /// in its capture funnel so the drop is visible rather than silent (Principle
    /// #7). Content-free by construction (Principle #9): no key, no count, no
    /// timing. (Backspace is exempt — the adapter forwards every backspace
    /// keyDown, so held-backspace deletions are not dropped.)
    AutorepeatDropped,
    /// **Space keyDown observed at the tap (capture-integrity probe, QA-15).**
    /// Emitted on every deliberate (non-auto-repeat) space keyDown the L1 tap
    /// sees, carrying the adapter's cumulative count of such presses. The tap
    /// observes the keyDown reliably even when the matching text key (emitted
    /// later, on keyUp) is lost — so reconciling this running total against the
    /// spaces the engine actually receives makes an intermittent space DROP
    /// visible (the Google-Docs weld). **Observe-only** — drives no behaviour.
    /// Content-free (Principle #8/#9): a count, never the character, its timing,
    /// or the focused app.
    SpaceObserved { total: u64 },
    /// **Injection dead-zone signal (TF-08, host-scoped suppression).** The L1
    /// adapter reports whether the currently focused field is a place a synthetic
    /// delete+retype correction CANNOT land. The confirmed case is a Safari
    /// web/contenteditable surface on Intel (x86_64), whose AX caret is a pinned
    /// phantom (`loc=1`, no tracking — TF-08 probe), so blind dead-reckoned
    /// injection garbles or no-ops. In the dead zone the engine goes WATCH-ONLY:
    /// it keeps observing/learning slips but withholds the bubble entirely (no
    /// cue) — never a "worse autocorrect" (Principle #9). This supersedes the Watch
    /// Dog competition there (nothing is injected, so nothing double-corrects).
    ///
    /// A **level** signal, re-evaluated on the adapter heartbeat and emitted only
    /// when it flips. `dead` is the **raw** structural fact (arch + Safari +
    /// AXTextArea) — the `suppress` toggle is applied engine-side, not by the
    /// adapter. The pre-signal default is `false`, so nothing changes for native
    /// fields, Chrome, or Apple Silicon. Content-free (Principle #8): a single
    /// bool — never the app id, field role, or arch that derived it.
    InjectionZone { dead: bool },
    /// **Web/contenteditable host signal (TF-08b, weld suppression).** The L1
    /// adapter reports whether the focused field lives inside a web content area
    /// (an `AXWebArea` ancestor, OR — fallback — the frontmost app is a browser,
    /// which rescues Google Docs' canvas editor whose degraded AX subtree defeats
    /// the walk). In such a host a competing **web** autocorrect (Google Docs/Gmail)
    /// can shrink a word a beat before Jordan's accept, so Jordan's dead-reckoned
    /// delete count over-deletes into the previous word (the weld). The engine pairs
    /// this with a length-delta test (`len(typed) != len(target)`, the exact
    /// weld-risk set) to WITHHOLD only length-changing corrections in web hosts —
    /// ceding that one word to the host. Same-length slips, and every slip in native
    /// apps, are untouched.
    ///
    /// A **level** signal, re-evaluated on the adapter heartbeat and emitted only
    /// when it flips. `web` is the **raw** web-host fact — the `weld_suppress`
    /// toggle is applied engine-side, not by the adapter (a faceless L1 adapter must
    /// not depend on reading the user's config dir). The pre-signal default is
    /// `false`. Content-free (Principle #8): a single bool, never the app id or
    /// field role that derived it.
    WebHost { web: bool },
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
    /// **Anchored replace (M3 bubble, accept-after-typing-on).** Replace a word
    /// the caret has already moved PAST: move the caret left `left` chars (to just
    /// after the word), delete `delete_count` (the word), type `replacement` (the
    /// target, no boundary), then move right `right` chars to restore the caret.
    /// Positional, not caret-relative — so an accept after the user typed on lands
    /// on the right word. Used only within a strict same-line/distance/time gate.
    InjectAnchored {
        left: u32,
        delete_count: u32,
        replacement: String,
        right: u32,
    },
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
    fn shift_tap_round_trips() {
        // The Swift sidecar emits this on an isolated Shift tap (the accept
        // gesture). snake_case tag + field name must match the Swift bridge.
        let e = InputEvent::ShiftTap {
            timestamp_ms: 1_700_000_000_000,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"shift_tap","timestamp_ms":1700000000000}"#);
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
    fn permission_status_event_round_trips() {
        // The sidecar emits this on every spawn (before its permission gates) and
        // on each heartbeat. Both grants are independent booleans; the snake_case
        // tag + field names must match what the Swift bridge writes.
        let e = InputEvent::PermissionStatus {
            accessibility: true,
            input_monitoring: false,
        };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(
            json,
            r#"{"type":"permission_status","accessibility":true,"input_monitoring":false}"#
        );
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn caret_moved_event_round_trips() {
        // The L1 adapter emits this on a click / focus change. `reason` is a
        // content-free log tag; it must survive the round-trip, and the
        // variant must also parse when `reason` is absent.
        let e = InputEvent::CaretMoved {
            reason: Some("mouse".into()),
        };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""type":"caret_moved""#));
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);

        let bare: InputEvent = serde_json::from_str(r#"{"type":"caret_moved"}"#).unwrap();
        assert_eq!(bare, InputEvent::CaretMoved { reason: None });
    }

    #[test]
    fn autorepeat_dropped_event_round_trips() {
        // The Swift sidecar emits this (content-free) when it drops a
        // non-backspace auto-repeat keyDown. snake_case tag, no fields.
        let e = InputEvent::AutorepeatDropped;
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"autorepeat_dropped"}"#);
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn space_observed_event_round_trips() {
        // The Swift sidecar emits this on each deliberate space keyDown, carrying
        // its running count. snake_case tag + `total` field must match the bridge.
        let e = InputEvent::SpaceObserved { total: 42 };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"space_observed","total":42}"#);
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn injection_zone_event_round_trips() {
        // TF-08: L1 reports whether the focused field is an injection dead zone
        // (host-scoped suppression). snake_case tag + `dead` field must match the
        // Swift bridge's `injection_zone` encoding.
        let e = InputEvent::InjectionZone { dead: true };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"injection_zone","dead":true}"#);
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn web_host_event_round_trips() {
        // TF-08b: L1 reports whether the focused field is a web/contenteditable
        // host (weld suppression). snake_case tag + `web` field must match the
        // Swift bridge's `web_host` encoding.
        let e = InputEvent::WebHost { web: true };
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, r#"{"type":"web_host","web":true}"#);
        let back: InputEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn inject_anchored_command_round_trips() {
        let c = OutboundCommand::InjectAnchored {
            left: 6,
            delete_count: 3,
            replacement: "the".into(),
            right: 6,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(
            json,
            r#"{"type":"inject_anchored","left":6,"delete_count":3,"replacement":"the","right":6}"#
        );
        let back: OutboundCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
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
