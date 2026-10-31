//! Open at Login — registers the app as a macOS login item so it comes back after
//! a restart or an update that quit it (a silent death otherwise: nothing
//! relaunches a menu-bar app).
//!
//! Uses `SMAppService.mainApp` (macOS 13+), which registers *this signed .app*,
//! lists it under System Settings ▸ General ▸ Login Items (where the user can also
//! switch it off), and needs no helper bundle. The OS call sits behind
//! [`LoginItemBackend`] so the policy below — when to register, the once-only
//! default, never touching a dev/sandbox run — is unit-tested without a signed
//! build. What a unit test cannot prove (that macOS accepts the registration and
//! launches the app at login) is listed in the PR and waits on the signed install.
//!
//! **State is read from macOS, never cached.** The user can flip the item in
//! System Settings at any time, so the Settings toggle always shows
//! [`LoginItemBackend::status`], not a saved preference. The only thing we store
//! is a marker that the ON default has been applied once, so an update never
//! turns it back on after the user has turned it off.
//!
//! **Not eligible** (a no-op, and the toggle is disabled): a run that is not a
//! bundled `.app` (`tauri dev`, `cargo run` — registering a bare dev binary would
//! be wrong), and any run with `TYPEASSIST_DATA_DIR` set (the sandbox valve —
//! a scratch run must never change the real login items).

use serde::Serialize;
use std::path::Path;

/// What macOS says about our login item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStatus {
    /// Registered and will launch at login.
    Enabled,
    /// Not registered (the user turned it off, or it never was on).
    NotRegistered,
    /// Registered but the user must approve it in System Settings.
    RequiresApproval,
    /// macOS can't find the app's registration (e.g. not a bundled app).
    NotFound,
}

/// The OS-facing seam. The real one calls `SMAppService`; tests use a fake.
pub trait LoginItemBackend {
    fn status(&self) -> LoginStatus;
    fn register(&self) -> Result<(), String>;
    fn unregister(&self) -> Result<(), String>;
}

/// The Settings toggle's view of the world, sent to the webview.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LoginItemState {
    /// On means macOS will launch the app at login (or is waiting for approval).
    pub enabled: bool,
    /// `"on"`, `"off"`, `"needs_approval"` or `"unavailable"`.
    pub status: &'static str,
    /// False for a dev/sandbox run: the toggle is shown disabled.
    pub available: bool,
}

/// Whether this process may touch login items at all.
pub fn eligible(bundled: bool, scratch_data_dir: bool) -> bool {
    bundled && !scratch_data_dir
}

/// True when the running binary lives inside a `.app` bundle.
pub fn running_from_app_bundle() -> bool {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false)
}

/// True when the `TYPEASSIST_DATA_DIR` scratch valve is in use.
pub fn scratch_data_dir_in_use() -> bool {
    std::env::var_os("TYPEASSIST_DATA_DIR").is_some()
}

/// Read the toggle state. Ineligible ⇒ unavailable + off, with no OS call.
pub fn read_state(backend: &dyn LoginItemBackend, eligible: bool) -> LoginItemState {
    if !eligible {
        return LoginItemState {
            enabled: false,
            status: "unavailable",
            available: false,
        };
    }
    match backend.status() {
        LoginStatus::Enabled => LoginItemState {
            enabled: true,
            status: "on",
            available: true,
        },
        LoginStatus::RequiresApproval => LoginItemState {
            enabled: true,
            status: "needs_approval",
            available: true,
        },
        LoginStatus::NotRegistered | LoginStatus::NotFound => LoginItemState {
            enabled: false,
            status: "off",
            available: true,
        },
    }
}

/// The Settings toggle: register or unregister, then return the status macOS now
/// reports. Also records the once-only marker so the ON default can never
/// override a choice the user has made.
pub fn set_enabled(
    backend: &dyn LoginItemBackend,
    marker: Option<&Path>,
    eligible: bool,
    enabled: bool,
) -> Result<LoginItemState, String> {
    if !eligible {
        return Err("Open at login isn't available in this build.".to_string());
    }
    if enabled {
        backend.register()?;
    } else {
        backend.unregister()?;
    }
    write_marker(marker);
    tracing::info!(
        "LOGIN_ITEM action={} status={:?}",
        if enabled { "register" } else { "unregister" },
        backend.status()
    );
    Ok(read_state(backend, true))
}

/// What the first-launch default did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultOutcome {
    /// Not a bundled run, or a sandbox run: nothing touched.
    Ineligible,
    /// Already applied on an earlier launch (or the user has chosen): left alone.
    AlreadyApplied,
    /// Registered now (macOS shows its "login item added" notice).
    Registered,
    /// Registration failed; the marker is NOT written so the next launch retries.
    Failed,
}

/// Register the login item ONCE, on the first launch of a build that has this
/// feature — for existing users too. After that the user's own toggle (or System
/// Settings) is the source of truth and this never fires again.
pub fn apply_default(
    backend: &dyn LoginItemBackend,
    marker: Option<&Path>,
    eligible: bool,
) -> DefaultOutcome {
    if !eligible {
        return DefaultOutcome::Ineligible;
    }
    if marker_present(marker) {
        return DefaultOutcome::AlreadyApplied;
    }
    // Already on (the user may have added it by hand): nothing to register.
    if matches!(
        backend.status(),
        LoginStatus::Enabled | LoginStatus::RequiresApproval
    ) {
        write_marker(marker);
        return DefaultOutcome::AlreadyApplied;
    }
    match backend.register() {
        Ok(()) => {
            write_marker(marker);
            tracing::info!(
                "LOGIN_ITEM action=default_register status={:?}",
                backend.status()
            );
            DefaultOutcome::Registered
        }
        Err(e) => {
            tracing::warn!("LOGIN_ITEM default_register failed ({e}) — will retry next launch");
            DefaultOutcome::Failed
        }
    }
}

fn marker_present(marker: Option<&Path>) -> bool {
    marker.map(|p| p.exists()).unwrap_or(false)
}

fn write_marker(marker: Option<&Path>) {
    let Some(p) = marker else { return };
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(p, b"applied\n");
}

// ---- The real macOS backend -------------------------------------------------

#[cfg(target_os = "macos")]
pub struct MacLoginItem;

#[cfg(target_os = "macos")]
impl LoginItemBackend for MacLoginItem {
    fn status(&self) -> LoginStatus {
        use objc2_service_management::{SMAppService, SMAppServiceStatus};
        // SAFETY: `mainAppService` and `status` take no arguments and have no
        // preconditions; both are plain reads on the main-app service object.
        let s = unsafe { SMAppService::mainAppService().status() };
        if s == SMAppServiceStatus::Enabled {
            LoginStatus::Enabled
        } else if s == SMAppServiceStatus::RequiresApproval {
            LoginStatus::RequiresApproval
        } else if s == SMAppServiceStatus::NotRegistered {
            LoginStatus::NotRegistered
        } else {
            LoginStatus::NotFound
        }
    }

    fn register(&self) -> Result<(), String> {
        use objc2_service_management::SMAppService;
        // SAFETY: documented to be callable from any thread; the NSError, if any,
        // is returned (not thrown) and converted to a string below.
        unsafe { SMAppService::mainAppService().registerAndReturnError() }
            .map_err(|e| e.localizedDescription().to_string())
    }

    fn unregister(&self) -> Result<(), String> {
        use objc2_service_management::SMAppService;
        // SAFETY: as `register`.
        unsafe { SMAppService::mainAppService().unregisterAndReturnError() }
            .map_err(|e| e.localizedDescription().to_string())
    }
}

/// Open System Settings ▸ General ▸ Login Items (for the "needs approval" state).
#[cfg(target_os = "macos")]
pub fn open_login_items_settings() {
    // SAFETY: a plain class method that opens the Settings pane; no arguments.
    unsafe { objc2_service_management::SMAppService::openSystemSettingsLoginItems() };
}

// Non-macOS builds: there is no login-item API wired up, so everything reports
// unavailable (the toggle is disabled) and nothing is registered.
#[cfg(not(target_os = "macos"))]
pub struct MacLoginItem;

#[cfg(not(target_os = "macos"))]
impl LoginItemBackend for MacLoginItem {
    fn status(&self) -> LoginStatus {
        LoginStatus::NotFound
    }
    fn register(&self) -> Result<(), String> {
        Err("login items are only supported on macOS".to_string())
    }
    fn unregister(&self) -> Result<(), String> {
        Err("login items are only supported on macOS".to_string())
    }
}

#[cfg(not(target_os = "macos"))]
pub fn open_login_items_settings() {}

// ---- Tests --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A fake OS: records every call and keeps a registered flag.
    struct Fake {
        registered: RefCell<bool>,
        needs_approval: bool,
        fail_register: bool,
        register_calls: RefCell<u32>,
        unregister_calls: RefCell<u32>,
    }
    impl Fake {
        fn new(registered: bool) -> Self {
            Fake {
                registered: RefCell::new(registered),
                needs_approval: false,
                fail_register: false,
                register_calls: RefCell::new(0),
                unregister_calls: RefCell::new(0),
            }
        }
    }
    impl LoginItemBackend for Fake {
        fn status(&self) -> LoginStatus {
            if *self.registered.borrow() {
                if self.needs_approval {
                    LoginStatus::RequiresApproval
                } else {
                    LoginStatus::Enabled
                }
            } else {
                LoginStatus::NotRegistered
            }
        }
        fn register(&self) -> Result<(), String> {
            *self.register_calls.borrow_mut() += 1;
            if self.fail_register {
                return Err("denied".to_string());
            }
            *self.registered.borrow_mut() = true;
            Ok(())
        }
        fn unregister(&self) -> Result<(), String> {
            *self.unregister_calls.borrow_mut() += 1;
            *self.registered.borrow_mut() = false;
            Ok(())
        }
    }

    fn marker_in(tag: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("ta-login-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("login_item_default_applied")
    }

    #[test]
    fn toggle_on_registers_once_and_off_unregisters_once() {
        let fake = Fake::new(false);
        let marker = marker_in("toggle");
        let on = set_enabled(&fake, Some(&marker), true, true).unwrap();
        assert!(on.enabled && on.status == "on");
        assert_eq!(*fake.register_calls.borrow(), 1);
        assert_eq!(*fake.unregister_calls.borrow(), 0);

        let off = set_enabled(&fake, Some(&marker), true, false).unwrap();
        assert!(!off.enabled && off.status == "off");
        assert_eq!(*fake.unregister_calls.borrow(), 1);
        assert_eq!(*fake.register_calls.borrow(), 1, "off must not register again");
    }

    #[test]
    fn first_launch_default_registers_once_for_existing_users() {
        let fake = Fake::new(false);
        let marker = marker_in("default");
        assert_eq!(apply_default(&fake, Some(&marker), true), DefaultOutcome::Registered);
        assert_eq!(*fake.register_calls.borrow(), 1);
        assert!(marker.exists(), "the once-only marker must be written");
    }

    #[test]
    fn default_never_reapplies_after_the_user_turns_it_off() {
        let fake = Fake::new(false);
        let marker = marker_in("reapply");
        apply_default(&fake, Some(&marker), true);
        set_enabled(&fake, Some(&marker), true, false).unwrap(); // the user switches it off
        assert_eq!(fake.status(), LoginStatus::NotRegistered);

        // A later launch (e.g. after an update) must leave their choice alone.
        assert_eq!(apply_default(&fake, Some(&marker), true), DefaultOutcome::AlreadyApplied);
        assert_eq!(*fake.register_calls.borrow(), 1, "no second registration");
        assert_eq!(fake.status(), LoginStatus::NotRegistered);
    }

    #[test]
    fn dev_and_sandbox_runs_make_zero_os_calls() {
        let fake = Fake::new(false);
        let marker = marker_in("inelig");
        // not bundled (tauri dev) and scratch data dir (sandbox) are both ineligible
        assert!(!eligible(false, false));
        assert!(!eligible(true, true));
        assert!(eligible(true, false));

        assert_eq!(apply_default(&fake, Some(&marker), false), DefaultOutcome::Ineligible);
        assert!(set_enabled(&fake, Some(&marker), false, true).is_err());
        let s = read_state(&fake, false);
        assert!(!s.available && s.status == "unavailable");
        assert_eq!(*fake.register_calls.borrow(), 0);
        assert_eq!(*fake.unregister_calls.borrow(), 0);
        assert!(!marker.exists(), "an ineligible run must not leave a marker");
    }

    #[test]
    fn failed_default_is_retried_next_launch() {
        let mut fake = Fake::new(false);
        fake.fail_register = true;
        let marker = marker_in("retry");
        assert_eq!(apply_default(&fake, Some(&marker), true), DefaultOutcome::Failed);
        assert!(!marker.exists(), "a failed registration must not be marked done");
        fake.fail_register = false;
        assert_eq!(apply_default(&fake, Some(&marker), true), DefaultOutcome::Registered);
    }

    #[test]
    fn already_enabled_by_hand_is_not_registered_again() {
        let fake = Fake::new(true);
        let marker = marker_in("byhand");
        assert_eq!(apply_default(&fake, Some(&marker), true), DefaultOutcome::AlreadyApplied);
        assert_eq!(*fake.register_calls.borrow(), 0);
    }

    #[test]
    fn needs_approval_reads_as_on_with_its_own_status() {
        let mut fake = Fake::new(true);
        fake.needs_approval = true;
        let s = read_state(&fake, true);
        assert!(s.enabled && s.status == "needs_approval");
    }

    #[test]
    fn state_follows_system_settings_not_a_saved_preference() {
        let fake = Fake::new(true);
        assert_eq!(read_state(&fake, true).status, "on");
        *fake.registered.borrow_mut() = false; // the user removes it in System Settings
        assert_eq!(read_state(&fake, true).status, "off");
    }
}
