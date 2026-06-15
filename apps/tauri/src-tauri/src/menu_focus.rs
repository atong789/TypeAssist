//! macOS-only: land the native tray NSMenu's first keyboard focus on the
//! topmost ACTIONABLE item, skipping the disabled status header.
//!
//! Why this exists. The tray menu is a native `NSMenu` (kept native so it opens
//! over fullscreen apps — see `build_tray`). On macOS 26, the menu opens with
//! NOTHING highlighted, and the first Tab jumps past Warm-up and Progress to
//! "Open TypeAssist". The disabled status header must be skipped and the first
//! actionable item (Warm-up while active; Reconnect… / Restart capture while
//! stopped) should be lit the moment the menu opens.
//!
//! How (synthesize the first navigation key). We tried setting `highlightedItem`
//! directly: it's read-only publicly and the private setters are either gone or
//! a confirmed no-op on macOS 26 (verified on device — `highlightedItem` stayed
//! nil after the call). So instead of fighting AppKit's highlight model we drive
//! it the way a user would: when our menu begins tracking, post a single
//! Down-arrow key event. AppKit's arrow-nav moves the highlight to the first
//! selectable item (the first enabled, non-separator row = the first actionable
//! item below the disabled status), which is exactly the target — and, unlike
//! Tab, arrow-nav doesn't do the skip.
//!
//! We reach the menu via the PUBLIC `NSMenuDidBeginTracking` notification (fires
//! as the menu opens, on the main thread, every open) and scope the key-post to
//! OUR menu only (its first row is the disabled "Jordan …" status header), so
//! other menus are untouched.
//!
//! Capture integrity (Principle #7). The key is posted with
//! `NSApplication.postEvent:atStart:` — into the app's own Cocoa event queue,
//! NOT the CGEvent/HID stream. The sidecar's session event tap reads the HID
//! stream, so it never sees this synthetic key: no phantom keystroke is captured.
//!
//! If a future macOS changes arrow-nav, this is a safe no-op (a stray Down
//! during our own menu's tracking does nothing harmful), and a one-time
//! `MENU_FOCUS` stderr dump of the menu's items is emitted for diagnosis.

use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSMenu,
    NSMenuDidBeginTrackingNotification,
};
use objc2_foundation::{MainThreadMarker, NSNotification, NSNotificationCenter, NSPoint, NSString};
use std::io::Write;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use block2::RcBlock;

/// Down-arrow virtual key code.
const KEYCODE_DOWN: u16 = 125;
/// `NSDownArrowFunctionKey` — the unicode the arrow key carries.
const DOWN_ARROW_UNICHAR: &str = "\u{F701}";

/// Emit the one-time item dump once (avoid one block per open).
static DUMP_DONE: AtomicBool = AtomicBool::new(false);

/// Verbose trace for the first few opens while behaviour is being nailed down.
static TRACE_FIRES: AtomicUsize = AtomicUsize::new(0);
fn trace(msg: &str) {
    if TRACE_FIRES.load(Ordering::Relaxed) < 12 {
        let _ = writeln!(std::io::stderr(), "MENU_FOCUS {msg}");
    }
}

/// Register the one main-thread observer that pre-selects our tray menu. Call
/// once, from Tauri `setup` (runs on the main thread).
pub fn install() {
    let block = RcBlock::new(|notif: NonNull<NSNotification>| {
        // SAFETY: the notification center hands us a live `NSNotification` for
        // the duration of the call; we only borrow it.
        on_menu_begin_tracking(unsafe { notif.as_ref() });
    });
    // SAFETY: standard NotificationCenter registration. `name` scopes us to
    // menu-tracking starts; `object: None` = any menu (we filter to ours);
    // `queue: None` = delivered synchronously on the posting (main) thread,
    // which is required for AppKit calls.
    let token = unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(NSMenuDidBeginTrackingNotification),
            None,
            None,
            &block,
        )
    };
    std::mem::forget(token);
    trace("observer installed");
}

fn on_menu_begin_tracking(notif: &NSNotification) {
    // The notification's `object` is the NSMenu that began tracking.
    let Some(obj) = notif.object() else { return };
    let Some(menu) = obj.downcast_ref::<NSMenu>() else {
        return;
    };
    if !is_our_tray_menu(menu) {
        return;
    }
    TRACE_FIRES.fetch_add(1, Ordering::Relaxed);

    // One-time snapshot of the items + flags, so any keyboard-eligibility quirk
    // (e.g. a row with no action) is visible if the key-nav doesn't land right.
    if !DUMP_DONE.swap(true, Ordering::SeqCst) {
        dump_items(menu);
    }

    // The menu is tracking now: drive arrow-nav to the first selectable item.
    select_first_item_via_down_arrow();
    trace("posted Down-arrow to select first actionable item");
}

/// Identify our tray menu: its first row is the status header — the only menu we
/// build whose first item is disabled and titled "Jordan …" (see `status_text`).
/// Keeps the key-post off webview / context menus.
fn is_our_tray_menu(menu: &NSMenu) -> bool {
    let Some(first) = menu.itemArray().firstObject() else {
        return false;
    };
    !first.isEnabled() && first.title().to_string().starts_with("Jordan")
}

/// Post one synthetic Down-arrow into the app's Cocoa event queue. The tracking
/// menu consumes it like a real key press and moves the highlight to the first
/// selectable item. Posted to the app queue (NOT the HID stream), so the
/// sidecar's capture tap never sees it.
fn select_first_item_via_down_arrow() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let chars = NSString::from_str(DOWN_ARROW_UNICHAR);
    // Arrow keys carry the Function + NumericPad modifier bits.
    let flags = NSEventModifierFlags::Function | NSEventModifierFlags::NumericPad;
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown,
        NSPoint::new(0.0, 0.0),
        flags,
        0.0,
        0,
        None,
        &chars,
        &chars,
        false,
        KEYCODE_DOWN,
    );
    if let Some(event) = event {
        // Posted to our own app's Cocoa event queue (not the HID stream).
        app.postEvent_atStart(&event, true);
    } else {
        trace("failed to build Down-arrow NSEvent");
    }
}

/// One-time stderr dump of every row's title + keyboard-relevant flags, so a
/// skip (e.g. an item with no action, or hidden) is explainable from a log.
fn dump_items(menu: &NSMenu) {
    let n = menu.numberOfItems();
    let mut out = String::from("items: ");
    for i in 0..n {
        let Some(it) = menu.itemAtIndex(i) else {
            continue;
        };
        let kind = if it.isSeparatorItem() { "sep" } else { "item" };
        let has_action = it.action().is_some();
        out.push_str(&format!(
            "[{kind} '{}' en={} hid={} sub={} act={} view={}] ",
            it.title(),
            it.isEnabled(),
            it.isHidden(),
            it.hasSubmenu(),
            has_action,
            it.view().is_some(),
        ));
    }
    trace(&out);
}
