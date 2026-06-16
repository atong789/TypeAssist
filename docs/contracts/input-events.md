# L1 ↔ L2 wire format

Line-delimited JSON. Each line is one event (sidecar → core) or one command (core → sidecar). Rust source of truth: `crates/behavioural-model/src/events.rs`. Swift encoder: `adapters/macos/Sources/typeassist-input-macos/Bridge.swift`.

## Sidecar → core (events)

### `key`

A character-producing key event. Emitted on key-up so `dwell_ms` is known.

```json
{
  "type": "key",
  "key": "a",
  "timestamp_ms": 1729000000000,
  "modifiers": {
    "shift": false,
    "control": false,
    "option": false,
    "command": false,
    "caps_lock": false,
    "function": false
  },
  "dwell_ms": 82
}
```

- `key` is the printable Unicode string the OS resolved (e.g. `"a"`, `"A"`, `"é"`). For non-printable keys (arrows, escape, etc.) the sidecar will emit a symbolic name or omit the event; the precise list is TBD.
- `timestamp_ms` is sourced from the OS event clock, monotonic within a session.
- `dwell_ms` is press-to-release duration.

### `backspace`

Broken out from `key` because self-corrections are signal, not noise. L4 records these as evidence of `SelfCorrected` outcomes.

```json
{ "type": "backspace", "timestamp_ms": 1729000000123 }
```

### `permission_required`

Emitted at startup if a required OS-level permission is missing (Accessibility OR Input Monitoring — capture needs both). The sidecar exits with status 2 immediately after. A `permission_status` (below) is emitted just *before* this, so a partial grant is observable even on a spawn that's about to exit.

```json
{ "type": "permission_required" }
```

### `permission_status`

Reports the two **independent** macOS grants the capture pipeline needs — `accessibility` (the AX API: secure-field focus + correction injection) and `input_monitoring` (the CGEventTap that captures keystrokes). Both are read-only, no-prompt checks (`AXIsProcessTrusted` / `IOHIDCheckAccess(kIOHIDRequestTypeListenEvent)`), safe to poll. Emitted **before** the startup permission gates (on every spawn) and again on each `heartbeat`. Unlike the aggregate capture-health `live` flag — which can only be observed once *both* grants are in effect, because the sidecar exits before its first heartbeat otherwise — this surfaces a partial grant, so first-run onboarding and the Reconnect panel can tick each permission row the moment its own grant lands.

```json
{ "type": "permission_status", "accessibility": true, "input_monitoring": false }
```

### `ready`

Emitted once when the event tap is installed and events will start flowing.

```json
{ "type": "ready" }
```

### `shutdown`

Emitted just before the sidecar exits cleanly.

```json
{ "type": "shutdown" }
```

## Core → sidecar (commands)

### `inject_correction`

Replace the just-typed word: delete `delete_count` characters from the focused field, then type `replacement`. `replacement` includes the trailing word-boundary character (e.g. the space), so the adapter does no word logic of its own — the core decides exactly what to remove and what to type. Implementation is sidecar-specific (on macOS: synthesized backspaces + layout-independent Unicode keystrokes).

```json
{ "type": "inject_correction", "delete_count": 4, "replacement": "the " }
```

### `shutdown`

Tell the sidecar to stop the event tap and exit. EOF on stdin has the same effect.

```json
{ "type": "shutdown" }
```

## Versioning

There is no explicit `version` field on the wire today; the event-type tags are versioned by their `type` string. Breaking changes will introduce a new type rather than mutate an existing shape. If the wire grows complex enough to warrant a version header, that change must be tracked in both `events.rs` and `Bridge.swift` in the same PR.
