//! macOS-only: land the native tray NSMenu's first keyboard focus on the
//! topmost ACTIONABLE item, skipping the disabled status header.
//!
//! Why this exists. The tray menu is a native `NSMenu` (kept native so it opens
//! over fullscreen apps — see `build_tray`). With Full Keyboard Access, AppKit's
//! default initial highlight on a status-item menu is a stale row several down,
//! so the first Tab landed on "Open TypeAssist" instead of the topmost action
//! (Warm-up while active; Reconnect… / Restart capture while stopped). The
//! status line itself is non-interactive (disabled) and must be skipped.
//!
//! How. Tauri seals off the NSMenu pointer (`inner_context`/`ns_menu` are
//! `pub(crate)`), so we can't set a menu delegate at build time. Instead we
//! observe the PUBLIC `NSMenuDidBeginTrackingNotification` — it fires as the
//! menu opens, on the main thread — and, when the menu is positively ours, set
//! the initially-highlighted item to the first enabled, non-separator item.
//! "First enabled, non-separator item" is exactly "topmost actionable item below
//! the disabled status", in every state.
//!
//! Safety / blast radius. The highlight is set via the private selector
//! `_setInitiallyHighlightedItem:`, GUARDED by `respondsToSelector:` — if Apple
//! ever drops it this is a silent no-op (the menu behaves exactly as before, no
//! regression). We only touch a menu we positively identify as ours (its first
//! row is the disabled "Jordan …" status header), so webview context menus are
//! untouched. Setting the highlight does not affect item actions (those fire
//! through each NSMenuItem's own target/action), so click handling is unchanged.

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::sel;
use objc2_app_kit::{NSMenu, NSMenuDidBeginTrackingNotification, NSMenuItem};
use objc2_foundation::{NSNotification, NSNotificationCenter};
use std::ptr::NonNull;

/// Register the one main-thread observer that pre-highlights our tray menu.
/// Call once, from Tauri `setup` (which runs on the main thread). The observer
/// lives for the whole app, so we deliberately leak its token.
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
    let Some(item) = first_actionable_item(menu) else {
        return;
    };
    set_initial_highlight(menu, &item);
}

/// Identify our tray menu: its first row is the status header, which is the only
/// menu we build whose first item is disabled and titled "Jordan …" (see
/// `status_text`). Keeps the fix off webview/context menus.
fn is_our_tray_menu(menu: &NSMenu) -> bool {
    let Some(first) = menu.itemArray().firstObject() else {
        return false;
    };
    !first.isEnabled() && first.title().to_string().starts_with("Jordan")
}

/// The topmost actionable item: first enabled, non-separator item. Skips the
/// disabled status header (and any separators).
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

fn set_initial_highlight(menu: &NSMenu, item: &NSMenuItem) {
    let sel = sel!(_setInitiallyHighlightedItem:);
    if menu.respondsToSelector(sel) {
        // SAFETY: guarded by respondsToSelector; takes one NSMenuItem* arg and
        // returns void. A no-op if the private selector is ever removed.
        unsafe {
            let _: () = msg_send![menu, _setInitiallyHighlightedItem: item];
        }
    }
}
