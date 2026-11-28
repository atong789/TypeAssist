// Break-it lock for the accept check: the shipped `AcceptGuard` against a real
// TextEdit with REAL macOS autocorrect. Run via ./run.sh — never directly.
//
// Keys are posted straight to the throwaway TextEdit process, so they never pass
// the session event tap (an installed Jordan cannot see or learn from them).
import AppKit

let pid = pid_t(CommandLine.arguments[1])!
let autocorrect = CommandLine.arguments[2] == "on"
let app = AXUIElementCreateApplication(pid)
func attr(_ e: AXUIElement, _ a: String) -> CFTypeRef? {
    var v: CFTypeRef?
    return AXUIElementCopyAttributeValue(e, a as CFString, &v) == .success ? v : nil
}
let titles = (attr(app, kAXWindowsAttribute as String) as? [AXUIElement] ?? [])
    .map { attr($0, kAXTitleAttribute as String) as? String ?? "?" }
precondition(titles == ["accept-lock.txt"], "not the throwaway instance: \(titles)")
let field = attr(app, kAXFocusedUIElementAttribute as String) as! AXUIElement
func text() -> String { attr(field, kAXValueAttribute as String) as? String ?? "<nil>" }
func wait(_ s: Double) {
    let end = CFAbsoluteTimeGetCurrent() + s
    while CFAbsoluteTimeGetCurrent() < end { CFRunLoopRunInMode(.defaultMode, 0.005, false) }
}

var lastKey = 0.0
let guardUnderTest = AcceptGuard(
    readCaret: { AcceptGuard.caret(ofApp: pid) },
    secondsSinceKeyDown: { CFAbsoluteTimeGetCurrent() - lastKey })

let source = CGEventSource(stateID: .privateState)
/// Post one key and report it to the guard, as the event tap would.
func key(_ code: CGKeyCode, _ ch: Character? = nil) {
    lastKey = CFAbsoluteTimeGetCurrent()
    guardUnderTest.keyDown(isSpace: code == 0 && ch == " ")
    for down in [true, false] {
        let e = CGEvent(keyboardEventSource: source, virtualKey: code, keyDown: down)!
        if let ch = ch {
            var u = Array(String(ch).utf16)
            e.keyboardSetUnicodeString(stringLength: u.count, unicodeString: &u)
        }
        e.postToPid(pid)
    }
}
func type(_ s: String, gap: Double = 0.1) { for c in s { key(0, c); wait(gap) } }
func fresh() {
    AXUIElementSetAttributeValue(field, kAXValueAttribute as CFString, "" as CFString)
    wait(0.2)
    guardUnderTest.caretRelocated()
    wait(0.2)
}
/// A host rewrite with no keystroke, made deterministically through AX.
func hostRewrite(_ location: Int, _ length: Int, _ with: String, caret: Int) {
    func select(_ l: Int, _ n: Int) {
        var r = CFRange(location: l, length: n)
        AXUIElementSetAttributeValue(field, kAXSelectedTextRangeAttribute as CFString, AXValueCreate(.cfRange, &r)!)
    }
    select(location, length)
    AXUIElementSetAttributeValue(field, kAXSelectedTextAttribute as CFString, with as CFString)
    select(caret, 0)
    wait(0.2)
}
func perform(_ d: AcceptGuard.Decision, _ replacement: String) {
    guard case .inject(let delete, _) = d else { return }
    guardUnderTest.willInject(delete: delete, replacement: replacement)
    for _ in 0..<delete { key(51) }
    for c in replacement { key(0, c) }
    wait(0.6)
}

var failures = 0
func expect(_ name: String, _ ok: Bool, _ detail: String) {
    print("\(ok ? "PASS" : "FAIL")  [autocorrect \(autocorrect ? "on" : "off")] \(name) — \(detail)")
    if !ok { failures += 1 }
}

// The re-split, as the engine sends it for `No tthink `.
func drift() -> AcceptGuard.Decision { guardUnderTest.checkAccept(delete: 8, expected: 7, alternate: 6) }

if autocorrect {
    // REAL macOS autocorrect — a bonus check. macOS decides when it fires (and
    // stops after a correction is rejected), so an unfired context is skipped,
    // and nothing here reverts a correction.
    var exercised = 0
    for left in ["No", "So", "Go", "On", "Do", "To"] {
        fresh(); type("\(left) tthink "); wait(0.6)
        guard text() == "\(left) think " else { continue }
        let d = drift(); perform(d, "t think ")
        expect("REAL autocorrect, re-split", text() == "\(left)t think ", "\(d) → \(text().debugDescription)")
        exercised += 1
        break
    }
    for (typed, fixed, delete, alternate) in [("tthink", "think", 7, 6), ("becaus", "because", 7, 8),
                                              ("woould", "would", 7, 6), ("libary", "library", 7, 8)] {
        fresh(); type("I \(typed) "); wait(0.6)
        guard text() == "I \(fixed) " else { continue }
        let d = guardUnderTest.checkAccept(delete: delete, expected: delete, alternate: alternate)
        perform(d, "\(fixed) ")
        expect("REAL autocorrect, single word \(typed)", text() == "I \(fixed) ", "\(d) → \(text().debugDescription)")
        exercised += 1
    }
    print("REAL autocorrect cases exercised: \(exercised)")
    fresh()
    exit(failures == 0 ? 0 : 1)
}

// ---- Deterministic cases (autocorrect off; the host rewrite is simulated) ----

fresh(); type("No tthink ")
var d = drift(); perform(d, "t think ")
expect("re-split accept, field untouched", text() == "Not think ", "\(d) → \(text().debugDescription)")
d = guardUnderTest.checkRevert(delete: 8); perform(d, " tthink ")
expect("Esc revert restores the typed words", text() == "No tthink ", "\(d) → \(text().debugDescription)")

fresh(); type("No tthink "); hostRewrite(3, 6, "think", caret: 9)
d = drift(); perform(d, "t think ")
expect("re-split accept, host shortened the word", text() == "Not think ", "\(d) → \(text().debugDescription)")

fresh(); type("I tthink "); hostRewrite(2, 6, "think", caret: 8)
d = guardUnderTest.checkAccept(delete: 7, expected: 7, alternate: 6); perform(d, "think ")
expect("single word, host shortened", text() == "I think ", "\(d) → \(text().debugDescription)")

fresh(); type("No becaus "); hostRewrite(3, 6, "because", caret: 11)
d = guardUnderTest.checkAccept(delete: 7, expected: 7, alternate: 8); perform(d, "because ")
expect("single word, host lengthened", text() == "No because ", "\(d) → \(text().debugDescription)")

// Sealed by punctuation, not a space.
fresh(); type("No zzqx."); wait(0.6)
d = guardUnderTest.checkAccept(delete: 5, expected: 5, alternate: 5); perform(d, "word.")
expect("sealed by punctuation", text() == "No word.", "\(d) → \(text().debugDescription)")

// An unexplained length must abort and leave the field alone.
fresh(); type("No zzqxv "); hostRewrite(3, 5, "zz", caret: 6)
let before = text(); d = guardUnderTest.checkAccept(delete: 6, expected: 6, alternate: 7); perform(d, "word ")
expect("length mismatch aborts", d == .abort(reason: "length_changed") && text() == before,
       "\(d) → \(text().debugDescription)")

// Revert after the field moved on must abort.
fresh(); type("No zzqxv "); perform(guardUnderTest.checkAccept(delete: 6, expected: 6, alternate: 6), "zzqxw ")
hostRewrite(0, 2, "Nope", caret: (text() as NSString).length + 2)
let edited = text(); d = guardUnderTest.checkRevert(delete: 6); perform(d, "zzqxv ")
expect("revert after a change aborts", d == .abort(reason: "length_changed") && text() == edited,
       "\(d) → \(text().debugDescription)")

// The next letter beats the baseline read: no baseline, never a wrong one.
fresh(); type("No zzqxv ", gap: 0.005); wait(0.3)
d = guardUnderTest.checkAccept(delete: 6, expected: 6, alternate: 6)
expect("fast typing aborts", d == .abort(reason: "no_baseline"), "\(d)")

// A selection must abort.
fresh(); type("No zzqxv "); hostRewrite(3, 0, "", caret: 9)
var sel = CFRange(location: 3, length: 2)
AXUIElementSetAttributeValue(field, kAXSelectedTextRangeAttribute as CFString, AXValueCreate(.cfRange, &sel)!)
wait(0.2); d = guardUnderTest.checkAccept(delete: 6, expected: 6, alternate: 6)
expect("selection aborts", d == .abort(reason: "selection"), "\(d)")

fresh()
exit(failures == 0 ? 0 : 1)
