import CoreGraphics
import Foundation

final class EventTap {
    private let bridge: Bridge
    private var tap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var keyDownTimestamps: [Int64: UInt64] = [:]
    /// M3 SUGGEST flow: while true, the tap CONSUMES the Tab key (returns nil)
    /// so the focused app never receives it — accepting a suggestion can't move
    /// browser focus. Toggled by arm/disarmTabShield from the engine. Set on the
    /// run-loop thread (the callback's thread), so no locking needed.
    private var tabShieldArmed = false

    /// Tab's hardware keycode (kVK_Tab).
    private static let tabKeycode: Int64 = 48

    init(bridge: Bridge) {
        self.bridge = bridge
    }

    func start() -> Bool {
        let mask = (1 << CGEventType.keyDown.rawValue) | (1 << CGEventType.keyUp.rawValue)

        let selfPtr = Unmanaged.passUnretained(self).toOpaque()
        // ACTIVE tap (`.defaultTap`, not `.listenOnly`) so the callback's return
        // value can CONSUME an event (return nil) — needed for the Tab shield.
        // Every other event is passed through unchanged.
        guard let port = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: CGEventMask(mask),
            callback: EventTap.callback,
            userInfo: selfPtr
        ) else {
            return false
        }

        let src = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, port, 0)
        CFRunLoopAddSource(CFRunLoopGetCurrent(), src, .commonModes)
        CGEvent.tapEnable(tap: port, enable: true)

        self.tap = port
        self.runLoopSource = src
        return true
    }

    func handleCommand(_ command: OutboundCommand) {
        switch command {
        case .injectCorrection(let deleteCount, let replacement):
            Accessibility.injectCorrection(deleteCount: deleteCount, replacement: replacement)
        case .shutdown:
            stop()
            exit(0)
        case .restartTap:
            // Soft capture restart — engine writes this when auto-
            // re-enable hasn't recovered the tap (or the user clicked
            // "Restart capture"). Tear down the current tap fully,
            // then build a new one. Logged to stderr so a crash-loop
            // is visible from the host's process inspector.
            FileHandle.standardError.write(Data("restarting CGEventTap on request\n".utf8))
            stop()
            if !start() {
                FileHandle.standardError.write(Data("CGEventTap restart failed\n".utf8))
            }
        case .axProbe:
            // Phase 0 / M3 feasibility — content-blind geometry probe of the
            // focused element, reported on stderr (stdout is the JSON event
            // contract). Runs through the sidecar's working AX grant.
            FileHandle.standardError.write(Data((Accessibility.probeFocusedGeometry() + "\n").utf8))
        case .armTabShield:
            tabShieldArmed = true
        case .disarmTabShield:
            tabShieldArmed = false
        }
    }

    /// Current view of the tap's enabled state — used by the
    /// heartbeat emitter to tell the engine whether capture is
    /// actually live. Reads the CGEvent state directly rather than
    /// caching it, so a tap disabled OUT FROM UNDER US (by the OS,
    /// without our callback firing for some reason) still reports
    /// truthfully.
    func isTapEnabled() -> Bool {
        guard let tap = self.tap else { return false }
        return CGEvent.tapIsEnabled(tap: tap)
    }

    private func stop() {
        if let tap = self.tap { CGEvent.tapEnable(tap: tap, enable: false) }
        if let src = self.runLoopSource { CFRunLoopRemoveSource(CFRunLoopGetCurrent(), src, .commonModes) }
        self.tap = nil
        self.runLoopSource = nil
    }

    private static let callback: CGEventTapCallBack = { _, type, event, userInfo in
        guard let userInfo = userInfo else { return Unmanaged.passUnretained(event) }
        let me = Unmanaged<EventTap>.fromOpaque(userInfo).takeUnretainedValue()
        // `process` returns true to CONSUME the event (Tab shield); otherwise
        // the event passes through to the focused app unchanged.
        let consume = me.process(type: type, event: event)
        return consume ? nil : Unmanaged.passUnretained(event)
    }

    /// Returns `true` if the event should be consumed (dropped before the app).
    private func process(type: CGEventType, event: CGEvent) -> Bool {
        // **Self-heal**: macOS disables our tap and delivers ONE of these
        // event types when a callback runs long (`tapDisabledByTimeout`)
        // or on certain user-input / system signals
        // (`tapDisabledByUserInput`). Without action the tap stays dead
        // and no keystrokes ever flow again — this was the long-session
        // capture-death bug. Re-arm immediately.
        //
        // CGEventType's `.rawValue` is used here because the enum is
        // sparsely populated; the disabled-* cases aren't always
        // matched by `switch type` directly in older SDKs.
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            if let tap = self.tap {
                CGEvent.tapEnable(tap: tap, enable: true)
                FileHandle.standardError.write(
                    Data("CGEventTap re-enabled after \(type == .tapDisabledByTimeout ? "timeout" : "user-input") disable\n".utf8)
                )
            }
            return false
        }

        // NOTE: our own injection echo is dropped on the ENGINE side by an exact
        // count of injected events (see `pending_echo` in engine.rs), NOT here —
        // an `eventSourceUserData` tag did not survive the post→tap round-trip
        // in practice, so this tap forwards everything and the engine filters
        // deterministically.

        let keycode = event.getIntegerValueField(.keyboardEventKeycode)
        let timestampNs = event.timestamp
        let timestampMs = UInt64(timestampNs / 1_000_000)

        switch type {
        case .keyDown:
            keyDownTimestamps[keycode] = timestampNs
            // Consume the Tab key-DOWN while armed so the app never reacts to
            // it. The accept is reported to the engine on key-UP (below).
            if tabShieldArmed && keycode == Self.tabKeycode {
                return true
            }
        case .keyUp:
            let dwellMs: UInt32
            if let downNs = keyDownTimestamps.removeValue(forKey: keycode) {
                dwellMs = UInt32((timestampNs - downNs) / 1_000_000)
            } else {
                dwellMs = 0
            }

            // Keycode 51 = delete/backspace
            if keycode == 51 {
                bridge.emit(.backspace(timestampMs: timestampMs))
            } else if keycode == 53 {
                // Keycode 53 = Escape. `keyboardGetUnicodeString` returns an
                // EMPTY string for it (it's not a text-producing key), so the
                // engine would never see it. Emit an explicit U+001B — the
                // codepoint the engine's M3 correction-undo window matches on
                // (KEY_ESCAPE). Modifiers are carried so the engine can require
                // a *bare* Escape.
                bridge.emit(.key(
                    key: "\u{1b}",
                    timestampMs: timestampMs,
                    modifiers: Self.modifiers(for: event),
                    dwellMs: dwellMs
                ))
            } else {
                let key = Self.keyString(for: event)
                bridge.emit(.key(
                    key: key,
                    timestampMs: timestampMs,
                    modifiers: Self.modifiers(for: event),
                    dwellMs: dwellMs
                ))
                // M3 SUGGEST: while armed, the engine still RECEIVES the Tab
                // (emitted above) so it can accept the suggestion, but the app
                // must NOT — consume the key-UP. Tab ("\t") is a normal key
                // string, so the engine matches on KEY_TAB when a suggestion is
                // live; outside the armed window this falls through (return
                // false) and Tab passes to the app normally.
                if tabShieldArmed && keycode == Self.tabKeycode {
                    return true
                }
            }
        default:
            break
        }
        return false
    }

    private static func keyString(for event: CGEvent) -> String {
        var length = 0
        var chars = [UniChar](repeating: 0, count: 4)
        event.keyboardGetUnicodeString(maxStringLength: 4, actualStringLength: &length, unicodeString: &chars)
        return String(utf16CodeUnits: chars, count: length)
    }

    private static func modifiers(for event: CGEvent) -> Modifiers {
        let flags = event.flags
        return Modifiers(
            shift: flags.contains(.maskShift),
            control: flags.contains(.maskControl),
            option: flags.contains(.maskAlternate),
            command: flags.contains(.maskCommand),
            capsLock: flags.contains(.maskAlphaShift),
            function: flags.contains(.maskSecondaryFn)
        )
    }
}
