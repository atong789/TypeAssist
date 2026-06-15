//! macOS-only: land the native tray NSMenu's first keyboard focus on the
//! topmost ACTIONABLE item, skipping the disabled status header.
//!
//! Why this exists. The tray menu is a native `NSMenu` (kept native so it opens
//! over fullscreen apps — see `build_tray`). On macOS 26 the menu opens with
//! NOTHING highlighted, so the user must press a key first, and Tab (not a menu
//! key) jumps past Warm-up/Progress to "Open TypeAssist". The disabled status
//! header must be skipped and the first actionable item (Warm-up while active;
//! Reconnect… / Restart capture while stopped) should be lit the moment the menu
//! opens.
//!
//! What didn't work, and why this does. Setting `highlightedItem` is a no-op on
//! macOS 26 (verified on device). Posting a synthetic Down-arrow AT
//! begin-tracking also did nothing — the tracking run-loop hasn't started
//! pumping events yet when that notification fires, so the event is never
//! consumed (verified on device: rows ARE arrow-navigable, but the open-time
//! post had no effect). The fix: DEFER into the tracking run-loop. We reach the
//! menu via the public `NSMenuDidBeginTracking` notification, then
//! `performSelector:withObject:afterDelay:inModes:[NSEventTrackingRunLoopMode]`
//! schedules our callback to run a beat later, WHILE the menu is actively
//! tracking. From there we (a) try the highlight setter at the right timing and
//! (b) if it still didn't take, post a Down-arrow — which the now-pumping
//! tracking loop consumes, moving the highlight to the first selectable row.
//!
//! Capture integrity (Principle #7). The Down-arrow is posted with
//! `NSApplication.postEvent:atStart:` — the app's Cocoa event queue, NOT the
//! CGEvent/HID stream the sidecar taps — so capture never sees a phantom key.
//!
//! Scope + safety. Everything is gated to OUR menu (first row = the disabled
//! "Jordan …" status header), so other menus are untouched, and a stray Down in
//! our own tracking session is harmless. A one-time `MENU_FOCUS` stderr dump of
//! the items + the post-attempt highlight state aids any future diagnosis.

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, Sel};
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventModifierFlags, NSEventTrackingRunLoopMode, NSEventType, NSMenu,
    NSMenuDidBeginTrackingNotification, NSMenuItem,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSNotification, NSNotificationCenter, NSPoint, NSString,
};
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
    if TRACE_FIRES.load(Ordering::Relaxed) < 16 {
        let _ = writeln!(std::io::stderr(), "MENU_FOCUS {msg}");
    }
}

define_class!(
    /// Carries our deferred "select the first item" callback so it can be
    /// scheduled into the event-tracking run-loop mode (it runs WHILE the menu
    /// is tracking, unlike the begin-tracking notification).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TypeAssistMenuNav"]
    struct MenuNav;

    unsafe impl NSObjectProtocol for MenuNav {}

    impl MenuNav {
        #[unsafe(method(fireForMenu:))]
        fn fire_for_menu(&self, menu: &NSMenu) {
            select_first_item_mid_tracking(menu);
        }
    }
);

impl MenuNav {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        unsafe { msg_send![mtm.alloc::<Self>(), init] }
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
    // `queue: None` = delivered synchronously on the posting (main) thread.
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
    let Some(obj) = notif.object() else { return };
    let Some(menu) = obj.downcast_ref::<NSMenu>() else {
        return;
    };
    if !is_our_tray_menu(menu) {
        return;
    }
    TRACE_FIRES.fetch_add(1, Ordering::Relaxed);

    if !DUMP_DONE.swap(true, Ordering::SeqCst) {
        dump_items(menu);
    }

    // Defer selection into the tracking run-loop: the tracking loop isn't
    // pumping events yet at begin-tracking, so do it a beat later, in
    // NSEventTrackingRunLoopMode, while the menu is actively tracking.
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let nav = MenuNav::new(mtm);
    // SAFETY: reading the AppKit-provided run-loop-mode constant.
    let tracking_mode = unsafe { NSEventTrackingRunLoopMode };
    let modes = NSArray::from_slice(&[tracking_mode]);
    // SAFETY: schedules `[nav fireForMenu:menu]` after 0s in the tracking mode.
    // performSelector:…afterDelay: retains both `nav` and `menu` until it fires,
    // so dropping our `nav`/`menu` refs here is fine.
    unsafe {
        let _: () = msg_send![
            &*nav,
            performSelector: sel!(fireForMenu:),
            withObject: menu,
            afterDelay: 0.0f64,
            inModes: &*modes,
        ];
    }
    trace("scheduled mid-tracking selection");
}

/// Identify our tray menu: its first row is the status header — the only menu we
/// build whose first item is disabled and titled "Jordan …" (see `status_text`).
fn is_our_tray_menu(menu: &NSMenu) -> bool {
    let Some(first) = menu.itemArray().firstObject() else {
        return false;
    };
    !first.isEnabled() && first.title().to_string().starts_with("Jordan")
}

/// The topmost actionable item: first enabled, non-separator item.
fn first_actionable_item(menu: &NSMenu) -> Option<Retained<NSMenuItem>> {
    let n = menu.numberOfItems();
    for i in 0..n {
        let Some(item) = menu.itemAtIndex(i) else {
            continue;
        };
        if !item.isSeparatorItem() && item.isEnabled() {
            return Some(item);
        }
    }
    None
}

/// Runs WHILE the menu is tracking. Try to set the highlight directly; if that
/// still doesn't take (the setter is a no-op on this macOS), post a Down-arrow,
/// which the now-pumping tracking loop consumes to select the first item.
fn select_first_item_mid_tracking(menu: &NSMenu) {
    let Some(item) = first_actionable_item(menu) else {
        trace("mid-tracking: no actionable item");
        return;
    };
    try_set_highlight(menu, &item);
    if menu.highlightedItem().is_none() {
        post_down_arrow();
        trace("mid-tracking: setter no-op → posted Down-arrow");
    } else {
        trace("mid-tracking: setter applied");
    }
}

/// Try the known private highlight setters (object-arg). A no-op if none stick.
fn try_set_highlight(menu: &NSMenu, item: &NSMenuItem) {
    let known: [Sel; 4] = [
        sel!(setHighlightedItem:),
        sel!(_setHighlightedItem:),
        sel!(highlightItem:),
        sel!(_highlightItem:),
    ];
    for selector in known {
        if menu.respondsToSelector(selector) {
            // SAFETY: each takes one object (NSMenuItem*); bind result as `()`
            // so objc2 never retains a setter's undefined return register.
            unsafe {
                let _: () = msg_send![menu, performSelector: selector, withObject: item];
            }
            return;
        }
    }
}

/// Post one synthetic Down-arrow into the app's Cocoa event queue (NOT the HID
/// stream — the sidecar's capture tap never sees it). The tracking menu consumes
/// it like a real key press and moves the highlight to the first selectable item.
fn post_down_arrow() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let chars = NSString::from_str(DOWN_ARROW_UNICHAR);
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
        app.postEvent_atStart(&event, true);
    }
}

/// One-time stderr dump of every row's title + keyboard-relevant flags.
fn dump_items(menu: &NSMenu) {
    let n = menu.numberOfItems();
    let mut out = String::from("items: ");
    for i in 0..n {
        let Some(it) = menu.itemAtIndex(i) else {
            continue;
        };
        let kind = if it.isSeparatorItem() { "sep" } else { "item" };
        out.push_str(&format!(
            "[{kind} '{}' en={} hid={} sub={} act={} view={}] ",
            it.title(),
            it.isEnabled(),
            it.isHidden(),
            it.hasSubmenu(),
            it.action().is_some(),
            it.view().is_some(),
        ));
    }
    trace(&out);
}
