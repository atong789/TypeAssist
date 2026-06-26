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
{"type":"inject_correction","delete_count":4,"replacement":"the "}
```

## AX geometry probe (Phase 0 / M3 overlay feasibility)

The M3 correction overlay needs an on-screen rect for a character range in the
focused field. There's a **content-blind** probe (role, `AXSelectedTextRange`,
`AXNumberOfCharacters`, `AXBoundsForRange` — never `AXStringForRange`, never the
typed text; Principle #8 holds) built into the sidecar, in
`Accessibility.probeFocusedGeometry()`. Two ways to trigger it:

- **Stdin command** — send `{"type":"ax_probe"}` on stdin; the result prints on
  **stderr** (stdout stays the JSON event contract). Rides the same command path
  as `inject_correction`.
- **Env flag** — set `TYPEASSIST_AX_PROBE=1` and the sidecar logs a probe line to
  stderr every 0.5s (de-duped to one line per capability change).

**Run it through the path that works.** Launch the *whole app* with the flag —
`TYPEASSIST_AX_PROBE=1 just dev` — and watch the sidecar's stderr in the dev
console. The engine propagates the flag to the sidecar spawn (`engine.rs`).

**Why not a standalone probe binary / a Terminal launch.** On macOS 26 (Tahoe) a
Terminal-launched probe — *and the sidecar binary itself when launched from a
Terminal* — returns `kAXErrorCannotComplete` (**-25204**) on every AX query even
though `AXIsProcessTrusted()` returns `true`. That is the tell: **-25204 is not a
permission error** (`kAXErrorAPIDisabled` is **-25211**) — the trust gate passes
and AX *IPC* fails. The variable is the **launch context / TCC responsible
process**, not the binary signature (the spike probe and the sidecar are both
ad-hoc signed) and not notarization (irrelevant to AX at runtime). The sidecar's
working grant comes from being launched by the app, so the probe must run there.
More Accessibility toggles / `tccutil reset` cycles do not fix a -25204.

## Future adapters

The same wire format applies. A Windows adapter (Rust + `SetWindowsHookEx`) or
an Android IME adapter must emit the exact same JSON events on stdout (or its
equivalent transport) and accept the same commands on stdin.
