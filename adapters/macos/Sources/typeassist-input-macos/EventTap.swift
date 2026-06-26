import Carbon
import CoreGraphics
import Foundation

final class EventTap {
    private let bridge: Bridge
    private let secureMonitor: SecureFieldMonitor
    private var tap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var keyDownTimestamps: [Int64: UInt64] = [:]

    /// True while a Shift is held with nothing else pressed since — the window in
    /// which an isolated Shift TAP (the correction-accept gesture) can complete.
    /// Set when Shift goes down alone (`.flagsChanged`), cleared by any real key
    /// or mouse-down (a `Shift+key` chord is not a tap). On Shift release while
    /// still armed we emit `.shiftTap`.
    private var shiftArmed = false

    /// Verbose per-keystroke `GATE_READ` diagnostics, off unless
    /// `TYPEASSIST_LOG_GATE=1`. Read once at init so the hot path is a bool
    /// check, not an env lookup. The `SECURE_FIELD_DROP` / `SECURE_FIELD_FOCUS`
    /// lines (low-volume, only on drops/transitions) always log.
    private let logGateReads =
        ProcessInfo.processInfo.environment["TYPEASSIST_LOG_GATE"] == "1"

    init(bridge: Bridge, secureMonitor: SecureFieldMonitor) {
        self.bridge = bridge
        self.secureMonitor = secureMonitor
    }

    func start() -> Bool {
        // Key events for capture; mouse-DOWNs for Fix-B caret-move detection.
        // A trackpad click IS a mouse-down, so left/right/other cover trackpad
        // too. We only need the click happened, never where — no coordinates.
        let mask = (1 << CGEventType.keyDown.rawValue)
            | (1 << CGEventType.keyUp.rawValue)
            | (1 << CGEventType.flagsChanged.rawValue)
            | (1 << CGEventType.leftMouseDown.rawValue)
            | (1 << CGEventType.rightMouseDown.rawValue)
            | (1 << CGEventType.otherMouseDown.rawValue)

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
        case .injectAnchored(let left, let deleteCount, let replacement, let right):
            Accessibility.injectAnchored(
                left: left, deleteCount: deleteCount, replacement: replacement, right: right)
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

        // **Fix-B caret-move**: a mouse / trackpad click can reposition the
        // caret with no keystroke the engine can dead-reckon. Emit a
        // content-free CaretMoved so the engine resets its line model before a
        // live correction could fire backspaces at the wrong spot. Emitted
        // regardless of secure-field state (it carries no typed content), and
        // BEFORE the keycode/secure-gate path (a click is not a key).
        switch type {
        case .leftMouseDown, .rightMouseDown, .otherMouseDown:
            shiftArmed = false // a click cancels an in-progress isolated-Shift tap
            bridge.emit(.caretMoved(reason: "mouse"))
            return
        case .flagsChanged:
            // Modifier-key transition (no character). Drive the isolated-Shift-tap
            // detector. Content-free, so handled before the secure-field gate.
            handleFlagsChanged(event)
            return
        default:
            break
        }

        // A real key press cancels an isolated-Shift accept window — Shift+key is a
        // chord, not a tap. Done BEFORE the secure-field gate so a password
        // keystroke cancels it too (and no .shiftTap fires mid-password).
        if type == .keyDown {
            shiftArmed = false
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

        // Per-key diagnostic (opt-in via TYPEASSIST_LOG_GATE=1): exactly the
        // booleans the gate is about to act on, plus the reader thread id —
        // keycode only, never the character. Off by default so normal runs
        // aren't flooded with one line per keystroke; enable it to confirm the
        // tap sees `secureField=true` in a password field (and reader/writer
        // threads line up).
        if logGateReads {
            FileHandle.standardError.write(Data(
                "GATE_READ keycode=\(keycode) secureField=\(secureField) secureInput=\(secureInput) tid=\(threadID())\n".utf8))
        }

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
            // **Auto-repeat is load-bearing for backspace.** Holding a key fires
            // repeated `.keyDown` events but only ONE `.keyUp` on release. Text
            // keys are emitted on `.keyUp` (so dwell = press-to-release), which
            // means an auto-repeated keyDown is a keystroke the engine never
            // sees. When a user HOLDS backspace to erase a word — the target
            // user's most common erase gesture — the OS deletes N characters but
            // the engine only ever heard one, so its dead-reckoned line buffer
            // desyncs and glues separate retype attempts together (the
            // stale-line-buffer / `edndd` class). Fix: emit backspace on EVERY
            // keyDown (incl. repeats). Backspace dwell is unused (the engine
            // treats it as 0), so moving it off keyUp loses nothing — and it is
            // NOT re-emitted on keyUp below.
            let isRepeat = event.getIntegerValueField(.keyboardEventAutorepeat) != 0
            if keycode == 51 {
                bridge.emit(.backspace(timestampMs: timestampMs))
            } else {
                keyDownTimestamps[keycode] = timestampNs
                // Any OTHER key on auto-repeat is still emitted once on keyUp, so
                // its repeats are dropped. Emit a content-free marker (Principle
                // #7: capture must see its own loss) — it also reveals whether
                // held *letter* keys drop in real use, the only thing that would
                // justify moving every key onto keyDown later.
                if isRepeat {
                    bridge.emit(.autorepeatDropped)
                }
            }
        case .keyUp:
            // Keycode 51 = backspace — emitted on keyDown now (see above),
            // never here. (Nothing was stored in keyDownTimestamps for it.)
            if keycode == 51 {
                break
            }
            let dwellMs: UInt32
            if let downNs = keyDownTimestamps.removeValue(forKey: keycode) {
                dwellMs = UInt32((timestampNs - downNs) / 1_000_000)
            } else {
                dwellMs = 0
            }

            if keycode == 53 {
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

    /// Isolated-Shift-tap detector, driven by `.flagsChanged` transitions.
    /// Arms when Shift goes down with NO other modifier; emits `.shiftTap` when
    /// Shift releases while still armed (cleared meanwhile by any real key /
    /// mouse-down / chord). Caps Lock and Fn are ignored — only Ctrl/Opt/Cmd
    /// count as "another modifier" that turns a tap into a chord.
    private func handleFlagsChanged(_ event: CGEvent) {
        let flags = event.flags
        let shiftDown = flags.contains(.maskShift)
        let otherMods = flags.contains(.maskControl)
            || flags.contains(.maskAlternate)
            || flags.contains(.maskCommand)
        if shiftDown {
            // Arm only when Shift is the sole modifier; a chord (e.g. Shift+Cmd)
            // is never an accept tap.
            shiftArmed = !otherMods
        } else {
            // Shift released. Fire only on a clean isolated tap.
            if shiftArmed && !otherMods {
                let timestampMs = UInt64(event.timestamp / 1_000_000)
                bridge.emit(.shiftTap(timestampMs: timestampMs))
            }
            shiftArmed = false
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
