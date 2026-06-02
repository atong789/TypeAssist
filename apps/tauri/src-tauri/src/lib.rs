//! Layer 5 — Tauri shell.
//!
//! Hosts the Svelte webview and the engine (Swift sidecar + walking-skeleton
//! loop). See `engine.rs`.

mod engine;

use engine::{EngineControl, EngineControlSender};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    ActivationPolicy, AppHandle, Emitter, Manager, Runtime, WindowEvent,
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

/// Menu-bar (Grammarly model): TypeAssist has no Dock icon and lives entirely
/// behind the tray. The main app window and the Practice panel are *shown* from
/// the tray, never the Dock. Build the tray icon + native menu in `setup`.
///
/// The menu is a native `NSMenu`, so it opens even while another app is
/// fullscreen — unlike the Practice webview panel, which is a normal window
/// (acceptable: "nobody practices typing during a fullscreen call").
fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let open_main = MenuItem::with_id(app, "open_main", "Open TypeAssist", true, None::<&str>)?;
    let practice = MenuItem::with_id(app, "practice", "Practice mode", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let sep_a = PredefinedMenuItem::separator(app)?;
    let sep_b = PredefinedMenuItem::separator(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit TypeAssist"))?;
    let menu = Menu::with_items(
        app,
        &[&open_main, &practice, &sep_a, &settings, &sep_b, &quit],
    )?;

    TrayIconBuilder::with_id("main-tray")
        // Template image: macOS recolors it for the light/dark menu bar. The
        // "weak keys worth practicing" state is shown by SHAPE (a badge dot in
        // tray-icon-dot.png), never colour — see the a11y rule.
        .icon(tauri::include_image!("icons/tray-icon.png"))
        .icon_as_template(true)
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            // Quit is a PredefinedMenuItem and handled by the OS.
            "open_main" => show_main(app, "today"),
            "settings" => show_main(app, "settings"),
            "practice" => show_practice(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Let the positioner cache the tray-icon rect so the Practice panel
            // can anchor under the menu-bar icon (TrayBottomCenter).
            tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
        })
        .build(app)?;
    Ok(())
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
fn show_practice<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("practice") {
        let _ = w.move_window(Position::TrayBottomCenter);
        let _ = w.show();
        let _ = w.set_focus();
        let _ = app.emit("practice://open", ());
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
            open_practice
        ])
        .on_window_event(|window, event| match event {
            // Menu-bar app: a window's close button / Cmd+W must NOT quit the
            // app or destroy the window — hide it so the tray can reopen it.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            // The Practice panel is a dropdown: clicking away (losing focus)
            // dismisses it. Typing keeps focus, so an active round never hides.
            WindowEvent::Focused(false) if window.label() == "practice" => {
                let _ = window.hide();
            }
            _ => {}
        })
        .setup(|app| {
            // No Dock icon — TypeAssist is a quiet menu-bar companion. Must run
            // before any window would otherwise activate the app in the Dock.
            #[cfg(target_os = "macos")]
            let _ = app.set_activation_policy(ActivationPolicy::Accessory);

            build_tray(app.handle())?;

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
