//! Layer 5 — Tauri shell.
//!
//! Hosts the Svelte webview and the engine (Swift sidecar + walking-skeleton
//! loop). See `engine.rs`.

mod engine;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            if let Err(e) = engine::spawn(&app.handle()) {
                tracing::error!("failed to spawn engine: {e}");
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
