//! Layer 5 — Tauri shell.
//!
//! Hosts the Svelte webview and the engine (Swift sidecar + walking-skeleton
//! loop). See `engine.rs`.

mod allow_list;
mod engine;

use allow_list::AllowList;
use engine::{EngineControl, EngineControlSender};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
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

/// Menu-bar (Grammarly model): TypeAssist has no Dock icon and lives entirely
/// behind the tray. The main app window and the Practice panel are *shown* from
/// the tray, never the Dock. Build the tray icon + native menu in `setup`.
///
/// The menu is a native `NSMenu`, so it opens even while another app is
/// fullscreen — unlike the Practice webview panel, which is a normal window
/// (acceptable: "nobody practices typing during a fullscreen call").
fn build_tray<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<CheckMenuItem<R>> {
    let open_main = MenuItem::with_id(app, "open_main", "Open TypeAssist", true, None::<&str>)?;
    let practice = MenuItem::with_id(app, "practice", "Practice mode", true, None::<&str>)?;
    let progress = MenuItem::with_id(app, "progress", "Progress", true, None::<&str>)?;
    // M3 correction Step 1 — the menu-bar master gate (instant, one-action
    // global on/off, the brief's "global off") + the curation panel opener. The
    // check's initial state is read off disk so it reflects the persisted gate;
    // it then tracks the engine's authoritative echo (see `setup`). Default is
    // OFF — the feature ships dark.
    let corr_enabled = read_allow_list()
        .map(|al| al.correction_enabled)
        .unwrap_or(false);
    let corr_toggle = CheckMenuItem::with_id(
        app,
        "corr_toggle",
        "Enable corrections",
        true,
        corr_enabled,
        None::<&str>,
    )?;
    let corrections = MenuItem::with_id(app, "corrections", "Corrections…", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let sep_a = PredefinedMenuItem::separator(app)?;
    let sep_b = PredefinedMenuItem::separator(app)?;
    let sep_c = PredefinedMenuItem::separator(app)?;
    let quit = PredefinedMenuItem::quit(app, Some("Quit TypeAssist"))?;
    let menu = Menu::with_items(
        app,
        &[
            &open_main,
            &practice,
            &progress,
            &sep_a,
            &corr_toggle,
            &corrections,
            &sep_b,
            &settings,
            &sep_c,
            &quit,
        ],
    )?;

    // The check item toggles itself on click; read its (already-flipped) state
    // and post it to the engine, which persists + echoes the authoritative
    // value back (keeping the check honest even if the post is dropped).
    let toggle_for_menu = corr_toggle.clone();
    TrayIconBuilder::with_id("main-tray")
        // Template image: macOS recolors it for the light/dark menu bar. The
        // "weak keys worth practicing" state is shown by SHAPE (a badge dot in
        // tray-icon-dot.png), never colour — see the a11y rule.
        .icon(tauri::include_image!("icons/tray-icon.png"))
        .icon_as_template(true)
        .menu(&menu)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            // Quit is a PredefinedMenuItem and handled by the OS.
            "open_main" => show_main(app, "today"),
            "settings" => show_main(app, "settings"),
            "practice" => show_practice(app),
            "progress" => show_progress(app),
            "corrections" => show_corrections(app),
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
    Ok(corr_toggle)
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

/// Anchor the Corrections (allow-list) panel under the tray icon, show + focus
/// it. Same menu-bar-dropdown behaviour as Practice/Progress (hides on blur).
/// `corrections://open` tells it to (re)load its state on each open.
fn show_corrections<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("allowlist") {
        let _ = w.move_window(Position::TrayBottomCenter);
        let _ = w.show();
        let _ = w.set_focus();
        let _ = app.emit("corrections://open", ());
    }
}

/// Tauri command: open the Corrections allow-list panel from inside the app.
#[tauri::command]
fn open_corrections(app: AppHandle) {
    show_corrections(&app);
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

/// Tauri command: enable (add) or disable (remove) one `typed → target` pattern
/// in the allow-list. The panel's per-pattern toggle posts this; the engine
/// persists + echoes `corrections://state`.
#[tauri::command]
fn set_pattern_enabled(
    typed: String,
    target: String,
    enabled: bool,
    sender: tauri::State<EngineControlSender>,
) -> Result<(), String> {
    sender
        .send(EngineControl::SetPatternEnabled {
            typed,
            target,
            enabled,
        })
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
/// the store; `None` for the rare pair that isn't a clean motor slip.
#[derive(serde::Serialize)]
struct ImpactPattern {
    typed: String,
    target: String,
    obs: f32,
    ready: bool,
    class: Option<&'static str>,
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
    let mut out: Vec<ImpactPattern> = store
        .snapshots()
        .into_iter()
        .map(|s| {
            let class = correction_engine::classify_slip(&s.typed, &s.target).map(|c| c.as_tag());
            ImpactPattern {
                typed: s.typed,
                target: s.target,
                obs: s.weight,
                ready: s.weight >= correction_engine::TIER1_MIN_OBSERVATIONS,
                class,
            }
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
/// - `precision` — the motor map's per-key mis-hit rate (`slips / productions`):
///   "right key, clean hit" targeting.
/// - `coordination` — the share of this key's productions implicated in a
///   learned adjacent-transposition correction (letter-order slips). Sourced
///   from `word_patterns.json`, attributed to both swapped keys.
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

    // Precision: per-key mis-hit rate straight from the motor map. Punctuation
    // / digits the map tracks have no key on the board, so they're skipped.
    let motor_path = dir.join("motor_map.json");
    if motor_path.exists() {
        let map = correction_engine::MotorMap::load_from(&motor_path)
            .map_err(|e| format!("could not read motor_map.json: {e}"))?;
        for s in map.key_stats() {
            if let Some(entry) = keys.get_mut(&s.key) {
                entry.precision = s.slip_rate;
                entry.productions = s.total;
                entry.well_sampled = s.total >= correction_engine::MIN_SAMPLES;
            }
        }
    }

    // Coordination: attribute each learned adjacent-transposition pattern's
    // decayed weight to BOTH intended keys whose order slipped, then divide by
    // that key's productions so it reads on the same "share of presses" scale as
    // precision. A key with no productions stays at 0 (can't be a rate).
    let pattern_path = dir.join("word_patterns.json");
    if pattern_path.exists() {
        let store = correction_engine::WordPatternStore::load_from(&pattern_path)
            .map_err(|e| format!("could not read word_patterns.json: {e}"))?;
        let mut coord_weight: BTreeMap<char, f32> = BTreeMap::new();
        for p in store.snapshots() {
            if let Some((a, b)) = correction_engine::transposition_keys(&p.typed, &p.target) {
                *coord_weight.entry(a).or_insert(0.0) += p.weight;
                *coord_weight.entry(b).or_insert(0.0) += p.weight;
            }
        }
        for (k, w) in coord_weight {
            if let Some(entry) = keys.get_mut(&k) {
                if entry.productions > 0.0 {
                    entry.coordination = (w / entry.productions).min(1.0);
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
            open_corrections,
            set_correction_enabled,
            set_pattern_enabled,
            request_allow_list,
            read_allow_list,
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
            }
            // The Practice and Progress panels are dropdowns: clicking away
            // (losing focus) dismisses them. Practice keeps focus while typing,
            // so an active round never hides; Progress is read-only.
            WindowEvent::Focused(false)
                if window.label() == "practice"
                    || window.label() == "progress"
                    || window.label() == "allowlist" =>
            {
                let _ = window.hide();
            }
            _ => {}
        })
        .setup(|app| {
            // No Dock icon — TypeAssist is a quiet menu-bar companion. Must run
            // before any window would otherwise activate the app in the Dock.
            #[cfg(target_os = "macos")]
            let _ = app.set_activation_policy(ActivationPolicy::Accessory);

            let corr_toggle = build_tray(app.handle())?;

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

            // M3 correction Step 1 — the visible cue. On every applied
            // correction (and its undo) briefly show the small HUD near the
            // top-right of the screen, then hide it. Rust owns show / position /
            // hide so the cue needs no positioner JS dependency and is shown
            // WITHOUT focus (the window is also `focus: false`), so it never
            // steals the caret from the app the user is typing in. The cue
            // webview renders the `typed → target` text from the same event.
            {
                let handle = app.handle().clone();
                app.handle()
                    .listen(engine::EVT_CORRECTION_APPLIED, move |_| {
                        if let Some(w) = handle.get_webview_window("cue") {
                            let _ = w.move_window(Position::TopRight);
                            let _ = w.show();
                            let w_hide = w.clone();
                            tauri::async_runtime::spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
                                let _ = w_hide.hide();
                            });
                        }
                    });
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
