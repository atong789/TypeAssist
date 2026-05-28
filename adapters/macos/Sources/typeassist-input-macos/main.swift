import Foundation

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

// Capture-health heartbeat — fires on the main run loop every
// `HEARTBEAT_INTERVAL_SECONDS`, reporting the tap's current enabled
// state to the engine. The engine's watchdog uses these to detect
// silent capture death (tap dead OR sidecar wedged) independent of
// whether the user is currently typing.
let HEARTBEAT_INTERVAL_SECONDS: TimeInterval = 2.0
let heartbeatTimer = Timer.scheduledTimer(withTimeInterval: HEARTBEAT_INTERVAL_SECONDS, repeats: true) { _ in
    let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
    bridge.emit(.heartbeat(timestampMs: nowMs, tapEnabled: tap.isTapEnabled()))
}
// Schedule on `.common` modes so the timer fires while modal panels
// (rare in a sidecar, but defensive) don't pause it.
RunLoop.current.add(heartbeatTimer, forMode: .common)
// Emit one immediately so the engine doesn't wait the full interval
// to see its first proof of life.
bridge.emit(.heartbeat(
    timestampMs: UInt64(Date().timeIntervalSince1970 * 1000),
    tapEnabled: tap.isTapEnabled()
))

RunLoop.current.run()
