import ApplicationServices
import CoreGraphics
import Foundation

enum Accessibility {
    /// Returns true if this process has been granted Accessibility permission.
    ///
    /// Pass `prompt: true` to let macOS show the system "…would like to control
    /// this computer" dialog when the permission is missing. Production callers
    /// pass `false` so the Tauri UI (L5) owns that conversation; the walking
    /// skeleton passes `true` so it can be granted with no UI running.
    static func isTrusted(prompt: Bool = false) -> Bool {
        let opts: NSDictionary = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as NSString: prompt]
        return AXIsProcessTrustedWithOptions(opts as CFDictionary)
    }

    /// Replace the just-typed word in the focused field: synthesize
    /// `deleteCount` backspaces, then type `replacement` (which already
    /// includes the trailing boundary character).
    ///
    /// Typing goes through `keyboardSetUnicodeString` so it is independent of
    /// the user's keyboard layout — we post the characters, not key positions.
    static func injectCorrection(deleteCount: Int, replacement: String) {
        guard let source = CGEventSource(stateID: .combinedSessionState) else { return }

        let backspace: CGKeyCode = 51
        for _ in 0..<max(0, deleteCount) {
            tapKey(backspace, source: source)
        }

        for character in replacement {
            typeCharacter(character, source: source)
        }
    }

    /// Post a key-down/key-up pair for a hardware key code.
    private static func tapKey(_ keyCode: CGKeyCode, source: CGEventSource) {
        CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: true)?
            .post(tap: .cgSessionEventTap)
        CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: false)?
            .post(tap: .cgSessionEventTap)
    }

    /// Post a single character as a Unicode keystroke (layout-independent).
    private static func typeCharacter(_ character: Character, source: CGEventSource) {
        let units = Array(String(character).utf16)
        guard let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
              let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false) else {
            return
        }
        units.withUnsafeBufferPointer { buffer in
            down.keyboardSetUnicodeString(stringLength: units.count, unicodeString: buffer.baseAddress)
            up.keyboardSetUnicodeString(stringLength: units.count, unicodeString: buffer.baseAddress)
        }
        down.post(tap: .cgSessionEventTap)
        up.post(tap: .cgSessionEventTap)
    }
}
