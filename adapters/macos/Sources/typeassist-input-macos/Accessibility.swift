import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
import IOKit.hid

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

    /// Read-only, no-prompt check of the Input Monitoring ("Listen Events")
    /// grant — a SEPARATE TCC permission from Accessibility (see main.swift for
    /// why the two are split and revoked independently). `IOHIDCheckAccess`
    /// only reports the current state; unlike `IOHIDRequestAccess` it never
    /// shows a dialog, so it is safe to poll on the heartbeat. This is the
    /// per-grant signal the onboarding / Reconnect permission rows read.
    static func inputMonitoringGranted() -> Bool {
        return IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted
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

    /// **Anchored replace** (M3 bubble accept-after-typing-on): Left × `left` (to
    /// just after the word), Backspace × `deleteCount` (the word), type
    /// `replacement` (the target, no boundary), then Right × `right` to restore
    /// the caret. Same layout-independent typing as `injectCorrection`. The whole
    /// burst (arrows + backspaces + chars) echoes back through the tap and is
    /// dropped engine-side by count (`pending_echo`).
    static func injectAnchored(left: Int, deleteCount: Int, replacement: String, right: Int) {
        guard let source = CGEventSource(stateID: .combinedSessionState) else { return }
        let leftArrow: CGKeyCode = 123
        let rightArrow: CGKeyCode = 124
        let backspace: CGKeyCode = 51
        for _ in 0..<max(0, left) { tapKey(leftArrow, source: source) }
        for _ in 0..<max(0, deleteCount) { tapKey(backspace, source: source) }
        for character in replacement { typeCharacter(character, source: source) }
        for _ in 0..<max(0, right) { tapKey(rightArrow, source: source) }
    }

    /// Post a key-down/key-up pair for a hardware key code. The echo is dropped
    /// engine-side by count (see `pending_echo` in engine.rs).
    private static func tapKey(_ keyCode: CGKeyCode, source: CGEventSource) {
        CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: true)?
            .post(tap: .cgSessionEventTap)
        CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: false)?
            .post(tap: .cgSessionEventTap)
    }

    /// US-layout characters that are produced by holding Shift: every shifted
    /// symbol on the ANSI keyboard. Uppercase letters are handled separately via
    /// `isUppercase` (covers the full alphabet without enumerating it).
    private static let shiftedSymbols: Set<Character> = [
        "~", "!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "_", "+",
        "{", "}", "|", ":", "\"", "<", ">", "?",
    ]

    /// True when posting `character` on the US layout would require the Shift
    /// key held — any uppercase letter, or a shifted symbol.
    private static func requiresShift(_ character: Character) -> Bool {
        character.isUppercase || shiftedSymbols.contains(character)
    }

    /// Post a single character as a Unicode keystroke (layout-independent).
    ///
    /// QA-21: native Cocoa reads the `keyboardSetUnicodeString` payload directly,
    /// so case survives regardless of modifier flags. Chromium/Electron surfaces
    /// (browser Gmail/Docs, Claude desktop) instead reconstruct the character
    /// from the virtual keycode + modifier flags, so a shift-requiring character
    /// posted with NO `.maskShift` flag lands in its unshifted (lowercase) form —
    /// `Setting` → `setting`. Setting `.maskShift` on both the down and up events
    /// for those characters makes the web layer reconstruct the shifted form; it
    /// does NOT change what Cocoa inserts (still the Unicode payload), so native
    /// is unaffected. Char COUNT is unchanged, so the count-based echo-skip
    /// (`pending_echo` in engine.rs) is untouched.
    private static func typeCharacter(_ character: Character, source: CGEventSource) {
        let units = Array(String(character).utf16)
        guard let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
              let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false) else {
            return
        }
        if requiresShift(character) {
            down.flags = .maskShift
            up.flags = .maskShift
        }
        units.withUnsafeBufferPointer { buffer in
            down.keyboardSetUnicodeString(stringLength: units.count, unicodeString: buffer.baseAddress)
            up.keyboardSetUnicodeString(stringLength: units.count, unicodeString: buffer.baseAddress)
        }
        down.post(tap: .cgSessionEventTap)
        up.post(tap: .cgSessionEventTap)
    }

    // MARK: - TF-08 host-scoped suppression (injection dead-zone gate)
    //
    // TF-08 established that a synthetic delete+retype correction cannot land in a
    // Safari web/contenteditable surface on Intel: its AX caret is a pinned phantom
    // (`loc=1`, writes hit a ghost AXTextArea — probe TF-08), so injection garbles
    // or no-ops. Neither HID-tap nor pacing fixed placement (both tried and
    // rejected). The fix is host-scoped suppression: in that confirmed dead zone
    // the engine goes WATCH-ONLY (keeps learning, withholds the bubble).
    //
    // The condition is derived, NOT hardcoded-behaviour-in-the-engine: the sidecar
    // (the only place OS/AX calls live) reads arch + frontmost bundle + focused
    // role and reports a single content-free bool. It is deliberately a STRUCTURAL
    // gate, not a runtime caret-trust signal, because caret-untrustworthiness does
    // NOT predict injection failure — Chrome-web likely vends an equally poor AX
    // caret yet its keystroke injection WORKS, so a pure caret signal would
    // wrongly suppress Chrome. Gating on arch+app+field is the only signal that
    // structurally cannot fire outside the confirmed dead zone (Chrome = different
    // bundle; native = different app/field; Apple Silicon = different arch).

    /// True when the focused field is the confirmed injection dead zone:
    /// **Intel (x86_64) + Safari frontmost + focused role `AXTextArea`** (the
    /// web/contenteditable surface that vends the phantom caret — Docs / Gmail /
    /// WhatsApp Web all match). Raw structural fact — the `suppress` toggle is
    /// applied ENGINE-side (a faceless L1 adapter must not depend on reading the
    /// user's config dir; that mismatch was the TF-08b `emit=false` bug). Content-
    /// blind: reads the frontmost bundle id (ephemeral, never persisted — Principle
    /// #8) and the focused element's ROLE only, never its text. On Apple Silicon the
    /// `#if` compiles this to `false`, so the arm64 slice can never suppress.
    static func isInjectionDeadZone() -> Bool {
        #if arch(x86_64)
        primeAXConnection()
        guard let front = NSWorkspace.shared.frontmostApplication,
              front.bundleIdentifier == "com.apple.Safari" else { return false }
        let appEl = AXUIElementCreateApplication(front.processIdentifier)
        guard let f = copyAttr(appEl, kAXFocusedUIElementAttribute as String),
              CFGetTypeID(f) == AXUIElementGetTypeID() else { return false }
        let focused = f as! AXUIElement
        let role = (copyAttr(focused, kAXRoleAttribute as String) as? String) ?? ""
        return role == "AXTextArea"
        #else
        return false
        #endif
    }

    // MARK: - TF-08b web-host detection (weld suppression gate)
    //
    // In a web/contenteditable host (Google Docs/Gmail in any browser, on any
    // arch), a competing WEB autocorrect can shrink a word a beat before Jordan's
    // accept, so her dead-reckoned delete count over-deletes into the previous word
    // — the weld. Read-back can't rescue it (content-blind wall + TF-08's phantom
    // caret), and Jordan is web-blind to the competitor. So the engine WITHHOLDS
    // only length-changing corrections (`len(typed) != len(target)`, the exact
    // weld-risk set) in web hosts, ceding that word to the host. This is the L1
    // half: report whether the focused field is a web host.
    //
    // "Web host" = the focused element has an `AXWebArea` ancestor. That role marks
    // rendered web content in WebKit (Safari) and Chromium (Chrome/Edge/Electron),
    // and is ABSENT for native fields (TextEdit/Notes are `AXTextArea` with no web
    // ancestor) — so this fires in browsers/Electron and never in native apps,
    // regardless of arch (Chrome-web welds on Silicon too). Content-blind: reads
    // element ROLES only, never text.

    private static let webAncestorMaxHops = 12

    /// Browser bundle ids — the FALLBACK web-host signal for canvas/iframe editors
    /// (Google Docs) whose degraded AX subtree defeats the AXWebArea ancestor walk.
    /// Docs/Gmail/any web tab runs in one of these, and the bundle id is readable
    /// regardless of how the page vends AX. (Electron web apps still detect via the
    /// AXWebArea walk, so they don't need to be listed.)
    private static let browserBundleIDs: Set<String> = [
        "com.apple.Safari", "com.apple.SafariTechnologyPreview",
        "com.google.Chrome", "com.google.Chrome.canary", "com.google.Chrome.beta",
        "com.microsoft.edgemac", "com.brave.Browser", "com.brave.Browser.beta",
        "org.mozilla.firefox", "com.operasoftware.Opera", "com.vivaldi.Vivaldi",
        "company.thebrowser.Browser", "com.kagi.kagimacOS",
        "com.duckduckgo.macos.browser",
    ]

    /// Whether the focused field is a web content host, plus a rich detail string
    /// for the validation log. Two signals, OR'd:
    ///   1. **AXWebArea ancestor** — walk the focused element's `AXParent` chain for
    ///      an `AXWebArea` (rendered web content). Works for normal contenteditable
    ///      (Gmail) and Electron; runtime-derived. But **Google Docs** is a
    ///      canvas-rendered editor whose input target sits in an offscreen iframe
    ///      with a degraded AX subtree, so the walk dead-ends before AXWebArea.
    ///   2. **Browser bundle id** (fallback) — the frontmost app is a known browser.
    ///      Rescues Docs (and any web surface the walk misses) since the bundle id
    ///      doesn't depend on the page's AX tree.
    /// Content-blind: reads element ROLES / SUBROLES and the app bundle id (an
    /// ephemeral, never-persisted read — Principle #8), never text. The `detail`
    /// records the full role chain + why the walk stopped, so a dev run confirms
    /// exactly where Docs' AX chain breaks.
    static func webHostDetail() -> (isWeb: Bool, detail: String) {
        primeAXConnection()
        guard let front = NSWorkspace.shared.frontmostApplication else {
            return (false, "no-frontmost")
        }
        let bundle = front.bundleIdentifier ?? "?"
        let isBrowser = browserBundleIDs.contains(bundle)

        func roleSub(_ e: AXUIElement) -> String {
            let r = (copyAttr(e, kAXRoleAttribute as String) as? String) ?? "?"
            let s = (copyAttr(e, kAXSubroleAttribute as String) as? String) ?? "-"
            return s == "-" ? r : "\(r)/\(s)"
        }

        guard let f = copyAttr(
                AXUIElementCreateApplication(front.processIdentifier),
                kAXFocusedUIElementAttribute as String),
              CFGetTypeID(f) == AXUIElementGetTypeID() else {
            // No focused element to walk — the browser fallback still decides.
            return (isBrowser, "bundle=\(bundle) browser=\(isBrowser) focused=none")
        }

        var el = f as! AXUIElement
        var chain = [roleSub(el)]
        var foundWebArea = false
        var stop = "depth-cap"
        for _ in 0..<webAncestorMaxHops {
            guard let p = copyAttr(el, kAXParentAttribute as String),
                  CFGetTypeID(p) == AXUIElementGetTypeID() else {
                stop = "nil-parent"
                break
            }
            let parent = p as! AXUIElement
            chain.append(roleSub(parent))
            if (copyAttr(parent, kAXRoleAttribute as String) as? String) == "AXWebArea" {
                foundWebArea = true
                stop = "found-AXWebArea"
                break
            }
            el = parent
        }

        let isWeb = foundWebArea || isBrowser
        let method = foundWebArea ? "webarea" : (isBrowser ? "browser-fallback" : "none")
        return (
            isWeb,
            "bundle=\(bundle) browser=\(isBrowser) method=\(method) stop=\(stop) "
                + "chain=\(chain.joined(separator: ">"))"
        )
    }

    // MARK: - Phase 0 / M3 overlay feasibility probe

    private static var axPrimed = false

    /// Connect this faceless CLI sidecar to the WindowServer so AX IPC works.
    /// A tool with no `NSApplication` can pass `AXIsProcessTrusted()` and run a
    /// `CGEventTap`, yet still get `kAXErrorCannotComplete` (-25204) on every
    /// `AXUIElement` *read* — the system-wide focused-element query needs the
    /// app-server connection that instantiating `NSApplication` establishes.
    /// Touching `NSApplication.shared` forces that connection (the Swift
    /// equivalent of the old `NSApplicationLoad()`). Must run on the main
    /// thread — both probe entry points (the run-loop timer and the
    /// main-dispatched stdin command) do. Lazy; never on the normal capture
    /// path. Idempotent.
    static func primeAXConnection() {
        if axPrimed { return }
        axPrimed = true
        _ = NSApplication.shared
    }

    /// Content-blind text-geometry probe of the currently focused element,
    /// tried two ways so Phase 0 can see which path an app vends geometry on:
    ///   A. the **system-wide** element (`AXUIElementCreateSystemWide`)
    ///   B. the **frontmost app's own** element (`AXUIElementCreateApplication`)
    /// On Tahoe (macOS 26) the system-wide path returns -25204 from a sidecar
    /// even with a working Accessibility grant (its `CGEventTap` captures
    /// fine); the per-app path is the robust route. Reporting both, with each
    /// path's `AXError`, turns "it failed" into a per-app feasibility matrix.
    /// (-25204=cannotComplete, -25208=notImplemented, -25211=APIDisabled,
    /// -25212=noValue — i.e. the app vends no focused UI element.)
    ///
    /// CONTENT-BLIND BY CONSTRUCTION (Principle #9): reads role,
    /// `AXSelectedTextRange` (positions only) and `AXBoundsForRange` (rects
    /// only). Never calls `AXStringForRange`; never reads, returns, or logs
    /// typed text — only motor/geometry shape. Emits nothing itself (caller
    /// routes it to stderr), so the stdout JSON event contract is untouched.
    static func probeFocusedGeometry() -> String {
        primeAXConnection()

        let front = NSWorkspace.shared.frontmostApplication
        let bundle = front?.bundleIdentifier ?? "?"
        let pid = front?.processIdentifier ?? 0

        let a = focusedGeometry(of: AXUIElementCreateSystemWide())
        let b = pid != 0
            ? focusedGeometry(of: AXUIElementCreateApplication(pid))
            : GeomResult(role: "—", word: nil, err: 0)

        let verdict: String
        if a.word != nil || b.word != nil {
            verdict = "WORD-RECT ✓ (\(a.word != nil ? "sysWide" : "appEl"))"
        } else {
            verdict = "UNAVAILABLE"
        }
        return "AXPROBE app=\(bundle)"
            + " sysWide[role=\(a.role) word=\(rectStr(a.word)) err=\(a.err)]"
            + " appEl[role=\(b.role) word=\(rectStr(b.word)) err=\(b.err)]"
            + " ⇒ \(verdict)"
    }

    private struct GeomResult {
        var role: String
        var word: CGRect?
        var err: Int32
    }

    /// Focused element under `root` → its role + the on-screen rect of the
    /// 4 chars before the caret (the unit a per-word mark anchors to).
    private static func focusedGeometry(of root: AXUIElement) -> GeomResult {
        var focusedRef: CFTypeRef?
        let ferr = AXUIElementCopyAttributeValue(root, kAXFocusedUIElementAttribute as CFString, &focusedRef)
        guard ferr == .success, let fref = focusedRef, CFGetTypeID(fref) == AXUIElementGetTypeID() else {
            return GeomResult(role: "NONE", word: nil, err: ferr.rawValue)
        }
        let focused = fref as! AXUIElement
        let role = (copyAttr(focused, kAXRoleAttribute as String) as? String) ?? "?"
        guard let sel = selectedRange(focused) else {
            return GeomResult(role: role, word: nil, err: 0)  // no caret/selection range
        }
        let word = boundsForRange(focused, CFRange(location: max(0, sel.location - 4), length: 4))
        return GeomResult(role: role, word: word, err: 0)
    }

    private static func copyAttr(_ el: AXUIElement, _ attr: String) -> CFTypeRef? {
        var v: CFTypeRef?
        return AXUIElementCopyAttributeValue(el, attr as CFString, &v) == .success ? v : nil
    }

    private static func asAXValue(_ v: CFTypeRef?) -> AXValue? {
        guard let v = v, CFGetTypeID(v) == AXValueGetTypeID() else { return nil }
        return (v as! AXValue)
    }

    private static func selectedRange(_ el: AXUIElement) -> CFRange? {
        guard let axv = asAXValue(copyAttr(el, kAXSelectedTextRangeAttribute as String)) else { return nil }
        var r = CFRange()
        return AXValueGetValue(axv, .cfRange, &r) ? r : nil
    }

    private static func boundsForRange(_ el: AXUIElement, _ range: CFRange) -> CGRect? {
        var r = range
        guard let axRange = AXValueCreate(.cfRange, &r) else { return nil }
        var out: CFTypeRef?
        let err = AXUIElementCopyParameterizedAttributeValue(
            el, kAXBoundsForRangeParameterizedAttribute as CFString, axRange, &out)
        guard err == .success, let axv = asAXValue(out) else { return nil }
        var rect = CGRect.zero
        return AXValueGetValue(axv, .cgRect, &rect) ? rect : nil
    }

    private static func rectStr(_ r: CGRect?) -> String {
        guard let r = r else { return "—" }
        return String(format: "(%.0f,%.0f %.0f×%.0f)", r.origin.x, r.origin.y, r.size.width, r.size.height)
    }
}
