//! Layer 5 — Tauri shell.
//!
//! Hosts the Svelte webview and the engine (Swift sidecar + walking-skeleton
//! loop). See `engine.rs`.

mod engine;

use engine::{EngineControl, EngineControlSender};
use tauri::Manager;

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

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![reset_lexicon, set_learning_paused])
        .setup(|app| {
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
