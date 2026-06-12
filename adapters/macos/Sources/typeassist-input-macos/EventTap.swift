import Carbon
import CoreGraphics
import Foundation

final class EventTap {
    private let bridge: Bridge
    private let secureMonitor: SecureFieldMonitor
    private var tap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var keyDownTimestamps: [Int64: UInt64] = [:]

    init(bridge: Bridge, secureMonitor: SecureFieldMonitor) {
        self.bridge = bridge
        self.secureMonitor = secureMonitor
    }

    func start() -> Bool {
        let mask = (1 << CGEventType.keyDown.rawValue) | (1 << CGEventType.keyUp.rawValue)

        let selfPtr = Unmanaged.passUnretained(self).toOpaque()
        guard let port = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .listenOnly,
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
        me.process(type: type, event: event)
        return Unmanaged.passUnretained(event)
    }

    private func process(type: CGEventType, event: CGEvent) {
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
            return
        }

        // NOTE: our own injection echo is dropped on the ENGINE side by an exact
        // count of injected events (see `pending_echo` in engine.rs), NOT here —
        // an `eventSourceUserData` tag did not survive the post→tap round-trip
        // in practice, so this tap forwards everything and the engine filters
        // deterministically.

        let keycode = event.getIntegerValueField(.keyboardEventKeycode)

        // **Secure-field gate** — refuse to forward ANY part of a keystroke
        // (character, timing, modifiers, backspace) while a password / secure
        // field is in play, so password content never leaves this capture
        // process. Two independent signals, checked before the event is ever
        // emitted on the wire:
        //   Layer 1 — `IsSecureEventInputEnabled()`: cheap, global; true for
        //     native secure fields, the login window, `sudo` in Terminal.
        //   Layer 2 — `secureMonitor.isSecureFieldFocused`: an AX-focus-driven
        //     cached flag (no per-key AX query) that also catches web /
        //     Electron / custom password fields, which expose an
        //     `AXSecureTextField` subrole but often DON'T trip Layer 1.
        // The whole event is dropped; we never record its keyDown timestamp,
        // so no dwell leaks either. Each signal is read ONCE here.
        let secureInput = IsSecureEventInputEnabled()
        let secureField = secureMonitor.isSecureFieldFocused

        // Per-key diagnostic: exactly the booleans the gate is about to act on,
        // plus the reader thread id — keycode only, never the character. Lets
        // the sandbox confirm the tap sees `secureField=true` while a password
        // field is focused (and that the reader/writer threads line up).
        FileHandle.standardError.write(Data(
            "GATE_READ keycode=\(keycode) secureField=\(secureField) secureInput=\(secureInput) tid=\(threadID())\n".utf8))

        if secureInput || secureField {
            let reason = secureInput ? "secureEventInput" : "axSecureField"
            FileHandle.standardError.write(
                Data("SECURE_FIELD_DROP keycode=\(keycode) reason=\(reason)\n".utf8))
            return
        }

        let timestampNs = event.timestamp
        let timestampMs = UInt64(timestampNs / 1_000_000)

        switch type {
        case .keyDown:
            keyDownTimestamps[keycode] = timestampNs
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
            }
        default:
            break
        }
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
