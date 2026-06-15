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
//! How (delegate-swap). Tauri seals off the NSMenu pointer
//! (`inner_context`/`ns_menu` are `pub(crate)`), so we can't set a delegate at
//! build time. We reach the menu via the PUBLIC `NSMenuDidBeginTracking`
//! notification (it fires as the menu opens, on the main thread) and, the first
//! time we see OUR menu, swap in our own `NSMenuDelegate`. From then on the
//! delegate's `menuWillOpen:` runs BEFORE the menu is displayed — the canonical
//! place to set initial keyboard focus — and we highlight the first enabled,
//! non-separator item ("topmost actionable below the disabled status", in every
//! state). The first open is handled inline by the notification itself, so it's
//! correct too. Swapping the delegate is safe: muda's delegate is empty (only a
//! window-menu id, unused for a tray menu) and item clicks fire through each
//! NSMenuItem's own target/action, not the menu delegate.
//!
//! Setting the highlight. There is no PUBLIC highlight setter (`highlightedItem`
//! is read-only). The actual setter is a private selector whose name has drifted
//! across macOS versions (`_setInitiallyHighlightedItem:` is gone on macOS 26).
//! So we (1) try a short list of known names via `respondsToSelector:`, then
//! (2) if none match, DISCOVER the setter by scanning NSMenu's selectors at
//! runtime for a one-argument "highlight … item" method — making the fix
//! self-healing across macOS versions — and call it via
//! `performSelector:withObject:`. If even discovery finds nothing, this is a
//! silent no-op (no regression) and a one-time `MENU_FOCUS` stderr line reports
//! the menu's shape + NSMenu's highlight-ish selectors for a quick follow-up.

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, Sel};
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{NSMenu, NSMenuDelegate, NSMenuDidBeginTrackingNotification, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSNotification, NSNotificationCenter};
use std::io::Write;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use block2::RcBlock;

/// Verbose trace for the first few menu opens so we can see, on stderr, whether
/// the hook fires, whether we identify our menu, and which setter applied —
/// ground truth while the behavior is still being nailed down. Quiet afterward.
static TRACE_FIRES: AtomicUsize = AtomicUsize::new(0);
fn trace(msg: &str) {
    if TRACE_FIRES.load(Ordering::Relaxed) < 12 {
        let _ = writeln!(std::io::stderr(), "MENU_FOCUS {msg}");
    }
}

/// Whether our delegate is installed yet (installed once, on the first open of
/// our menu). The highlight itself runs on EVERY open, from two timings that are
/// idempotent together: the delegate's `menuWillOpen:` (before display) and the
/// begin-tracking notification (while the menu is tracking, when the highlight
/// setter is most likely to stick). Setting the same item twice is a no-op.
static DELEGATE_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Emit the no-setter diagnostic at most once (avoid one line per open).
static DIAG_DONE: AtomicBool = AtomicBool::new(false);

define_class!(
    /// Our tray-menu delegate. Only `menuWillOpen:` matters: set the initial
    /// keyboard highlight before the menu is shown.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TypeAssistMenuFocusDelegate"]
    struct FocusDelegate;

    unsafe impl NSObjectProtocol for FocusDelegate {}

    unsafe impl NSMenuDelegate for FocusDelegate {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, menu: &NSMenu) {
            highlight_first_actionable(menu);
        }
    }
);

impl FocusDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // No ivars; just an NSObject subclass conforming to NSMenuDelegate.
        unsafe { msg_send![mtm.alloc::<Self>(), init] }
    }
}

/// Register the one main-thread observer that wires up our tray menu. Call once,
/// from Tauri `setup` (runs on the main thread).
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
        trace("beginTracking: object is not an NSMenu");
        return;
    };
    let ours = is_our_tray_menu(menu);
    let first_title = menu
        .itemArray()
        .firstObject()
        .map(|it| it.title().to_string());
    trace(&format!(
        "beginTracking fired; ours={ours} firstItem={first_title:?}"
    ));
    TRACE_FIRES.fetch_add(1, Ordering::Relaxed);
    if !ours {
        return;
    }

    // First time we see our menu: swap in our delegate so opens also get
    // `menuWillOpen:` (which fires before display — a second shot at highlight).
    if !DELEGATE_INSTALLED.swap(true, Ordering::SeqCst) {
        if let Some(mtm) = MainThreadMarker::new() {
            let delegate = FocusDelegate::new(mtm);
            let proto = objc2::runtime::ProtocolObject::from_ref(&*delegate);
            menu.setDelegate(Some(proto));
            // NSMenu's delegate is a weak ref — keep ours alive for the app.
            std::mem::forget(delegate);
        }
    }
    // The menu is tracking now, so the highlight setter is most likely to stick.
    // Runs on every open (idempotent with the delegate's pre-display attempt).
    highlight_first_actionable(menu);
}

/// Identify our tray menu: its first row is the status header — the only menu we
/// build whose first item is disabled and titled "Jordan …" (see `status_text`).
/// Keeps the fix off webview / context menus.
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

fn highlight_first_actionable(menu: &NSMenu) {
    let Some(item) = first_actionable_item(menu) else {
        trace("no actionable item found");
        return;
    };
    let applied = try_set_highlight(menu, &item);
    trace(&format!(
        "target='{}' setterApplied={applied:?} highlightedNow={:?}",
        item.title(),
        menu.highlightedItem().map(|h| h.title().to_string()),
    ));
    if applied.is_none() && !DIAG_DONE.swap(true, Ordering::SeqCst) {
        // Nothing applied — emit ground-truth ONCE so the working selector can
        // be pinned without another guessing round.
        dump_diagnostics(menu);
    }
}

/// Set the menu's highlighted item via whichever private setter this macOS
/// exposes (`highlightedItem` itself is read-only in the public API, and the
/// setter's name has drifted across releases). Tries known names first, then —
/// so the fix is self-healing across macOS versions — DISCOVERS the setter by
/// scanning NSMenu's selectors at runtime. Returns the selector applied, if any.
fn try_set_highlight(menu: &NSMenu, item: &NSMenuItem) -> Option<String> {
    // Known names, newest-macOS-plausible first. `sel!` is a compile-time
    // literal, so these are the cheap fast path.
    let known: [Sel; 5] = [
        sel!(setHighlightedItem:),
        sel!(_setHighlightedItem:),
        sel!(_setInitiallyHighlightedItem:),
        sel!(highlightItem:),
        sel!(_highlightItem:),
    ];
    for selector in known {
        if menu.respondsToSelector(selector) {
            perform_with_item(menu, selector, item);
            return Some(selector.name().to_string_lossy().into_owned());
        }
    }

    // Fallback: discover a "highlight … item" one-argument setter by name. Skips
    // index-based variants (they take an integer, not our item) and getters
    // (zero colons). Calling a "…HighlightItem:" / "…HighlightedItem:" selector
    // with an NSMenuItem is the right arg shape.
    if let Some(selector) = discover_highlight_item_setter() {
        perform_with_item(menu, selector, item);
        return Some(selector.name().to_string_lossy().into_owned());
    }
    None
}

/// `performSelector:withObject:` with the item. SAFETY: only ever called with a
/// selector that takes one object arg (an NSMenuItem); we bind the result as `()`
/// so objc2 never tries to retain a setter's undefined return register.
fn perform_with_item(menu: &NSMenu, selector: Sel, item: &NSMenuItem) {
    unsafe {
        let _: () = msg_send![menu, performSelector: selector, withObject: item];
    }
}

/// Scan NSMenu (+ superclasses) for a one-argument selector that sets the
/// highlighted *item* — e.g. `setHighlightedItem:` under a name we didn't
/// hard-code. Excludes index-based and getter selectors.
fn discover_highlight_item_setter() -> Option<Sel> {
    use objc2::runtime::AnyClass;
    let mut cls: Option<&AnyClass> = AnyClass::get(c"NSMenu");
    while let Some(c) = cls {
        for m in c.instance_methods().iter() {
            let sel = m.name();
            let Ok(name) = sel.name().to_str() else {
                continue;
            };
            let l = name.to_ascii_lowercase();
            let one_arg = name.matches(':').count() == 1 && name.ends_with(':');
            if one_arg && l.contains("highlight") && l.contains("item") && !l.contains("index") {
                return Some(sel);
            }
        }
        if c.name().to_str() == Ok("NSObject") {
            break;
        }
        cls = c.superclass();
    }
    None
}

/// One-time stderr dump when no known setter applied: the menu's shape PLUS the
/// real NSMenu (+ superclass) selectors whose names mention highlight/select/
/// initial. macOS 26 renamed/removed the old setter, so this enumerates the
/// runtime to reveal the current name — conclusive ground truth, no more
/// guessing. Quiet on normal runs (gated by DIAG_DONE).
fn dump_diagnostics(menu: &NSMenu) {
    use objc2::runtime::AnyClass;

    let n = menu.numberOfItems();
    let mut items = String::new();
    for i in 0..n {
        if let Some(it) = menu.itemAtIndex(i) {
            let flag = if it.isSeparatorItem() {
                "—"
            } else if it.isEnabled() {
                "•"
            } else {
                "×"
            };
            items.push_str(&format!("[{flag}{}] ", it.title()));
        }
    }

    // Walk NSMenu and its superclasses, collecting selector names that look
    // highlight/selection-related — the candidate setters we'd call instead.
    let mut found = String::new();
    let mut cls: Option<&AnyClass> = AnyClass::get(c"NSMenu");
    while let Some(c) = cls {
        for m in c.instance_methods().iter() {
            if let Ok(name) = m.name().name().to_str() {
                let l = name.to_ascii_lowercase();
                if l.contains("highlight") || l.contains("initiallyselected") {
                    found.push_str(name);
                    found.push(' ');
                }
            }
        }
        if c.name().to_str() == Ok("NSObject") {
            break;
        }
        cls = c.superclass();
    }

    let _ = writeln!(
        std::io::stderr(),
        "MENU_FOCUS no known highlight setter applied.\n\
         MENU_FOCUS items: {items}\n\
         MENU_FOCUS NSMenu highlight-ish selectors: [{found}]"
    );
}
