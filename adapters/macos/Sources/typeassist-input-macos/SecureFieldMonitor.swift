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
        let callback: AXObserverCallback = { _, _, _, refcon in
            guard let refcon = refcon else { return }
            let me = Unmanaged<SecureFieldMonitor>.fromOpaque(refcon).takeUnretainedValue()
            me.focusChanged()
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
        // Focused-element changed is the primary trigger; selected-text changed
        // is an extra re-assert for the reload case — clicking into / placing
        // the caret in the rebuilt password field fires it even when the focus
        // notification landed early. Both just trigger a re-evaluation.
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

    /// AX notification trigger (focus or selection changed): read now, then
    /// re-poll briefly to catch a subrole that populates after the element is
    /// rebuilt (page reload).
    private func focusChanged() {
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
