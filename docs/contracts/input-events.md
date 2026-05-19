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

Emitted at startup if the OS-level Accessibility permission is missing. The sidecar exits with status 2 immediately after.

```json
{ "type": "permission_required" }
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

Replace the currently-being-typed word with `word`. Implementation is sidecar-specific (on macOS: backspaces + keystroke synthesis against the focused field via AX).

```json
{ "type": "inject_correction", "word": "hello" }
```

### `shutdown`

Tell the sidecar to stop the event tap and exit. EOF on stdin has the same effect.

```json
{ "type": "shutdown" }
```

## Versioning

There is no explicit `version` field on the wire today; the event-type tags are versioned by their `type` string. Breaking changes will introduce a new type rather than mutate an existing shape. If the wire grows complex enough to warrant a version header, that change must be tracked in both `events.rs` and `Bridge.swift` in the same PR.
