//! Layer 5 — Tauri shell.
//!
//! Hosts the Svelte webview, spawns the Swift sidecar (Layer 1), and wires its
//! event stream into the Rust behavioural model (Layer 2).

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|_app| {
            // TODO: spawn the Swift sidecar, parse its line-delimited JSON events,
            // and feed them into a BehaviouralModel. See CLAUDE.md.
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
