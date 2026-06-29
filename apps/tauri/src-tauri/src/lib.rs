//! Layer 5 — Tauri shell.
//!
//! Hosts the Svelte webview and the engine (Swift sidecar + walking-skeleton
//! loop). See `engine.rs`.

mod allow_list;
mod engine;
#[cfg(target_os = "macos")]
mod menu_focus;

use allow_list::AllowList;
use engine::{EngineControl, EngineControlSender};
use tauri::{
    menu::{CheckMenuItem, IconMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    ActivationPolicy, AppHandle, Emitter, Listener, Manager, Runtime, WindowEvent,
};
use tauri_plugin_positioner::{Position, WindowExt};

/// Tauri command: ask the engine to wipe the L4 LEXICON state (the
/// proposer AND the lex's learned set) — the "Reset LEXICON" button
/// on the debug panel. Distinct from a panel-side Clear (mirrors
/// only): this is the engine-side wipe a re-test of recency
/// reclamation needs. Returns an error string iff the engine task
/// has already exited (the control channel is closed) — the panel
/// surfaces it so a stuck reset doesn't fail silently.
#[tauri::command]
fn reset_lexicon(sender: tauri::State<EngineControlSender>) -> Result<(), String> {
    sender
        .send(EngineControl::ResetLexicon)
        .map_err(|e| format!("engine control channel closed: {e}"))
}

/// Tauri command: suppress or resume LEXICON credit. The panel
/// computes the combined pause state (manual toggle OR debug-window
/// focus) and posts it here. Engine echoes back via
/// `engine://learning-paused` so the indicator reflects the actual
/// engine flag.
///
/// Privacy invariant: this is a **single bit** the engine receives.
/// The host does NOT learn which app the user is typing into; the
/// panel decides locally from its own DOM focus state and posts
/// only the resulting boolean. Matches CLAUDE.md's "Do not track
/// which app was focused" principle — TypeAssist's own window
/// focus is a non-invasive native capability the OS provides any
/// app, and no other app's identity is consulted.
#[tauri::command]
fn set_learning_paused(
    paused: bool,
    sender: tauri::State<EngineControlSender>,
) -> Result<(), String> {
    sender
        .send(EngineControl::SetLearningPaused(paused))
        .map_err(|e| format!("engine control channel closed: {e}"))
}

/// Tauri command: hard-pause the engine. While true, Key/Backspace
/// events from the sidecar are dropped at the engine task boundary —
/// before model.ingest, before tokenization, before any emission.
/// The FEED freezes, the engine effectively sleeps. Used by the
/// "Pause input" toggle when the user is talking ABOUT the system
/// and wants the engine quiet (rather than just suppressing learning).
#[tauri::command]
fn set_input_paused(paused: bool, sender: tauri::State<EngineControlSender>) -> Result<(), String> {
    sender
        .send(EngineControl::SetInputPaused(paused))
        .map_err(|e| format!("engine control channel closed: {e}"))
}

/// Tauri command: ask the engine to soft-restart capture. Writes
/// the RestartTap OutboundCommand to the sidecar's stdin so it
/// tears down its current event tap and creates a fresh one.
/// Always available from the panel — when capture is the broken
/// thing, this is the recovery path that must stay live.
#[tauri::command]
fn restart_capture(sender: tauri::State<EngineControlSender>) -> Result<(), String> {
    sender
        .send(EngineControl::RestartCapture)
        .map_err(|e| format!("engine control channel closed: {e}"))
}

/// Tauri command: ask the engine to emit the current C5c motor
/// [`StabilityReport`] on `engine://motor-stability` — the weakest-keys
/// preview Practice mode builds its curriculum from, plus the kill-switch
/// coverage/slip-rate inputs. Read-only: the engine emits data, the
/// kill-switch decision is not made here (or anywhere yet). The response
/// arrives asynchronously on the event, not as this command's return.
#[tauri::command]
fn request_motor_stability(sender: tauri::State<EngineControlSender>) -> Result<(), String> {
    sender
        .send(EngineControl::RequestMotorStability)
        .map_err(|e| format!("engine control channel closed: {e}"))
}

/// Status-line label text. The dot beside it (the item's *icon*) carries the
/// state colour — a native `NSMenu` renders text in the menu's own colour, so a
/// coloured dot can only be an image, not a text glyph. Active = the calm
/// "is active"; stopped = the plain, unambiguous "has stopped" (the menu-bar
/// icon, not the wording, carries the alarm — see the design note).
fn status_text(active: bool) -> &'static str {
    if active {
        "Jordan is active"
    } else {
        "Jordan has stopped"
    }
}

const UI_BLUE: (u8, u8, u8) = (0x0a, 0x84, 0xff); // --focus-ring, the UI blue
const UI_GREY: (u8, u8, u8) = (0x8e, 0x8e, 0x93); // system secondary grey (light+dark)

/// The status-line dot, built in memory as the item's icon (rendered in colour,
/// NOT a template). Trimmed small — it sits beside the text, not as a bullet.
/// Active = a small filled blue dot; stopped = a small hollow grey ring (so the
/// state reads by shape too, and it's NEVER red — the no-deficit-framing rule).
fn status_dot(active: bool) -> tauri::image::Image<'static> {
    const N: u32 = 32;
    let c = N as f32 / 2.0;
    let r = N as f32 * 0.26; // small — the old dot was full-bleed and oversized
    let ring = 3.0; // hollow-ring stroke
    let (cr, cg, cb) = if active { UI_BLUE } else { UI_GREY };
    let mut rgba = vec![0u8; (N * N * 4) as usize];
    for y in 0..N {
        for x in 0..N {
            let dx = x as f32 + 0.5 - c;
            let dy = y as f32 + 0.5 - c;
            let d = (dx * dx + dy * dy).sqrt();
            let cov = if active {
                // Filled disc, 1px feather.
                (r - d + 0.5).clamp(0.0, 1.0)
            } else {
                // Hollow ring: within half a stroke of radius r.
                (1.0 - ((d - r).abs() - ring / 2.0)).clamp(0.0, 1.0)
            };
            let i = ((y * N + x) * 4) as usize;
            rgba[i] = cr;
            rgba[i + 1] = cg;
            rgba[i + 2] = cb;
            rgba[i + 3] = (cov * 255.0) as u8;
        }
    }
    tauri::image::Image::new_owned(rgba, N, N)
}

/// Fill coverage (0..1, 1px feather) for a point inside a triangle (a,b,c).
fn tri_cov(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> f32 {
    let edge = |q: (f32, f32), r: (f32, f32)| (p.0 - q.0) * (r.1 - q.1) - (p.1 - q.1) * (r.0 - q.0);
    let (d1, d2, d3) = (edge(a, b), edge(b, c), edge(c, a));
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    if has_neg && has_pos {
        0.0
    } else {
        1.0
    }
}

/// "Restart capture" leading icon: a circular arrow (refresh). Drawn in the UI
/// blue (a coloured menu-item icon, like the status dot) so the recovery action
/// stands out from the routine items. 32×32.
fn refresh_icon() -> tauri::image::Image<'static> {
    const N: i32 = 32;
    let c = N as f32 / 2.0;
    let r = 9.0;
    let stroke = 3.0;
    // Ring with a gap at the top; an arrowhead caps the gap's clockwise end so
    // it reads as a refresh, not a plain ring.
    let gap_center = -std::f32::consts::FRAC_PI_2; // straight up
    let half_gap = 0.6_f32; // radians (~34°)
    let end = gap_center + half_gap; // clockwise end of the arc (upper-right)
    let p_end = (c + r * end.cos(), c + r * end.sin());
    // Tangent (clockwise) + radial at the arc end → a small arrowhead triangle.
    let tan = (-end.sin(), end.cos());
    let nor = (end.cos(), end.sin());
    let tip = (p_end.0 + tan.0 * 5.0, p_end.1 + tan.1 * 5.0);
    let base = (p_end.0 - tan.0 * 2.0, p_end.1 - tan.1 * 2.0);
    let b1 = (base.0 + nor.0 * 4.0, base.1 + nor.1 * 4.0);
    let b2 = (base.0 - nor.0 * 4.0, base.1 - nor.1 * 4.0);
    let mut rgba = vec![0u8; (N * N * 4) as usize];
    for y in 0..N {
        for x in 0..N {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let dx = px - c;
            let dy = py - c;
            let d = (dx * dx + dy * dy).sqrt();
            // Angular distance from the gap centre — skip the ring inside the gap.
            let ang = dy.atan2(dx);
            let mut da = (ang - gap_center).abs();
            if da > std::f32::consts::PI {
                da = std::f32::consts::TAU - da;
            }
            let ring_cov = if da > half_gap {
                (1.0 - ((d - r).abs() - stroke / 2.0)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let cov = ring_cov.max(tri_cov((px, py), tip, b1, b2));
            let i = ((y * N + x) * 4) as usize;
            rgba[i] = UI_BLUE.0;
            rgba[i + 1] = UI_BLUE.1;
            rgba[i + 2] = UI_BLUE.2;
            rgba[i + 3] = (cov * 255.0) as u8;
        }
    }
    tauri::image::Image::new_owned(rgba, N as u32, N as u32)
}

/// "Reconnect…" leading icon: a padlock. Drawn in the UI blue (like the status
/// dot / refresh) so it stands out as the recovery action. 32×32.
fn lock_icon() -> tauri::image::Image<'static> {
    const N: i32 = 32;
    let n = N as f32;
    let cx = n / 2.0;
    // Body: rounded rect in the lower half.
    let (bw, bh) = (15.0_f32, 12.0_f32);
    let (bx, by) = (cx, 21.0); // body centre
    let corner = 2.5;
    // Shackle: top half of a ring above the body.
    let s_cy = 13.0;
    let s_r = 5.0;
    let s_stroke = 2.5;
    let body_sdf = |px: f32, py: f32| -> f32 {
        let qx = (px - bx).abs() - (bw / 2.0 - corner);
        let qy = (py - by).abs() - (bh / 2.0 - corner);
        let ax = qx.max(0.0);
        let ay = qy.max(0.0);
        (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - corner
    };
    let mut rgba = vec![0u8; (N * N * 4) as usize];
    for y in 0..N {
        for x in 0..N {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let body_cov = (-body_sdf(px, py) + 0.5).clamp(0.0, 1.0);
            // Shackle: ring stroke, upper semicircle only (py above its centre).
            let d = ((px - cx).powi(2) + (py - s_cy).powi(2)).sqrt();
            let shackle_cov = if py <= s_cy {
                (1.0 - ((d - s_r).abs() - s_stroke / 2.0)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let cov = body_cov.max(shackle_cov);
            let i = ((y * N + x) * 4) as usize;
            rgba[i] = UI_BLUE.0;
            rgba[i + 1] = UI_BLUE.1;
            rgba[i + 2] = UI_BLUE.2;
            rgba[i + 3] = (cov * 255.0) as u8;
        }
    }
    tauri::image::Image::new_owned(rgba, N as u32, N as u32)
}

/// Menu-bar (Grammarly model): TypeAssist has no Dock icon and lives entirely
/// behind the tray. The main app window and the Practice panel are *shown* from
/// the tray, never the Dock. Build the tray icon + native menu in `setup`.
///
/// The menu is a native `NSMenu`, so it opens even while another app is
/// fullscreen — unlike the Practice webview panel, which is a normal window
/// (acceptable: "nobody practices typing during a fullscreen call").
fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<CheckMenuItem<R>> {
    // The locked design-doc v23 §03 "while-learning" menu, top to bottom:
    //   status · Warm-up · Progress · Open TenCalmDigits · Settings… · ─── · Quit
    // Status is a non-interactive header (disabled, so it never highlights or
    // fires); its text + dot are updated live by the debounced capture-UI
    // listener below. Active = a small filled blue dot; stopped = a small hollow
    // grey ring (see `status_dot`).
    let status = IconMenuItem::with_id(
        app,
        "status",
        status_text(true),
        false,
        Some(status_dot(true)),
        None::<&str>,
    )?;
    let practice = MenuItem::with_id(app, "practice", "Warm-up", true, None::<&str>)?;
    let progress = MenuItem::with_id(app, "progress", "Progress", true, None::<&str>)?;
    let open_main = MenuItem::with_id(app, "open_main", "Open TenCalmDigits", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    // Recovery actions — built now, but NOT in the menu while active. The
    // debounced capture-UI listener inserts exactly one of them directly under
    // the status when capture has stopped, chosen by the sidecar's permission
    // signal: "Restart capture" (re-arm/respawn in place — permission intact) or
    // "Reconnect…" (open the Reconnect panel — Accessibility was revoked). Each
    // carries a small leading icon so it stands out from the routine items.
    let restart_item = IconMenuItem::with_id(
        app,
        "restart_capture",
        "Restart capture",
        true,
        Some(refresh_icon()),
        None::<&str>,
    )?;
    let reconnect_item = IconMenuItem::with_id(
        app,
        "reconnect",
        "Reconnect…",
        true,
        Some(lock_icon()),
        None::<&str>,
    )?;
    // Corrections — the menu-bar master gate: a single, always-present item in
    // its own group above Quit. It's a CheckMenuItem, so the check on the right
    // IS the state (On = checked, Off = unchecked); tapping flips it. The initial
    // state is read off disk so it reflects the persisted gate; it then tracks
    // the engine's authoritative echo (see `setup`). Default is OFF — the engine
    // never turns it on itself, and there is no unlock/reveal gate.
    let corr_enabled = read_allow_list()
        .map(|al| al.correction_enabled)
        .unwrap_or(false);
    let corr_toggle = CheckMenuItem::with_id(
        app,
        "corr_toggle",
        "Corrections",
        true,
        corr_enabled,
        None::<&str>,
    )?;
    // Two dividers: Corrections sits in its own group, and Quit stays isolated at
    // the very bottom.
    let sep = PredefinedMenuItem::separator(app)?;
    let sep_quit = PredefinedMenuItem::separator(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit TenCalmDigits"))?;
    let menu = Menu::with_items(
        app,
        &[
            &status, &practice, &progress, &open_main, &settings, &sep, &corr_toggle, &sep_quit,
            &quit,
        ],
    )?;

    // The check item toggles itself on click; read its (already-flipped) state
    // and post it to the engine, which persists + echoes the authoritative
    // value back (keeping the check honest even if the post is dropped).
    let toggle_for_menu = corr_toggle.clone();
    TrayIconBuilder::with_id("main-tray")
        // Template image (macOS recolors it for the light/dark menu bar): the
        // plain hand glyph. TWO states only — this On icon and the capture-Off
        // icon (hand + slash, built in `engine::capture_off_icon`); the engine
        // swaps between them via `apply_tray_icon`. Signal is SHAPE, never
        // colour — see the a11y rule.
        .icon(tauri::include_image!("icons/tray-icon.png"))
        .icon_as_template(true)
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            // Quit is a PredefinedMenuItem and handled by the OS.
            "open_main" => show_main(app, "today"),
            "settings" => show_main(app, "settings"),
            "practice" => show_practice(app),
            "progress" => show_progress(app),
            // Capture-stopped recovery (permission intact): re-arm the tap /
            // respawn the sidecar in place — no System Settings round-trip.
            "restart_capture" => {
                if let Some(sender) = app.try_state::<EngineControlSender>() {
                    let _ = sender.send(EngineControl::RestartCapture);
                }
            }
            // Capture-stopped recovery (Accessibility revoked): open the
            // Reconnect panel, which deep-links to Settings and auto-resumes.
            "reconnect" => show_reconnect(app),
            "corr_toggle" => {
                let enabled = toggle_for_menu.is_checked().unwrap_or(false);
                if let Some(sender) = app.try_state::<EngineControlSender>() {
                    let _ = sender.send(EngineControl::SetCorrectionEnabled(enabled));
                }
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Let the positioner cache the tray-icon rect so the Practice panel
            // can anchor under the menu-bar icon (TrayBottomCenter).
            tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
        })
        .build(app)?;

    // Wire the status line + recovery item to the engine's DEBOUNCED capture-UI
    // signal (engine://capture-ui, payload `{ active, permission_revoked }`).
    // Using the debounced event — not raw capture-health — keeps the menu from
    // strobing on a transient tap blip: it only flips to stopped once capture
    // has stayed down past the self-heal window. On each change we (1) set the
    // status text + dot, and (2) insert/remove the right recovery item directly
    // under the status (index 1). The routine items are never touched — they
    // stay usable while stopped.
    {
        let status_item = status.clone();
        let menu_ref = menu.clone();
        let restart = restart_item.clone();
        let reconnect = reconnect_item.clone();
        app.listen(engine::EVT_CAPTURE_UI, move |event| {
            let v = serde_json::from_str::<serde_json::Value>(event.payload()).ok();
            let active = v
                .as_ref()
                .and_then(|v| v.get("active").and_then(|b| b.as_bool()))
                .unwrap_or(true);
            let permission_revoked = v
                .as_ref()
                .and_then(|v| v.get("permission_revoked").and_then(|b| b.as_bool()))
                .unwrap_or(false);
            let _ = status_item.set_text(status_text(active));
            let _ = status_item.set_icon(Some(status_dot(active)));
            // Reconcile the recovery item. remove() on an absent item is a
            // harmless Err, so clearing both first keeps this idempotent.
            let _ = menu_ref.remove(&restart);
            let _ = menu_ref.remove(&reconnect);
            if !active {
                let item: &dyn IsMenuItem<R> = if permission_revoked {
                    &reconnect
                } else {
                    &restart
                };
                let _ = menu_ref.insert(item, 1);
            }
        });
    }

    Ok(corr_toggle)
}

// ---- Main-window position memory --------------------------------------------
// The main window is fixed-size and non-resizable; it HIDES (not closes) on
// Cmd+W, so its position is already kept within a running session. These two
// helpers persist that position across full app restarts. Window chrome only —
// writes a tiny `{x,y}` file to the app config dir, never user/recovery data,
// never ~/.typeassist.

fn main_position_path<R: Runtime>(app: &AppHandle<R>) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("main-window-position.json"))
}

fn save_main_position<R: Runtime>(app: &AppHandle<R>, x: i32, y: i32) {
    if let Some(path) = main_position_path(app) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, format!("{{\"x\":{x},\"y\":{y}}}"));
    }
}

/// Restore the saved main-window position (if any). No-op on first run, so the
/// window falls back to the config's `center: true`.
fn restore_main_position<R: Runtime>(app: &AppHandle<R>) {
    let Some(path) = main_position_path(app) else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    if let (Some(x), Some(y)) = (
        v.get("x").and_then(serde_json::Value::as_i64),
        v.get("y").and_then(serde_json::Value::as_i64),
    ) {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
        }
    }
}

/// Show + focus the main window and route it. The main window starts hidden
/// (menu-bar-only); the route is delivered via `app://route`, which `App.svelte`
/// listens for. The webview stays loaded across hide/show, so its listener
/// persists — no race on re-open.
fn show_main<R: Runtime>(app: &AppHandle<R>, route: &str) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        let _ = app.emit("app://route", route);
    }
}

/// Anchor the Practice panel under the tray icon, show + focus it. It hides on
/// blur (see `on_window_event`), so it behaves like a menu-bar dropdown.
///
/// Warm-up is a full-screen takeover, not a second window: hide the main window
/// first so the warm-up panel is the only thing on screen (no two windows
/// competing when launched from Today). When warm-up ends — finish, close, or
/// blur — it just hides and returns to the menu bar; nothing here reopens the
/// main window, so a launch from Today and a launch from the menu end the same
/// way. Hiding main *before* showing practice avoids any focus shuffle blurring
/// the practice window into the hide-on-blur path.
fn show_practice<R: Runtime>(app: &AppHandle<R>) {
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.hide();
        // A native hide doesn't fire visibilitychange, so signal the webview to
        // drop keyboard focus — same as the CloseRequested path — or a stale
        // focus ring returns on the next open.
        let _ = app.emit("app://main-hidden", ());
    }
    if let Some(w) = app.get_webview_window("practice") {
        let _ = w.move_window(Position::TrayBottomCenter);
        let _ = w.show();
        let _ = w.set_focus();
        let _ = app.emit("practice://open", ());
        // C5e: a warm-up / Practice round is now on screen — its typed text is
        // app-generated, so tell the engine to keep it out of the word-freq
        // vocabulary tally (the motor map still observes). Cleared on blur-hide.
        if let Some(sender) = app.try_state::<EngineControlSender>() {
            let _ = sender.send(EngineControl::SetPromptedCaptureActive(true));
        }
    }
}

/// Anchor the Progress dashboard panel under the tray icon, show + focus it.
/// Same menu-bar-dropdown behaviour as Practice (hides on blur). The panel is
/// read-only; `progress://open` tells it to (re)load its data on each open.
fn show_progress<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("progress") {
        let _ = w.move_window(Position::TrayBottomCenter);
        let _ = w.show();
        let _ = w.set_focus();
        let _ = app.emit("progress://open", ());
    }
}

/// Tauri command: ask the engine to reconstruct a per-key slip-rate trend
/// from the weekly motor-map snapshots and emit it on `engine://practice-trend`.
/// Practice calls this at the snapshot with the keys the round leaned into; the
/// reply arrives asynchronously on the event. Read-only history scan.
#[tauri::command]
fn request_practice_trend(
    keys: Vec<char>,
    sender: tauri::State<EngineControlSender>,
) -> Result<(), String> {
    sender
        .send(EngineControl::RequestPracticeTrend { keys })
        .map_err(|e| format!("engine control channel closed: {e}"))
}

/// Tauri command: open the Practice panel from inside the app (e.g. Warm-up's
/// "Start practicing" hand-off). Same path the tray's "Practice mode" item
/// uses — anchors under the menu-bar icon, shows + focuses, hides on blur.
#[tauri::command]
fn open_practice(app: AppHandle) {
    show_practice(&app);
}

/// Tauri command: open the Progress dashboard panel from inside the app. Mirrors
/// `open_practice` — same tray-anchored, hide-on-blur presentation.
#[tauri::command]
fn open_progress(app: AppHandle) {
    show_progress(&app);
}

/// Tauri command: the menu-bar Impact glance's "See the full list in the app →"
/// link. Shows + focuses the main window and routes it to the Progress tab — the
/// integrated keyboard+corrections view, in its default all-corrections state (no
/// key selected). Reuses the existing `show_main` / `app://route` deep-link; the
/// caller hides the popover.
#[tauri::command]
fn open_main_progress(app: AppHandle) {
    show_main(&app, "progress");
}

/// Show + focus the Reconnect panel (its own webview window, label "reconnect").
/// Surfaced by the menu-bar "Reconnect…" recovery item when the sidecar reports
/// Accessibility was revoked. Centred (not tray-anchored), and — unlike the
/// other panels — it deliberately does NOT hide on blur: the user has to leave
/// it to flip the switch in System Settings, so it must survive that round-trip
/// and stay up until it auto-resumes. `reconnect://open` (re)starts its
/// permission poll; it dismisses itself once capture comes back (see
/// `ReconnectPanel.svelte`).
fn show_reconnect<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("reconnect") {
        // Re-arm always-on-top in case a previous open dropped it to reach the
        // Settings toggle (see `reconnect_open_accessibility`) — a fresh open
        // should surface above other apps.
        let _ = w.set_always_on_top(true);
        let _ = w.center();
        let _ = w.show();
        let _ = w.set_focus();
        let _ = app.emit("reconnect://open", ());
    }
}

/// Step the Reconnect panel out of the way so the user can reach a Settings
/// toggle. The reconnect window is `alwaysOnTop` (so it surfaces over other apps
/// when the menu opens it); left as-is it floats above System Settings and hides
/// the very switch the user came to flip. Dropping always-on-top lets the
/// subsequent `open` bring System Settings frontmost — the panel sits behind it.
/// The panel stays OPEN (it never hides on blur), so it survives the round-trip
/// and is ready to surface its "reconnected" confirmation via `reconnect_surface`.
fn step_reconnect_aside<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("reconnect") {
        let _ = w.set_always_on_top(false);
    }
}

/// Tauri command: from the Reconnect panel, open System Settings ▸ Accessibility
/// (one of the two grants capture needs) and step the panel aside.
#[tauri::command]
fn reconnect_open_accessibility(app: AppHandle) {
    step_reconnect_aside(&app);
    open_accessibility_settings();
}

/// Tauri command: from the Reconnect panel, open System Settings ▸ Input
/// Monitoring (the other grant capture needs) and step the panel aside. Re-granting
/// Accessibility alone does NOT restore capture — the keystroke tap needs this
/// one — so the panel offers a button for each pane.
#[tauri::command]
fn reconnect_open_input_monitoring(app: AppHandle) {
    step_reconnect_aside(&app);
    open_input_monitoring_settings();
}

/// Tauri command: bring the Reconnect panel back to the front to show its
/// "reconnected" confirmation. Capture returns while System Settings is
/// frontmost, so we re-activate our window over it and restore always-on-top
/// (dropped by `reconnect_open_accessibility`) so the confirmation is what the
/// user sees.
#[tauri::command]
fn reconnect_surface(app: AppHandle) {
    if let Some(w) = app.get_webview_window("reconnect") {
        let _ = w.set_always_on_top(true);
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Deep-link a specific System Settings ▸ Privacy & Security pane by its
/// `x-apple.systempreferences:` anchor. Pure UI convenience — launches the OS
/// settings pane and reads/writes no user data.
fn open_privacy_pane(anchor: &str) {
    let _ = std::process::Command::new("open")
        .arg(format!(
            "x-apple.systempreferences:com.apple.preference.security?{anchor}"
        ))
        .spawn();
}

/// Tauri command: open System Settings ▸ Privacy & Security ▸ Accessibility, the
/// grant the AX API (secure-field focus, correction injection) needs. Used by
/// first-run onboarding.
#[tauri::command]
fn open_accessibility_settings() {
    open_privacy_pane("Privacy_Accessibility");
}

/// Tauri command: open System Settings ▸ Privacy & Security ▸ Input Monitoring,
/// the grant the keystroke-capture CGEventTap needs. SEPARATE from Accessibility
/// and revoked independently (e.g. by an app update), so the reconnect flow must
/// be able to point the user here too.
#[tauri::command]
fn open_input_monitoring_settings() {
    open_privacy_pane("Privacy_ListenEvent");
}

/// Tauri command: bring the main window back to the front. Used by onboarding
/// step 2 after the Accessibility grant — System Settings is frontmost at that
/// point, so we re-activate our own window (we never force-close System Settings).
#[tauri::command]
fn focus_main_window(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Tauri command: flip the global correction master gate. The panel's master
/// switch posts this; the engine persists + echoes `corrections://state`, which
/// the panel and the tray check both render from. (The tray check has its own
/// one-click path in `build_tray`.)
#[tauri::command]
fn set_correction_enabled(
    enabled: bool,
    sender: tauri::State<EngineControlSender>,
) -> Result<(), String> {
    sender
        .send(EngineControl::SetCorrectionEnabled(enabled))
        .map_err(|e| format!("engine control channel closed: {e}"))
}

/// Tauri command: ask the engine to (re)emit the current allow-list on
/// `corrections://state`. The panel calls this on open.
#[tauri::command]
fn request_allow_list(sender: tauri::State<EngineControlSender>) -> Result<(), String> {
    sender
        .send(EngineControl::RequestAllowList)
        .map_err(|e| format!("engine control channel closed: {e}"))
}

/// Tauri command: read `~/.typeassist/allow_list.json` straight off disk for the
/// panel's first paint (so it shows the right state before the engine echo
/// arrives). Read-only; a missing file is the shipped-dark default (gate off, no
/// patterns), not an error. The engine remains the sole *writer*.
#[tauri::command]
fn read_allow_list() -> Result<AllowList, String> {
    let path = match typeassist_data_dir() {
        Some(dir) => dir.join("allow_list.json"),
        None => return Ok(AllowList::new()),
    };
    AllowList::load_from(&path).map_err(|e| format!("could not read allow_list.json: {e}"))
}

/// FIRE_TIMING helper — wall-clock UNIX-epoch ms, the SAME clock as the engine's
/// `now_ms()` and the webview's `Date.now()`, so every t0..t4 stamp is directly
/// comparable on this machine. Measurement only.
fn fire_timing_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// FIRE_TIMING t4 — the bubble webview reports back the instant it received the
/// `corrections://suggested` event (t3_js) and the instant the card painted
/// (t4_paint, next animation frame). Logged here so the whole t0..t4 chain lands
/// in one dev log on one clock. Measurement only — does not affect firing.
#[tauri::command]
fn log_bubble_timing(t3_js: u64, t4_paint: u64) {
    tracing::info!(
        "FIRE_TIMING_UI t3_js={} t4_paint={} d_t3_t4_ms={}",
        t3_js,
        t4_paint,
        t4_paint.saturating_sub(t3_js),
    );
}

/// DEV-ONLY: force-show the correction bubble with dummy content (the Cmd+Shift+P
/// overlay's "Show bubble" button). Emits a synthetic `corrections://suggested`
/// so the cue window shows (Rust listener) AND the bubble renders (JS listener) —
/// isolating the UI/window path from the engine's fire decision. The button that
/// calls this is excluded from release builds.
#[tauri::command]
fn dev_show_bubble(app: AppHandle) {
    let _ = app.emit(
        engine::EVT_CORRECTION_SUGGESTED,
        serde_json::json!({ "typed": "waht", "target": "what", "highlight": [0, 1] }),
    );
}

/// The on-device data directory the read-only Progress commands resolve files
/// against. Mirrors the engine's `typeassist_dir`: `TYPEASSIST_DATA_DIR`
/// overrides (the dev-safety scratch valve), else `~/.typeassist`. `None` when
/// neither is set — the caller treats that as "no data yet".
fn typeassist_data_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("TYPEASSIST_DATA_DIR") {
        return Some(std::path::PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".typeassist"))
}

/// One learned correction, flattened for the Progress → Impact ledger. `obs` is
/// the pattern's **decayed** weight (the same number the kill-switch gates on),
/// so a long-idle pattern reads as the lower weight the engine actually sees.
/// `class` is the coordination/precision tag, **derived on read** from the
/// `typed→target` pair (`"coord"` / `"precis"`), so nothing new is persisted to
/// the store. Non-motor pairs (apostrophe/casing/grammar fixes) are filtered out
/// of this ledger entirely, so `class` is always present in practice.
#[derive(serde::Serialize)]
struct ImpactPattern {
    typed: String,
    target: String,
    obs: f32,
    ready: bool,
    class: Option<&'static str>,
    /// Char indices in `target` of the letter(s) the correction changed, so the
    /// Impact view can softly mark them (`couod → could` ⇒ `[3]`, the `l`).
    /// Derived on read; never persisted. Empty when the fix only removed a key.
    highlight: Vec<usize>,
    /// The keyboard keys this correction involves, so the Progress map can filter
    /// the panel to a selected key (coordination: the transposed pair; precision:
    /// the wrong + intended key). Derived on read; never persisted.
    keys: Vec<char>,
}

/// Tauri command: read `~/.typeassist/word_patterns.json` straight off disk and
/// return the learned patterns for the Impact tab. This is the read that
/// **replaces** scraping `KILL_SWITCH_DUMP` from Console — fully read-only, no
/// engine round-trip (the file is the source of truth, flushed every ~2s). A
/// missing file (nothing learned yet) is not an error: it returns an empty list.
///
/// Decay is applied via the store's own `snapshots()` so "N obs" matches the
/// engine's view exactly; rows are sorted by weight desc (the brief's order).
#[tauri::command]
fn read_word_patterns() -> Result<Vec<ImpactPattern>, String> {
    let path = match typeassist_data_dir() {
        Some(dir) => dir.join("word_patterns.json"),
        None => return Err("no data directory (HOME unset)".into()),
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let store = correction_engine::WordPatternStore::load_from(&path)
        .map_err(|e| format!("could not read word_patterns.json: {e}"))?;
    let lexicon = correction_engine::Lexicon::shared();
    // Load the motor map too, so the ledger's readiness reflects the same
    // Motor-Map-aware (affected-key) evidence bar the engine uses. Best-effort:
    // a missing/unreadable map → an empty map → no easing (the base bar), never
    // an error. Read-only.
    let motor_map = typeassist_data_dir()
        .map(|d| d.join("motor_map.json"))
        .filter(|p| p.exists())
        .and_then(|p| correction_engine::MotorMap::load_from(&p).ok())
        .unwrap_or_default();
    let mut out: Vec<ImpactPattern> = store
        .snapshots()
        .into_iter()
        .filter_map(|s| {
            use correction_engine::{MotorEdit, ObserveReason, PatternReadiness};
            // Reconciled with the risk-tiered, suggest-only classifier (the same
            // `classify` the engine uses). The ledger shows only **genuine motor
            // slips** — pairs that pass the structural gates (single-motor shape,
            // real-word target, multi-letter both ends). The "changed my mind"
            // word-swaps (`so → for`), single-letter cases (`s → is`, `f → of`),
            // non-word targets, and cross-token merge fragments (`have → havif`)
            // all fall into a structural `Observe` reason and are dropped here, so
            // they no longer masquerade as slips. (Still captured per Principle #7
            // where applicable — just not surfaced.)
            //
            // `ready` is the pattern's REAL readiness from the classifier
            // (`Suggest` clears its risk-tiered bar — low for a non-word source,
            // high for a real-word source), not a flat-12 cut.
            let ready =
                match correction_engine::classify(&s.typed, &s.target, &store, lexicon, &motor_map)
                {
                    PatternReadiness::Suggest { .. } => true,
                    // Genuine slip, just not yet actionable — show it as "observing".
                    PatternReadiness::Observe {
                        reason:
                            ObserveReason::InsufficientEvidence
                            | ObserveReason::Stale
                            | ObserveReason::BrakeTripped,
                    } => false,
                    // Not a motor slip at all (swap / single-letter / non-word target
                    // / merge / unseen) → drop from the ledger.
                    PatternReadiness::Observe { .. } => return None,
                };
            // Group tag from the strict motor shape, consistent with the filter
            // above (transposition = coordination; everything else = precision).
            // `single_motor_edit` is guaranteed `Some` for a surviving row.
            let class = match correction_engine::single_motor_edit(&s.typed, &s.target)? {
                MotorEdit::Transposition => "coord",
                MotorEdit::AdjacentSubstitution
                | MotorEdit::DroppedLetter
                | MotorEdit::ExtraLetter => "precis",
            };
            let highlight = correction_engine::corrected_target_indices(&s.typed, &s.target);
            let keys = correction_engine::involved_keys(&s.typed, &s.target);
            Some(ImpactPattern {
                typed: s.typed,
                target: s.target,
                obs: s.weight,
                ready,
                class: Some(class),
                highlight,
                keys,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.obs
            .partial_cmp(&a.obs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(out)
}

/// One calendar day's typing rollup, as the Statistics tab reads it. Mirrors
/// the engine's on-disk `DailyEntry`; `coord + precis == slips`.
#[derive(serde::Serialize, serde::Deserialize)]
struct ProgressDay {
    date: String,
    words: u64,
    slips: u64,
    coord: u64,
    precis: u64,
}

#[derive(serde::Deserialize)]
struct ProgressFile {
    #[allow(dead_code)]
    version: u32,
    days: Vec<ProgressDay>,
}

/// Tauri command: read `~/.typeassist/progress_snapshots.json` straight off
/// disk for the Statistics tab — the accumulated daily history (oldest→newest),
/// each day's words/slips/coord/precis. The engine flushes today's row live
/// (~2s), so the most recent entry is "today" as soon as the user has typed.
/// Read-only; a missing file (no typing yet) returns an empty list, not an
/// error. The UI derives Words-today / Slip-rate from the last entry, the
/// 7-day bars from the last 7, and the trend lines from the whole history.
#[tauri::command]
fn read_progress_stats() -> Result<Vec<ProgressDay>, String> {
    let path = match typeassist_data_dir() {
        Some(dir) => dir.join("progress_snapshots.json"),
        None => return Err("no data directory (HOME unset)".into()),
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("could not read progress file: {e}"))?;
    let parsed: ProgressFile = serde_json::from_slice(&bytes)
        .map_err(|e| format!("could not parse progress file: {e}"))?;
    Ok(parsed.days)
}

/// One letter key's two trouble scores for the Progress keyboard view — the
/// per-key expansion of the Statistics Coordination/Precision numbers. Both are
/// **raw decayed rates in `0..=1`**; the Svelte side scales colour intensity
/// *relative to the user's own worst key* (the brief), so no absolute scale is
/// baked in here.
///
/// - `precision` — "right key, clean hit" targeting. Like `coordination`, the
///   numerator is sourced from the learned word-pattern corrections gated by the
///   SAME `classify` filter the Impact ledger uses (adjacent-sub / dropped /
///   extra-letter slips), attributed to `involved_keys`, over the key's motor-map
///   `productions`. (Pre-2026-06-28 this read the motor map's raw per-key slip
///   rate, which could shade a key the corrections list had nothing to show for —
///   the two surfaces disagreed.)
/// - `coordination` — the share of this key's productions implicated in a
///   learned adjacent-transposition correction (letter-order slips). Sourced
///   from `word_patterns.json`, attributed to both swapped keys, and gated by
///   the SAME genuine-slip `classify` filter the Impact ledger uses (so the
///   keyboard and the corrections list agree — non-word-target junk swaps don't
///   shade a key here either).
/// - `productions` — decayed times the key was typed: the figure revealed on
///   tap, and the denominator behind both rates.
/// - `well_sampled` — `productions >= MIN_SAMPLES`. Below the bar the UI paints
///   the key neutral rather than inventing a (falsely good) score.
#[derive(serde::Serialize)]
struct KeyScore {
    key: String,
    precision: f32,
    coordination: f32,
    productions: f32,
    well_sampled: bool,
}

/// Tauri command: read `motor_map.json` (precision) and `word_patterns.json`
/// (coordination) straight off disk and return a per-key score for **every**
/// QWERTY letter `a..=z` — the data behind the Progress keyboard map. Fully
/// read-only; missing files (nothing learned yet) yield all-neutral keys, not an
/// error. Decay is whatever the engine last flushed (values are decayed to each
/// store's `last_now`, ~2s fresh) — a true "now" snapshot, no re-decay here.
///
/// Letters the motor map has never seen stay at zero / `well_sampled = false`,
/// so the board always renders a full keyboard with honest empty keys.
#[tauri::command]
fn read_key_scores() -> Result<Vec<KeyScore>, String> {
    use std::collections::BTreeMap;

    let dir = match typeassist_data_dir() {
        Some(d) => d,
        None => return Ok(Vec::new()),
    };

    // Seed all 26 letters neutral so the keyboard is always complete; the data
    // files only ever upgrade a key away from "no evidence".
    let mut keys: BTreeMap<char, KeyScore> = ('a'..='z')
        .map(|c| {
            (
                c,
                KeyScore {
                    key: c.to_string(),
                    precision: 0.0,
                    coordination: 0.0,
                    productions: 0.0,
                    well_sampled: false,
                },
            )
        })
        .collect();

    // Denominator + sampling, from the motor map: `productions` (how many times
    // the key was typed) and `well_sampled`. This is the ONE thing the motor map
    // supplies to this view — both lenses divide by `productions` so they read on
    // a "share of presses" scale, and a key under MIN_SAMPLES paints neutral.
    // Punctuation / digits the map tracks have no key on the board, so skipped.
    let motor_path = dir.join("motor_map.json");
    if motor_path.exists() {
        let map = correction_engine::MotorMap::load_from(&motor_path)
            .map_err(|e| format!("could not read motor_map.json: {e}"))?;
        for s in map.key_stats() {
            if let Some(entry) = keys.get_mut(&s.key) {
                entry.productions = s.total;
                entry.well_sampled = s.total >= correction_engine::MIN_SAMPLES;
            }
        }
    }

    // BOTH lenses' numerators come from the SAME source as the Impact corrections
    // ledger (`read_word_patterns`): the learned `word_patterns.json`, run through
    // the same risk-tiered `classify` gate, attributed to the same `involved_keys`
    // the ledger filters by. This is what keeps the keyboard and the corrections
    // list in lock-step: a key shades for a lens ⇔ there is a genuine correction
    // of that class touching it ⇔ clicking the key shows that correction (never an
    // empty panel).
    //
    // - GENUINE gate (2026-06-28): a pattern counts only if `classify` returns
    //   `Suggest` or a not-yet-actionable `Observe` (`InsufficientEvidence` /
    //   `Stale` / `BrakeTripped`) — never a structural `Observe` (word-swap /
    //   single-letter / NON-WORD TARGET / merge / unseen). Without it, junk like
    //   `transpiostion → transpoistion` (non-word target) lit up its keys here
    //   while the list correctly showed "none" — the two surfaces disagreed.
    // - CLASS via the strict motor shape (`single_motor_edit`), exactly as the
    //   ledger groups it: transposition → coordination; adjacent-sub / dropped /
    //   extra letter → precision.
    let pattern_path = dir.join("word_patterns.json");
    if pattern_path.exists() {
        use correction_engine::{MotorEdit, ObserveReason, PatternReadiness};
        let store = correction_engine::WordPatternStore::load_from(&pattern_path)
            .map_err(|e| format!("could not read word_patterns.json: {e}"))?;
        let lexicon = correction_engine::Lexicon::shared();
        // Best-effort motor map for the affected-key evidence bar (same easing
        // the ledger applies): missing/unreadable → empty map → base bar, never
        // an error. Read-only.
        let class_motor_map = {
            let p = dir.join("motor_map.json");
            if p.exists() {
                correction_engine::MotorMap::load_from(&p)
                    .ok()
                    .unwrap_or_default()
            } else {
                correction_engine::MotorMap::default()
            }
        };
        let mut coord_weight: BTreeMap<char, f32> = BTreeMap::new();
        let mut precis_weight: BTreeMap<char, f32> = BTreeMap::new();
        for p in store.snapshots() {
            // Same genuine-slip gate as `read_word_patterns`: keep `Suggest` and
            // the not-yet-actionable reasons; drop every structural `Observe`.
            let genuine = match correction_engine::classify(
                &p.typed,
                &p.target,
                &store,
                lexicon,
                &class_motor_map,
            ) {
                PatternReadiness::Suggest { .. } => true,
                PatternReadiness::Observe {
                    reason:
                        ObserveReason::InsufficientEvidence
                        | ObserveReason::Stale
                        | ObserveReason::BrakeTripped,
                } => true,
                PatternReadiness::Observe { .. } => false,
            };
            if !genuine {
                continue;
            }
            // Strict motor shape selects the lens bucket (guaranteed `Some` for a
            // surviving row, but skip defensively if not).
            let bucket = match correction_engine::single_motor_edit(&p.typed, &p.target) {
                Some(MotorEdit::Transposition) => &mut coord_weight,
                Some(
                    MotorEdit::AdjacentSubstitution
                    | MotorEdit::DroppedLetter
                    | MotorEdit::ExtraLetter,
                ) => &mut precis_weight,
                None => continue,
            };
            // Attribute to the SAME keys the ledger filters by, so shading and the
            // click-to-filter panel always agree.
            for k in correction_engine::involved_keys(&p.typed, &p.target) {
                *bucket.entry(k).or_insert(0.0) += p.weight;
            }
        }
        // A key's rate is its class weight over its productions; no productions →
        // stays 0 (can't be a rate), matching the "seen but clean" empty state.
        for (k, w) in coord_weight {
            if let Some(entry) = keys.get_mut(&k) {
                if entry.productions > 0.0 {
                    entry.coordination = (w / entry.productions).min(1.0);
                }
            }
        }
        for (k, w) in precis_weight {
            if let Some(entry) = keys.get_mut(&k) {
                if entry.productions > 0.0 {
                    entry.precision = (w / entry.productions).min(1.0);
                }
            }
        }
    }

    Ok(keys.into_values().collect())
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_positioner::init())
        .invoke_handler(tauri::generate_handler![
            reset_lexicon,
            set_learning_paused,
            set_input_paused,
            restart_capture,
            request_motor_stability,
            request_practice_trend,
            open_practice,
            open_progress,
            open_main_progress,
            open_accessibility_settings,
            open_input_monitoring_settings,
            reconnect_open_accessibility,
            reconnect_open_input_monitoring,
            reconnect_surface,
            focus_main_window,
            set_correction_enabled,
            request_allow_list,
            read_allow_list,
            dev_show_bubble,
            log_bubble_timing,
            read_word_patterns,
            read_progress_stats,
            read_key_scores
        ])
        .on_window_event(|window, event| match event {
            // Menu-bar app: a window's close button / Cmd+W must NOT quit the
            // app or destroy the window — hide it so the tray can reopen it.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
                // The main window only HIDES, never closes — so its webview's
                // last-focused element keeps its :focus, and WebKit restores it
                // on the next open (visibilitychange does NOT fire on a native
                // menu-bar hide, so the JS-side blur never runs). Signal the
                // webview explicitly so it can clear keyboard focus while
                // hidden; otherwise the stale element shows a second focus ring
                // on reopen. Emitted while the webview is still alive (hidden ≠
                // destroyed), so the listener runs before the next show.
                if window.label() == "main" {
                    let _ = window.app_handle().emit("app://main-hidden", ());
                }
            }
            // The Practice and Progress panels are dropdowns: clicking away
            // (losing focus) dismisses them. Practice keeps focus while typing,
            // so an active round never hides; Progress is read-only.
            WindowEvent::Focused(false)
                if window.label() == "practice" || window.label() == "progress" =>
            {
                let _ = window.hide();
                // C5e: the warm-up / Practice round is gone — resume feeding the
                // word-freq vocabulary tally from ambient (real) typing.
                if window.label() == "practice" {
                    if let Some(sender) = window.app_handle().try_state::<EngineControlSender>() {
                        let _ = sender.send(EngineControl::SetPromptedCaptureActive(false));
                    }
                }
            }
            // Remember where the user puts the main window, across restarts.
            WindowEvent::Moved(pos) if window.label() == "main" => {
                save_main_position(window.app_handle(), pos.x, pos.y);
            }
            _ => {}
        })
        .setup(|app| {
            // No Dock icon — TypeAssist is a quiet menu-bar companion. Must run
            // before any window would otherwise activate the app in the Dock.
            #[cfg(target_os = "macos")]
            let _ = app.set_activation_policy(ActivationPolicy::Accessory);

            // Put the (hidden) main window back where the user last left it.
            restore_main_position(app.handle());

            let corr_toggle = build_tray(app.handle())?;

            // Make the native tray menu's first keyboard focus land on the
            // topmost ACTIONABLE item (skipping the disabled status header),
            // in every state — not AppKit's stale default. See `menu_focus`.
            #[cfg(target_os = "macos")]
            menu_focus::install();

            // Keep the menu-bar master-gate check in sync with the engine's
            // authoritative state: the engine echoes the whole allow-list on
            // `corrections://state` after any mutation (panel toggle, tray
            // click, or an Escape teach-stop). The tray check follows that, so
            // it's correct even when the gate is flipped from the panel.
            {
                let toggle = corr_toggle.clone();
                app.handle()
                    .listen(engine::EVT_CORRECTION_STATE, move |event| {
                        if let Ok(al) = serde_json::from_str::<AllowList>(event.payload()) {
                            let _ = toggle.set_checked(al.correction_enabled);
                        }
                    });
            }

            // M3 correction bubble — the suggest/accept/undo HUD. Rust owns
            // show + position (top-right, no positioner JS dependency) and the
            // window is `focus: false`, so it never steals the caret from the app
            // the user is typing in. We show on BOTH a new suggestion and an
            // apply/undo; the bubble webview owns its own HIDE (it knows the
            // stage-dependent lifetimes — ~5s suggest, 6s accepted-undo, brief
            // familiar), so there's no Rust timer racing the UI's fade.
            {
                let handle = app.handle().clone();
                let show_cue = move |h: &tauri::AppHandle| {
                    if let Some(w) = h.get_webview_window("cue") {
                        let _ = w.move_window(Position::TopRight);
                        let _ = w.show();
                    }
                };
                let h_sug = handle.clone();
                let show_sug = show_cue.clone();
                app.handle()
                    .listen(engine::EVT_CORRECTION_SUGGESTED, move |_| {
                        // FIRE_TIMING t3 — the Tauri main process received the
                        // suggestion event and is about to show the cue window.
                        // (The webview's JS listener fires separately; t3_js/t4
                        // come from log_bubble_timing.) Measurement only.
                        tracing::info!("FIRE_TIMING t3_rust={}", fire_timing_now_ms());
                        show_sug(&h_sug);
                    });
                let h_app = handle.clone();
                app.handle()
                    .listen(engine::EVT_CORRECTION_APPLIED, move |_| show_cue(&h_app));
            }

            match engine::spawn(&app.handle()) {
                Ok(control_tx) => {
                    // Hand the control sender to Tauri's managed
                    // state so #[tauri::command] handlers can fetch
                    // it via `tauri::State`. The receiver is owned
                    // by the engine task.
                    app.manage(control_tx);
                }
                Err(e) => {
                    tracing::error!("failed to spawn engine: {e}");
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
