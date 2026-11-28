import ApplicationServices
import CoreGraphics
import Foundation

/// **Accept check (caret length).** The engine dead-reckons from keystrokes, so a
/// host rewrite that emits none (macOS autocorrect: `tthink` → `think`) leaves its
/// delete count wrong and an accept eats the neighbouring text (`Nt think`).
///
/// This measures how long the word *really* is on screen: the caret position is
/// noted where the word starts (after whitespace, a click or a caret key), read
/// again at accept, and the difference is compared with what the engine believes.
///
/// **Content-blind:** reads `AXSelectedTextRange` (two integers) and element
/// identity only — never `AXValue`, never a character. In memory only.
///
/// Anything unprovable aborts. A missed accept is fine; a wrong delete is not.
final class AcceptGuard {
    struct Caret {
        let element: AXUIElement
        let location: Int
        let selectionLength: Int
    }

    enum Decision: Equatable {
        /// Delete exactly this many. `adjusted` = the host had already rewritten
        /// the word to the target's length, so the count moved with it.
        case inject(delete: Int, adjusted: Bool)
        case abort(reason: String)
    }

    /// First baseline read, this long after the boundary. A caret read straight
    /// after a key is stale; TextEdit was measured settling within 31 ms.
    static let settleSeconds = 0.040
    /// Second read; both must agree or the baseline is discarded.
    static let confirmSeconds = 0.015

    private let readCaret: () -> Caret?
    /// Seconds since the session last saw a keyDown — catches a key that reached
    /// the app before our tap callback ran.
    private let secondsSinceKeyDown: () -> Double

    /// Word-start baseline for the word being typed, and for the one before it.
    private var current: Caret?
    private var previous: Caret?
    private var lastKeyWasSpace = false
    /// Bumped by anything that invalidates a sample in flight.
    private var generation = 0
    /// Where our own last injection left the caret (verifies the Esc revert).
    private var afterInjection: Caret?
    /// Our own injected keyDowns still to be ignored, and until when.
    private var echoLeft = 0
    private var echoDeadline = 0.0

    init(readCaret: @escaping () -> Caret?, secondsSinceKeyDown: @escaping () -> Double) {
        self.readCaret = readCaret
        self.secondsSinceKeyDown = secondsSinceKeyDown
    }

    // MARK: - What the tap reports

    /// A real keyDown. `isSpace` covers whitespace that starts a new word.
    func keyDown(isSpace: Bool) {
        if echoLeft > 0 && now() < echoDeadline {
            echoLeft -= 1
            return
        }
        echoLeft = 0
        generation += 1
        lastKeyWasSpace = isSpace
        if isSpace {
            previous = current
            current = nil
            scheduleBaseline()
        }
    }

    /// A click, caret key, shortcut or app/focus change: old baselines are void.
    func caretRelocated() {
        generation += 1
        previous = nil
        current = nil
        afterInjection = nil
        lastKeyWasSpace = false
        scheduleBaseline()
    }

    private func scheduleBaseline() {
        let mine = generation
        let boundaryAt = now()
        after(Self.settleSeconds) { [weak self] in
            guard let self = self, mine == self.generation, let first = self.readCaret() else { return }
            self.after(Self.confirmSeconds) { [weak self] in
                guard let self = self, mine == self.generation,
                      self.secondsSinceKeyDown() >= (self.now() - boundaryAt) - 0.008,
                      let second = self.readCaret(),
                      second.selectionLength == 0,
                      second.location == first.location,
                      CFEqual(second.element, first.element) else { return }
                self.current = second
            }
        }
    }

    // MARK: - Decisions

    /// `expected` = characters from word start to caret if the screen still holds
    /// what was typed; `alternate` = the same if it already holds the target.
    func checkAccept(delete: Int, expected: Int, alternate: Int) -> Decision {
        // Sealed by a space ⇒ the word's baseline is the one before that space;
        // sealed by punctuation ⇒ no new word has started.
        guard let base = lastKeyWasSpace ? previous : current else {
            return .abort(reason: "no_baseline")
        }
        guard let caret = readCaret() else { return .abort(reason: "caret_unreadable") }
        guard CFEqual(caret.element, base.element) else { return .abort(reason: "field_changed") }
        guard caret.selectionLength == 0 else { return .abort(reason: "selection") }
        let onScreen = caret.location - base.location
        if onScreen == expected { return .inject(delete: delete, adjusted: false) }
        let adjusted = delete + (alternate - expected)
        if onScreen == alternate && adjusted >= 0 { return .inject(delete: adjusted, adjusted: true) }
        return .abort(reason: "length_changed")
    }

    /// The Esc revert: the caret must still be exactly where our fix left it.
    func checkRevert(delete: Int) -> Decision {
        guard let left = afterInjection else { return .abort(reason: "no_baseline") }
        guard let caret = readCaret() else { return .abort(reason: "caret_unreadable") }
        guard CFEqual(caret.element, left.element) else { return .abort(reason: "field_changed") }
        guard caret.selectionLength == 0 else { return .abort(reason: "selection") }
        guard caret.location == left.location else { return .abort(reason: "length_changed") }
        return .inject(delete: delete, adjusted: false)
    }

    /// Call just before posting an injection: remembers where it will leave the
    /// caret and ignores its echo. Baselines are void afterwards.
    func willInject(delete: Int, replacement: String) {
        afterInjection = readCaret().map {
            Caret(element: $0.element,
                  location: $0.location - delete + replacement.utf16.count,
                  selectionLength: 0)
        }
        generation += 1
        previous = nil
        current = nil
        echoLeft = delete + replacement.count
        echoDeadline = now() + 1.0
        // The replacement ends in the boundary, so a new word starts there.
        let mine = generation
        after(0.25) { [weak self] in
            guard let self = self, mine == self.generation else { return }
            self.lastKeyWasSpace = false
            self.current = self.readCaret()
        }
    }

    // MARK: - Plumbing

    private func now() -> Double { CFAbsoluteTimeGetCurrent() }

    /// CFRunLoopTimer — the only timer that fires in this binary (see main.swift).
    private func after(_ seconds: Double, _ work: @escaping () -> Void) {
        let timer = CFRunLoopTimerCreateWithHandler(
            kCFAllocatorDefault, now() + seconds, 0, 0, 0) { _ in work() }
        CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, .commonModes)
    }

    // MARK: - Live caret read

    /// The caret of `pid`'s focused element (positions only).
    static func caret(ofApp pid: pid_t) -> Caret? {
        let app = AXUIElementCreateApplication(pid)
        var focused: CFTypeRef?
        guard AXUIElementCopyAttributeValue(app, kAXFocusedUIElementAttribute as CFString, &focused) == .success,
              let f = focused, CFGetTypeID(f) == AXUIElementGetTypeID() else { return nil }
        let element = f as! AXUIElement
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, kAXSelectedTextRangeAttribute as CFString, &value) == .success,
              let v = value, CFGetTypeID(v) == AXValueGetTypeID() else { return nil }
        var range = CFRange()
        guard AXValueGetValue(v as! AXValue, .cfRange, &range) else { return nil }
        return Caret(element: element, location: range.location, selectionLength: range.length)
    }
}
