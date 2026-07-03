import CoreGraphics
import CoreFoundation
import Foundation

let bridge = Bridge()

// Build marker → stderr (the engine forwards sidecar stderr to its tracing log),
// so the dev Terminal proves WHICH sidecar binary is running: this one emits the
// isolated-Shift accept signal (`.shiftTap`, derived from `.flagsChanged`).
FileHandle.standardError.write(Data("SIDECAR_BUILD shiftTap=enabled flagsChanged=on\n".utf8))

// Skeleton-only: the daemon sets TYPEASSIST_AX_PROMPT=1 so macOS pops the
// Accessibility dialog when the permission is missing. Production keeps the
// prompt suppressed — L5 owns that conversation (see Accessibility.swift).
let promptForAccessibility = ProcessInfo.processInfo.environment["TYPEASSIST_AX_PROMPT"] == "1"

// Emit permission_required + exit non-zero, staying alive just long enough for
// the parent to read the event before deciding to relaunch after a grant. Used
// for BOTH permissions the capture pipeline needs — see below.
func exitNeedingPermission() -> Never {
    bridge.emit(.permissionRequired)
    Thread.sleep(forTimeInterval: 0.25)
    exit(2)
}

// Read-only, no-prompt snapshot of the two independent grants capture needs.
// Emitted HERE — before the permission gates below — so a partial grant is
// visible to the UI even when this spawn is about to exit for the *other*
// missing permission. (The aggregate capture-health `live` flag can't show
// this: it requires both grants, and the process exits before its first
// heartbeat if either is missing.) The onboarding / Reconnect permission rows
// read this to tick each grant independently. Re-emitted on each heartbeat
// (below) so a long-running process reflects a runtime revoke too.
func emitPermissionStatus() {
    bridge.emit(.permissionStatus(
        accessibility: Accessibility.isTrusted(),
        inputMonitoring: Accessibility.inputMonitoringGranted()
    ))
}
emitPermissionStatus()

// Capture needs TWO independent TCC grants on macOS 10.15+, revoked separately
// (an app update can drop Input Monitoring while leaving Accessibility intact):
//   • Accessibility — the AX API (secure-field focus, correction injection).
//   • Input Monitoring ("Listen Events") — the CGEventTap that CAPTURES keys.
// Re-granting one does NOT restore the other. Without the IM gate a missing IM
// grant makes the tap fail to install (exit 3) with no permission signal — so
// the engine thinks permission is fine and offers a useless "Restart capture"
// that can never recover. Surfacing permission_required instead routes the menu
// to "Reconnect…", which guides the user to the right pane.
//
// ACCESSIBILITY FIRST — gate on it BEFORE touching Input Monitoring. On macOS
// 13–26 an Accessibility grant also satisfies the listen-only tap's Input
// Monitoring requirement (undocumented but consistent Catalina-era subsumption;
// see docs/macos-signing.md), so once AX is granted, CGPreflightListenEventAccess
// returns true on its own and IM never has to prompt. If we requested IM first
// (as an earlier revision did), the IM prompt fired before AX was granted — a
// keystroke prompt with no context. Requesting IM only *after* AX is trusted
// means: pre-AX we exit clean with no IM prompt at all; post-AX IM is already
// satisfied by subsumption; and only on a machine where subsumption is absent do
// we reach CGRequestListenEventAccess() — by then AX is granted, so the request
// is in-context AND it registers the app in the IM pane (driving the onboarding
// conditional row). CGPreflightListenEventAccess() is the pure, never-prompt
// check; CGRequestListenEventAccess() registers + prompts once when undetermined.
guard Accessibility.isTrusted(prompt: promptForAccessibility) else {
    exitNeedingPermission()
}

var inputMonitoringGranted = CGPreflightListenEventAccess()
if !inputMonitoringGranted {
    _ = CGRequestListenEventAccess()
    inputMonitoringGranted = CGPreflightListenEventAccess()
}
guard inputMonitoringGranted else {
    FileHandle.standardError.write(
        Data("Input Monitoring permission missing — grant in System Settings › Privacy & Security › Input Monitoring\n".utf8))
    exitNeedingPermission()
}

// Secure-field gate (Layer 2 cache): start tracking focus BEFORE the tap so
// the "is the focused field secure?" flag is already correct when the first
// keystroke arrives. Layer 1 (global secure input) needs no setup.
let secureMonitor = SecureFieldMonitor(bridge: bridge)
secureMonitor.start()

let tap = EventTap(bridge: bridge, secureMonitor: secureMonitor)
guard tap.start() else {
    // We passed both preflights above, yet the tap still wouldn't install. The
    // overwhelmingly likely cause is Input Monitoring being revoked in the
    // window between the check and tapCreate (or a TCC state that preflight
    // reported stale). Treat it as a permission issue — not a silent exit 3 —
    // so the menu routes to "Reconnect…" rather than a "Restart capture" that
    // can't recover. (The tapCreate attempt also (re)lists us in the pane.)
    FileHandle.standardError.write(Data("failed to install CGEventTap — Input Monitoring likely missing\n".utf8))
    exitNeedingPermission()
}

bridge.emit(.ready)
bridge.runInputLoop(commandHandler: tap.handleCommand(_:))

// Capture-health heartbeat — fires every HEARTBEAT_INTERVAL_SECONDS on
// the main thread's CFRunLoop, alongside the EventTap's CFMachPort
// source. Added in `.commonModes` so any common mode the run loop
// happens to be in services it (matches how the EventTap source is
// registered).
//
// **Why CFRunLoopTimer and not NSTimer / DispatchSourceTimer / GCD
// asyncAfter / Thread+sleep.** All four of those silently fail to
// re-fire in this binary: the immediate emit lands but no scheduled
// fires execute. CFRunLoopTimer is the underlying primitive the
// others wrap, and it works directly when added to the main run
// loop with `.commonModes`. Verified empirically.
let HEARTBEAT_INTERVAL_SECONDS: Double = 2.0
let heartbeatTimer = CFRunLoopTimerCreateWithHandler(
    kCFAllocatorDefault,
    CFAbsoluteTimeGetCurrent() + HEARTBEAT_INTERVAL_SECONDS,
    HEARTBEAT_INTERVAL_SECONDS,
    0, 0
) { _ in
    let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
    bridge.emit(.heartbeat(timestampMs: nowMs, tapEnabled: tap.isTapEnabled()))
    // Refresh the per-grant snapshot alongside proof-of-life so the UI catches
    // a runtime revoke (e.g. Input Monitoring pulled while running) too.
    emitPermissionStatus()
}
CFRunLoopAddTimer(CFRunLoopGetMain(), heartbeatTimer, .commonModes)

// Emit one immediately so the engine doesn't wait the full interval
// to see its first proof of life.
bridge.emit(.heartbeat(
    timestampMs: UInt64(Date().timeIntervalSince1970 * 1000),
    tapEnabled: tap.isTapEnabled()
))
emitPermissionStatus()

// Phase 0 / M3 overlay feasibility probe (opt-in via TYPEASSIST_AX_PROBE=1).
// Mirrors the throwaway spike (`adapters/macos/spike/ax_probe.swift`) but runs
// inside the sidecar so it inherits the *working* Accessibility grant. Logs a
// content-blind geometry line to stderr every interval; click into a text field
// in each target app and watch the stderr stream. NSTimer/Timer silently fail
// to re-fire in this binary (see the heartbeat note above) — CFRunLoopTimer is
// the primitive that works.
if ProcessInfo.processInfo.environment["TYPEASSIST_AX_PROBE"] == "1" {
    FileHandle.standardError.write(Data("AXPROBE mode ON — content-blind geometry probe every 0.5s\n".utf8))
    var lastProbeLine = ""
    let probeTimer = CFRunLoopTimerCreateWithHandler(
        kCFAllocatorDefault, CFAbsoluteTimeGetCurrent() + 0.5, 0.5, 0, 0
    ) { _ in
        let line = Accessibility.probeFocusedGeometry()
        if line != lastProbeLine {  // de-dupe: one line per capability change, like the spike
            lastProbeLine = line
            FileHandle.standardError.write(Data((line + "\n").utf8))
        }
    }
    CFRunLoopAddTimer(CFRunLoopGetMain(), probeTimer, .commonModes)
}

RunLoop.current.run()
