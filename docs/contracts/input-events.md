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

Emitted on **keyDown, including OS auto-repeat** — so a held backspace produces one event per deletion. (Text keys are emitted on keyUp to measure dwell, but backspace dwell is unused, so it rides keyDown to keep the engine's dead-reckoned line buffer in step with reality when the user holds it down.)

```json
{ "type": "backspace", "timestamp_ms": 1729000000123 }
```

### `autorepeat_dropped`

Emitted when the adapter **drops a non-backspace auto-repeat keyDown**. A held text key fires repeated OS keyDowns, but the adapter only emits that key once (on keyUp, for dwell), so the repeats are lost. This content-free marker lets the engine's capture funnel count that loss instead of hiding it (Principle #7), and reveals whether held *letter* keys drop in real use. Carries no key, no count, no timing. Backspace is exempt — it forwards every keyDown (above).

```json
{ "type": "autorepeat_dropped" }
```

### `space_observed`

Emitted on every **deliberate (non-auto-repeat) space keyDown**, carrying the adapter's cumulative count of such presses. The keyDown is observed reliably even when the space's text key (emitted later, on keyUp) is lost, so the core reconciles this running total against the spaces it actually receives to flag an intermittently **dropped space** (the QA-15 Google-Docs weld). **Observe-only** — drives no behaviour. Content-free (Principle #8/#9): a count, never the character, its timing, or the focused app.

```json
{ "type": "space_observed", "total": 42 }
```

### `injection_zone`

**TF-08 host-scoped suppression.** Reports whether the focused field is a place a synthetic delete+retype correction **cannot** land. The confirmed case is a Safari web/contenteditable surface on **Intel (x86_64)**, whose AX caret is a pinned phantom (`loc=1`, no tracking — TF-08 probe), so blind dead-reckoned injection garbles or no-ops. In the dead zone the core goes **watch-only**: it keeps observing/learning slips but withholds the bubble entirely (no cue), rather than fire a "worse autocorrect" (Principle #9). Derived in the adapter from arch + frontmost bundle (Safari) + focused role (`AXTextArea`) — a structural gate that cannot fire on Chrome (different bundle), native apps (different app/field), or Apple Silicon (different arch). The adapter sets `dead: true` only when the field is a dead zone **and** the `suppress` toggle is on, so the pre-signal / toggle-off default is `false`. A **level** signal, re-emitted on focus change only when it flips. Content-free (Principle #8): a single bool, never the app id, field role, or arch.

```json
{ "type": "injection_zone", "dead": true }
```

### `shift_tap`

An **isolated Shift tap** — Shift pressed and released with no other key in between (either Shift, reachable one-handed). The accept gesture for a pending correction suggestion (the M3 bubble). The adapter derives it from `.flagsChanged` transitions and cancels the in-progress tap on any real key / mouse-down, so a `Shift+key` chord never produces it. Content-free.

```json
{ "type": "shift_tap", "timestamp_ms": 1729000000123 }
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
