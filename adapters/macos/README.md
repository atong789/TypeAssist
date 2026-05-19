# Layer 1 — macOS input adapter

A standalone Swift executable that captures keystrokes via `CGEventTap`, emits
them as line-delimited JSON on **stdout**, and reads correction commands as
line-delimited JSON on **stdin**.

The wire format is the single source of truth for the L1 ↔ L2 contract. See
`docs/contracts/input-events.md` and `crates/behavioural-model/src/events.rs`.

## Build

```bash
cd adapters/macos
swift build -c release
```

The binary lands at `.build/release/typeassist-input-macos`. `just build-sidecar`
runs this, copies the binary into `apps/tauri/src-tauri/sidecars/` with the Tauri
target-triple suffix, and ad-hoc codesigns it.

## Accessibility permission

`CGEventTap` requires the Accessibility permission. Until the user grants it,
event-tap creation will not deliver any events.

On startup the sidecar checks `AXIsProcessTrustedWithOptions` with the prompt
suppressed. If untrusted it emits:

```json
{"type":"permission_required"}
```

…and exits with status 2. The Tauri UI is responsible for guiding the user
through System Settings → Privacy & Security → Accessibility, then relaunching
the sidecar.

We deliberately do not pop the system prompt from the sidecar itself — the
parent app owns that conversation so it can show the user real context first
(why it's needed, what the app does and does not do with the events).

## Codesigning

The OS keys Accessibility permission to the binary's code signature. If you
build and forget to sign, the user has to re-grant the permission every time
the binary identity changes. The justfile runs `codesign --force --deep --sign -`
(ad-hoc) after every build. For shipping a real distributable, swap to a
Developer ID identity.

## Run standalone

You can run the sidecar without Tauri to inspect the event stream:

```bash
./.build/release/typeassist-input-macos
```

Then type into any focused field — you should see `{"type":"key", ...}` lines
on stdout. To test correction injection, paste an `inject_correction` command
on stdin:

```json
{"type":"inject_correction","word":"hello"}
```

## Future adapters

The same wire format applies. A Windows adapter (Rust + `SetWindowsHookEx`) or
an Android IME adapter must emit the exact same JSON events on stdout (or its
equivalent transport) and accept the same commands on stdin.
