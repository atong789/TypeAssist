import CoreGraphics
import Foundation

final class EventTap {
    private let bridge: Bridge
    private var tap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var keyDownTimestamps: [Int64: UInt64] = [:]

    init(bridge: Bridge) {
        self.bridge = bridge
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
        case .injectCorrection(let word):
            Accessibility.injectCorrection(word)
        case .shutdown:
            stop()
            exit(0)
        }
    }

    private func stop() {
        if let tap = self.tap { CGEvent.tapEnable(tap: tap, enable: false) }
        if let src = self.runLoopSource { CFRunLoopRemoveSource(CFRunLoopGetCurrent(), src, .commonModes) }
    }

    private static let callback: CGEventTapCallBack = { _, type, event, userInfo in
        guard let userInfo = userInfo else { return Unmanaged.passUnretained(event) }
        let me = Unmanaged<EventTap>.fromOpaque(userInfo).takeUnretainedValue()
        me.process(type: type, event: event)
        return Unmanaged.passUnretained(event)
    }

    private func process(type: CGEventType, event: CGEvent) {
        let keycode = event.getIntegerValueField(.keyboardEventKeycode)
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
