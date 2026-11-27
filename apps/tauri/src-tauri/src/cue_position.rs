//! TF-07 — position the correction bubble (the `cue` window) on the TOP-LEFT of
//! the screen the user is actively typing on, then show it.
//!
//! **Why native AppKit and not Tauri's monitor APIs.** tao's `cursor_position()`,
//! `monitor_from_point()` and `set_position()` mix physical pixels with logical
//! points and flip Y against the *primary* display's pixel height. On a mixed-DPI
//! multi-monitor rig (e.g. a Retina laptop + an external display) the cursor point
//! lands outside every monitor's `CGDisplayBounds`, so `monitor_from_point`
//! resolves *no* screen and we fell back to the primary — the bubble showed on the
//! laptop no matter where the user was typing. `NSEvent.mouseLocation` and
//! `NSScreen.frame` live in a single, consistent AppKit point space (bottom-left
//! origin, Y-up), so doing the cursor→screen match and the window placement there
//! needs no unit conversion and no Y-flip of our own.
//!
//! **Privacy (Principle #8).** The cursor location is read once, in memory, for
//! this single placement decision and is never stored, logged, or associated with
//! anything. An ephemeral read for an immediate choice is not data collection.
//!
//! Fixed position on the active screen — no caret tracking, no window-relative
//! tracking (both unreliable off native fields). The ~5s fade in/out lives in the
//! bubble webview and is untouched.

use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSScreen, NSWindow};
use objc2_foundation::{MainThreadMarker, NSPoint};
use tauri::{AppHandle, Manager, Runtime};

/// Insets into the active screen, in AppKit points.
const TOP_INSET: f64 = 60.0; // clear the menu bar + notch / traffic-light row
const LEFT_INSET: f64 = 16.0; // a small gap off the left bezel, not flush to it

/// Anchor the `cue` window to the top-left of the active screen and show it.
///
/// **Must run on the main thread** — it touches AppKit (`NSScreen`, and the
/// window's `NSWindow`). Callers dispatch via `AppHandle::run_on_main_thread`,
/// since the correction-event listeners can fire off the main thread. If somehow
/// invoked off-main we refuse the AppKit work rather than risk a bad-thread call,
/// and still show the window wherever it is (never swallow the bubble).
pub fn show_cue_top_left<R: Runtime>(app: &AppHandle<R>) {
    let Some(w) = app.get_webview_window("cue") else {
        return;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        // Not on the main thread — don't touch AppKit; just surface the bubble.
        let _ = w.show();
        return;
    };
    let Ok(ns_ptr) = w.ns_window() else {
        let _ = w.show();
        return;
    };
    // SAFETY: Tauri hands us the live `NSWindow` backing this webview window; it
    // outlives this synchronous main-thread call.
    let ns_window: &NSWindow = unsafe { &*(ns_ptr as *const NSWindow) };

    // Cursor in AppKit global points — the same space as `NSScreen.frame`, so
    // containment and placement need no unit conversion.
    let cursor = NSEvent::mouseLocation();

    // The screen under the cursor = the screen the user is actively typing on.
    // Fall back to the main screen if the cursor resolves to none (it always
    // should, but never guess an off-screen position).
    let screen = screen_containing(mtm, cursor).or_else(|| NSScreen::mainScreen(mtm));
    let Some(screen) = screen else {
        let _ = w.show();
        return;
    };
    let frame = screen.frame();

    // Top-left of that screen, inset. AppKit y grows upward, so the top edge is
    // `origin.y + height`; drop `TOP_INSET` below it. `setFrameTopLeftPoint`
    // places the window's top-left corner directly (no Y-flip on our side).
    let point = NSPoint::new(
        frame.origin.x + LEFT_INSET,
        frame.origin.y + frame.size.height - TOP_INSET,
    );
    // Main-thread AppKit call moving our own borderless HUD window.
    ns_window.setFrameTopLeftPoint(point);
    let _ = w.show();
}

/// The first screen whose frame contains `p` (AppKit global points), or `None`.
fn screen_containing(mtm: MainThreadMarker, p: NSPoint) -> Option<Retained<NSScreen>> {
    let screens = NSScreen::screens(mtm);
    for i in 0..screens.count() {
        let screen = screens.objectAtIndex(i);
        let f = screen.frame();
        if p.x >= f.origin.x
            && p.x < f.origin.x + f.size.width
            && p.y >= f.origin.y
            && p.y < f.origin.y + f.size.height
        {
            return Some(screen);
        }
    }
    None
}
