import AppKit
import ApplicationServices
import Foundation

/// Tracks whether the currently focused UI element is a secure / password
/// text field, so the capture path can refuse to forward those keystrokes
/// **without** walking the Accessibility tree on every key.
///
/// This is Layer 2 of the secure-field gate (see `EventTap.process`). Layer 1
/// is the cheap global `IsSecureEventInputEnabled()` check, which native
/// password fields, the login window and `sudo` in Terminal all trigger.
/// Layer 2 closes the real gap: **web / Electron / custom** password fields
/// that DON'T flip global secure input still expose an `AXSecureTextField`
/// subrole, so we can see them and drop their keystrokes.
///
/// **Performance.** Focus is event-driven, not polled. We register an
/// `AXObserver` for `kAXFocusedUIElementChangedNotification` on the frontmost
/// app (re-bound whenever the frontmost app changes), evaluate the focused
/// element's role/subrole only on those transitions, and cache the verdict in
/// `isSecureFieldFocused`. The per-keystroke path just reads that `Bool`.
///
/// **Content-blind (Principle #8).** Reads only `AXRole` / `AXSubrole` /
/// `AXFocusedUIElement` — never `AXValue`, never any typed text. The element's
/// *kind* is all we need to decide whether to capture.
final class SecureFieldMonitor {
    /// Cached verdict for the currently-focused element. `false` is the
    /// capture-allowed default; we only ever flip it `true` when AX positively
    /// reports a secure field, so a missing grant or an unreadable element
    /// fails *open to capture* — Layer 1 remains the backstop for anything that
    /// trips global secure input.
    ///
    /// **Cross-thread.** The AX observer writes this and the `CGEventTap`
    /// callback reads it on the hot per-key path. They are NOT guaranteed to be
    /// the same thread, and a plain stored `Bool` has no memory barrier — on
    /// Apple Silicon's weak memory model the tap could read a stale `false`
    /// forever (the bug this fixes). The lock both serialises access and
    /// publishes the write, so the tap sees the latest value.
    /// Sink for the Fix-B `CaretMoved` event emitted on focus/app changes
    /// (the secure gate and this share the one AX observer).
    private let bridge: Bridge

    init(bridge: Bridge) {
        self.bridge = bridge
    }

    private let lock = NSLock()
    private var _isSecureFieldFocused = false

    /// Thread-safe read for the gate. Cheap relative to a keystroke.
    var isSecureFieldFocused: Bool {
        lock.lock()
        defer { lock.unlock() }
        return _isSecureFieldFocused
    }

    private var appObserver: AXObserver?
    private var observedPid: pid_t = 0

    /// Bounded post-focus re-poll. A focus-change notification can arrive
    /// BEFORE a rebuilt web element (page reload) has its `AXSecureTextField`
    /// subrole populated, so the immediate read sees a non-secure element and,
    /// with no further notification for that same element, the flag would never
    /// re-assert. After each focus/selection change we re-read the focused
    /// element a few times over ~1s so the settled subrole wins. This is the
    /// ONLY repeated AX work, and it's bounded to just after a focus change —
    /// the per-keystroke gate still reads a cached flag, never AX.
    private var confirmTimer: CFRunLoopTimer?
    private var confirmFiresLeft = 0
    private static let confirmFireCount = 6        // ~6 re-reads
    private static let confirmInterval = 0.15      // every 150ms ⇒ ~0.9s window

    /// AX subrole string for a password field. WebKit (Safari) and Chromium
    /// (Chrome / Edge / Electron) both vend `<input type=password>` with this
    /// subrole, as does a native `NSSecureTextField`. Hard-coded rather than
    /// referencing `kAXSecureTextFieldSubrole` (not surfaced as a Swift
    /// constant in ApplicationServices on every SDK).
    private static let secureSubrole = "AXSecureTextField"

    /// Begin tracking. Must be called on the main thread (the AX observer's
    /// run-loop source and the `NSWorkspace` notification are serviced by the
    /// main run loop, the same loop the `CGEventTap` callback runs on — so the
    /// cached flag is written and read on one thread, no locking).
    func start() {
        // AX reads from a faceless CLI need the app-server connection that
        // instantiating NSApplication establishes (see Accessibility.swift).
        Accessibility.primeAXConnection()

        NSWorkspace.shared.notificationCenter.addObserver(
            self,
            selector: #selector(activeAppChanged(_:)),
            name: NSWorkspace.didActivateApplicationNotification,
            object: nil
        )
        // Evaluate whatever is frontmost right now (synchronous), so the gate
        // is correct from the first keystroke rather than the first focus
        // change after launch.
        rebindToFrontmostApp()
    }

    @objc private func activeAppChanged(_ note: Notification) {
        // Fix-B: switching apps puts the caret in a different field/app the
        // engine can't dead-reckon — reset its line model. (Not emitted from
        // the initial bind in `start()`, which doesn't route through here.)
        //
        // Reason tag `"app"` = a REAL application switch (a different corrector
        // regime may be active). Distinct from the in-app focused-element
        // re-publish below (`"focus-element"`), which fires repeatedly during
        // ordinary editing in rich-text / web surfaces (Notes, Docs) without the
        // user ever leaving the app. The engine resets the line model on BOTH
        // (Fix-B's bias-toward-resetting), but only an `"app"` switch is a true
        // change of corrector regime. Content-free tag — no app identity (#8).
        bridge.emit(.caretMoved(reason: "app"))
        rebindToFrontmostApp()
    }

    /// Point the AX observer at the current frontmost app (each app needs its
    /// own observer) and re-evaluate focus immediately.
    private func rebindToFrontmostApp() {
        guard let app = NSWorkspace.shared.frontmostApplication else {
            teardownObserver()
            observedPid = 0
            updateSecureFlag(forApp: nil)
            return
        }
        let pid = app.processIdentifier
        if pid != observedPid {
            teardownObserver()
            installObserver(for: pid)
            observedPid = pid
        }
        updateSecureFlag(forApp: AXUIElementCreateApplication(pid))
    }

    private func installObserver(for pid: pid_t) {
        var observer: AXObserver?
        let callback: AXObserverCallback = { _, _, notification, refcon in
            guard let refcon = refcon else { return }
            let me = Unmanaged<SecureFieldMonitor>.fromOpaque(refcon).takeUnretainedValue()
            me.axNotification(notification as String)
        }
        guard AXObserverCreate(pid, callback, &observer) == .success,
              let obs = observer else {
            // No observer (e.g. AX read blocked for this app): the flag stays
            // false and Layer 1 covers anything that trips global secure input.
            FileHandle.standardError.write(
                Data("SECURE_FIELD_OBSERVER_FAILED pid=\(pid)\n".utf8))
            return
        }
        let appEl = AXUIElementCreateApplication(pid)
        let refcon = Unmanaged.passUnretained(self).toOpaque()
        // Focused-element changed serves both jobs (secure re-eval + the Fix-B
        // focus caret-move). Selected-text changed is subscribed ONLY for the
        // secure re-eval — the reload re-assert when clicking into a rebuilt
        // password field — and never emits CaretMoved (it fires on normal
        // typing, so it can't be a caret-move signal; see `axNotification`).
        for note in [
            kAXFocusedUIElementChangedNotification,
            kAXSelectedTextChangedNotification,
        ] {
            AXObserverAddNotification(obs, appEl, note as CFString, refcon)
        }
        CFRunLoopAddSource(
            CFRunLoopGetCurrent(), AXObserverGetRunLoopSource(obs), .commonModes)
        appObserver = obs
    }

    private func teardownObserver() {
        if let obs = appObserver {
            CFRunLoopRemoveSource(
                CFRunLoopGetCurrent(), AXObserverGetRunLoopSource(obs), .commonModes)
        }
        appObserver = nil
    }

    /// AX notification trigger: emit the Fix-B `CaretMoved` signal on a genuine
    /// focus change, re-read the secure flag, then re-poll briefly to catch a
    /// subrole that populates after the element is rebuilt (page reload).
    ///
    /// Only **focused-element changed** emits `CaretMoved` (focus moved to a
    /// different element, so the caret is elsewhere — content-free). We do NOT
    /// emit on **selected-text changed**: `kAXSelectedTextChangedNotification`
    /// fires whenever the caret advances, which includes *normal typing*, so it
    /// can't be told apart from a keystroke — treating it as a caret move wiped
    /// the line after every key and starved correction (the reason the
    /// selection trigger was reverted). The subscription stays, but only to
    /// drive the secure re-eval below, which emits nothing to stdout. Clicks
    /// (incl. trackpad) and Up/Down are covered by EventTap and the engine.
    private func axNotification(_ name: String) {
        if name == (kAXFocusedUIElementChangedNotification as String) {
            // Reason tag `"focus-element"` = the frontmost app re-published its
            // focused AX element WITHOUT an app switch. In a plain NSTextField
            // this is a genuine field-to-field move; in rich-text / web surfaces
            // (Notes, Google Docs) the same element is re-vended constantly
            // during ordinary editing (new line/paragraph node, inline UI, relayout),
            // so this tag is NOISY by nature. Kept distinct from a real app
            // switch (`"app"`) so a stateful consumer (Watch Dog's CompetitorSense)
            // can choose NOT to reset on this churn while Fix-B still resets on
            // both. Content-free tag — no element/app identity (#8).
            bridge.emit(.caretMoved(reason: "focus-element"))
        }
        reevaluateNow()
        scheduleConfirmation()
    }

    private func reevaluateNow() {
        guard observedPid != 0 else { updateSecureFlag(forApp: nil); return }
        updateSecureFlag(forApp: AXUIElementCreateApplication(observedPid))
    }

    /// (Re)start the bounded re-poll window. If a poll is already running we
    /// just refill its budget so a fresh focus change extends the window
    /// instead of stacking timers. CFRunLoopTimer is the only timer primitive
    /// that fires in this binary (see main.swift's note); it's added to — and
    /// fires on — the same run loop as the AX observer, so every re-read writes
    /// the flag on the writer thread the lock already guards.
    private func scheduleConfirmation() {
        confirmFiresLeft = Self.confirmFireCount
        if confirmTimer != nil { return }
        let timer = CFRunLoopTimerCreateWithHandler(
            kCFAllocatorDefault,
            CFAbsoluteTimeGetCurrent() + Self.confirmInterval,
            Self.confirmInterval, 0, 0
        ) { [weak self] timer in
            guard let self = self else { CFRunLoopTimerInvalidate(timer); return }
            self.reevaluateNow()
            self.confirmFiresLeft -= 1
            if self.confirmFiresLeft <= 0 {
                CFRunLoopTimerInvalidate(timer)
                self.confirmTimer = nil
            }
        }
        confirmTimer = timer
        CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, .commonModes)
    }

    private func updateSecureFlag(forApp appEl: AXUIElement?) {
        let secure = appEl.map(Self.focusedElementIsSecure) ?? false
        lock.lock()
        let changed = secure != _isSecureFieldFocused
        _isSecureFieldFocused = secure
        lock.unlock()
        if changed {
            // One line per transition (not per keystroke) so the sandbox can
            // see focus crossing into and out of a password field. `tid` is the
            // writer thread — compare against the gate's reader `tid` to confirm
            // they're the threads the cross-thread fix assumes.
            FileHandle.standardError.write(
                Data("SECURE_FIELD_FOCUS \(secure ? "ENTER" : "LEAVE") tid=\(threadID())\n".utf8))
        }
    }

    /// True iff the app's focused UI element is a secure / password text field.
    /// Reads subrole only (content-blind). Catches native `NSSecureTextField`
    /// and WebKit/Chromium password inputs, which all carry the
    /// `AXSecureTextField` subrole.
    private static func focusedElementIsSecure(_ appEl: AXUIElement) -> Bool {
        var focusedRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
                appEl, kAXFocusedUIElementAttribute as CFString, &focusedRef) == .success,
              let fref = focusedRef,
              CFGetTypeID(fref) == AXUIElementGetTypeID() else {
            return false
        }
        let el = fref as! AXUIElement
        var subroleRef: CFTypeRef?
        guard AXUIElementCopyAttributeValue(
                el, kAXSubroleAttribute as CFString, &subroleRef) == .success else {
            return false
        }
        return (subroleRef as? String) == secureSubrole
    }
}
