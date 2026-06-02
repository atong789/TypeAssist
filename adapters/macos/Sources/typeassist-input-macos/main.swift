import Foundation
import CoreFoundation

let bridge = Bridge()

// Skeleton-only: the daemon sets TYPEASSIST_AX_PROMPT=1 so macOS pops the
// Accessibility dialog when the permission is missing. Production keeps the
// prompt suppressed — L5 owns that conversation (see Accessibility.swift).
let promptForAccessibility = ProcessInfo.processInfo.environment["TYPEASSIST_AX_PROMPT"] == "1"

guard Accessibility.isTrusted(prompt: promptForAccessibility) else {
    bridge.emit(.permissionRequired)
    // Stay alive briefly so the parent process can read the event,
    // then exit non-zero so the parent can decide to relaunch after the user grants.
    Thread.sleep(forTimeInterval: 0.25)
    exit(2)
}

let tap = EventTap(bridge: bridge)
guard tap.start() else {
    FileHandle.standardError.write(Data("failed to install CGEventTap\n".utf8))
    exit(3)
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
}
CFRunLoopAddTimer(CFRunLoopGetMain(), heartbeatTimer, .commonModes)

// Emit one immediately so the engine doesn't wait the full interval
// to see its first proof of life.
bridge.emit(.heartbeat(
    timestampMs: UInt64(Date().timeIntervalSince1970 * 1000),
    tapEnabled: tap.isTapEnabled()
))

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
