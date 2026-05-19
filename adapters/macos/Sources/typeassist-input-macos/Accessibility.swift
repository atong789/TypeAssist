import ApplicationServices
import Foundation

enum Accessibility {
    /// Returns true if this process has been granted Accessibility permission.
    /// We pass `false` for `kAXTrustedCheckOptionPrompt` so we do NOT show the
    /// system prompt from the sidecar — the Tauri UI surfaces that flow itself.
    static func isTrusted() -> Bool {
        let opts: NSDictionary = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as NSString: false]
        return AXIsProcessTrustedWithOptions(opts as CFDictionary)
    }

    /// Replace the currently-being-typed word with `word`.
    /// Implementation will likely use a combination of CGEvent keystroke synthesis
    /// and AX-API focused-element introspection. Stubbed for now.
    static func injectCorrection(_ word: String) {
        // TODO: synthesize backspace×N + new keystrokes against the focused field.
        _ = word
    }
}
