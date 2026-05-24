//! Engine host — spawns the Swift sidecar (L1) and runs the walking-skeleton
//! capture → decide → inject loop in-process.
//!
//! L2 (`BehaviouralModel`) observes every keystroke; its `timing` aggregator is
//! the only one with real logic for now, the rest are safe no-ops. The
//! correction *decision* is still the walking-skeleton `skeleton_lookup` — L2
//! is observe-only and doesn't influence behaviour.
//!
//! Three measurements per keystroke, so the debug view can attribute time:
//!   - `ingest_latency_ms`    — just the L2 dispatch (on the keystroke event)
//!   - `decision_latency_ms`  — keystroke arrival → decision made (includes ingest)
//!   - `injection_latency_ms` — keystroke arrival → sidecar.stdin write returned
//!
//! Events emitted to the debug panel:
//!   `engine://keystroke`       — every key (or backspace) + dwell + ingest cost
//!   `engine://decision`        — at a word boundary, with decision latency
//!   `engine://injection`       — when a correction fired, with end-to-end latency
//!   `engine://model-snapshot`  — L2 state after each ingested event

use std::time::Instant;

use behavioural_model::{BehaviouralModel, InputEvent, OutboundCommand};
use correction_engine::{boundary_char, is_word_char, skeleton_lookup};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

pub const EVT_KEYSTROKE: &str = "engine://keystroke";
pub const EVT_DECISION: &str = "engine://decision";
pub const EVT_INJECTION: &str = "engine://injection";
pub const EVT_MODEL_SNAPSHOT: &str = "engine://model-snapshot";
/// Fired once per confirmed slip detected by L2's `SlipDetector` during
/// ingest. The debug panel marks these in the feed.
pub const EVT_SLIP: &str = "engine://slip";

#[derive(Serialize, Clone)]
struct KeystrokePayload {
    key: String,
    dwell_ms: u32,
    /// Wall time the L2 `BehaviouralModel::ingest` call took for this event.
    /// Surfaced per-row so we can watch the observe-only L2 wiring stay cheap.
    ingest_latency_ms: f64,
}

#[derive(Serialize, Clone)]
struct DecisionPayload {
    word: String,
    matched: bool,
    replacement: Option<String>,
    /// Time from receiving the boundary keystroke to the decision being made.
    /// Includes the ingest cost above — measured from key arrival.
    decision_latency_ms: f64,
}

#[derive(Serialize, Clone)]
struct InjectionPayload {
    delete_count: u32,
    /// Full string sent to the sidecar (includes the trailing boundary char).
    replacement: String,
    /// End-to-end: keystroke received → sidecar.stdin write returned.
    injection_latency_ms: f64,
}

pub fn spawn<R: Runtime>(app: &AppHandle<R>) -> Result<(), Box<dyn std::error::Error>> {
    // TYPEASSIST_AX_PROMPT=1 asks the sidecar to pop the macOS Accessibility
    // dialog if the permission is missing — appropriate now that the Tauri app
    // is the engine's host (CLAUDE.md: "leaves the prompt to L5").
    let sidecar = app
        .shell()
        .sidecar("typeassist-input-macos")?
        .env("TYPEASSIST_AX_PROMPT", "1");

    let (mut rx, mut child) = sidecar.spawn()?;
    let app_handle = app.clone();

    tauri::async_runtime::spawn(async move {
        // Word currently being typed, assembled from key events — same state
        // shape the skeleton binary uses.
        let mut word = String::new();
        // L2 lives here for the life of the engine. Single owner, single async
        // task — no sync needed.
        let mut model = BehaviouralModel::new();

        while let Some(event) = rx.recv().await {
            match event {
                CommandEvent::Stdout(bytes) => {
                    // Plugin already splits on newline; one event = one line.
                    let line = match std::str::from_utf8(&bytes) {
                        Ok(s) => s.trim(),
                        Err(_) => continue,
                    };
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(parsed) = serde_json::from_str::<InputEvent>(line) else {
                        continue;
                    };

                    // t0 for the existing decision/injection latencies. Captured
                    // BEFORE ingest so those numbers continue to mean
                    // "key arrival → X" (i.e. include the ingest cost).
                    let t_received = Instant::now();

                    // Observe-only L2 dispatch. Only Key/Backspace flow into
                    // the model — sidecar lifecycle events (Ready/Shutdown/…)
                    // aren't keystrokes. Measured on its own so the debug view
                    // can show what L2 is costing per keystroke.
                    let ingest_latency_ms = if matches!(
                        parsed,
                        InputEvent::Key { .. } | InputEvent::Backspace { .. }
                    ) {
                        let t_ingest = Instant::now();
                        model.ingest(&parsed);
                        let lat = t_ingest.elapsed().as_secs_f64() * 1000.0;
                        // Drain any slips L2 confirmed during this ingest
                        // and broadcast them — one event per slip so the
                        // panel can mark each in the feed in order.
                        for slip in model.take_new_slips() {
                            let _ = app_handle.emit(EVT_SLIP, slip);
                        }
                        // Broadcast the new model state so the debug "Model
                        // state" tables update live as the user types.
                        let _ = app_handle.emit(EVT_MODEL_SNAPSHOT, model.snapshot());
                        lat
                    } else {
                        0.0
                    };

                    match parsed {
                        InputEvent::Ready => {
                            tracing::info!("sidecar ready — engine listening");
                        }
                        InputEvent::PermissionRequired => {
                            tracing::warn!(
                                "sidecar reports Accessibility permission missing — \
                                 grant in System Settings › Privacy & Security › Accessibility"
                            );
                        }
                        InputEvent::Shutdown => break,
                        InputEvent::Backspace { .. } => {
                            // Emit so the debug feed shows backspaces — they're
                            // signal, not noise (CLAUDE.md: self-corrections).
                            let _ = app_handle.emit(
                                EVT_KEYSTROKE,
                                KeystrokePayload {
                                    key: "⌫".to_string(),
                                    dwell_ms: 0,
                                    ingest_latency_ms,
                                },
                            );
                            word.pop();
                        }
                        InputEvent::Key { key, dwell_ms, .. } => {
                            let _ = app_handle.emit(
                                EVT_KEYSTROKE,
                                KeystrokePayload {
                                    key: key.clone(),
                                    dwell_ms,
                                    ingest_latency_ms,
                                },
                            );

                            if let Some(boundary) = boundary_char(&key) {
                                let lookup_result = skeleton_lookup(&word);
                                let decision_latency_ms =
                                    t_received.elapsed().as_secs_f64() * 1000.0;

                                // Skip empty-word boundaries (e.g. typing two
                                // spaces in a row): no decision was made, so
                                // emitting a row would just be noise.
                                if !word.is_empty() {
                                    let _ = app_handle.emit(
                                        EVT_DECISION,
                                        DecisionPayload {
                                            word: word.clone(),
                                            matched: lookup_result.is_some(),
                                            replacement: lookup_result.map(String::from),
                                            decision_latency_ms,
                                        },
                                    );
                                }

                                if let Some(replacement) = lookup_result {
                                    let delete_count = word.chars().count() as u32 + 1;
                                    let full_replacement = format!("{replacement}{boundary}");
                                    let cmd = OutboundCommand::InjectCorrection {
                                        delete_count,
                                        replacement: full_replacement.clone(),
                                    };
                                    if let Ok(mut json) = serde_json::to_string(&cmd) {
                                        json.push('\n');
                                        match child.write(json.as_bytes()) {
                                            Ok(()) => {
                                                let injection_latency_ms =
                                                    t_received.elapsed().as_secs_f64() * 1000.0;
                                                let _ = app_handle.emit(
                                                    EVT_INJECTION,
                                                    InjectionPayload {
                                                        delete_count,
                                                        replacement: full_replacement,
                                                        injection_latency_ms,
                                                    },
                                                );
                                                tracing::info!(
                                                    "corrected {:?} → {:?} (decision {:.2}ms, e2e {:.2}ms)",
                                                    word,
                                                    replacement,
                                                    decision_latency_ms,
                                                    injection_latency_ms,
                                                );
                                            }
                                            Err(e) => {
                                                tracing::error!(
                                                    "sidecar stdin write failed: {e}"
                                                );
                                            }
                                        }
                                    }
                                }
                                word.clear();
                            } else if is_word_char(&key) {
                                word.push_str(&key);
                            } else {
                                // Unmodeled key (arrow, escape, …) ends the word.
                                word.clear();
                            }
                        }
                    }
                }
                CommandEvent::Stderr(bytes) => {
                    if let Ok(s) = std::str::from_utf8(&bytes) {
                        let trimmed = s.trim_end();
                        if !trimmed.is_empty() {
                            tracing::info!(target: "sidecar", "{trimmed}");
                        }
                    }
                }
                CommandEvent::Error(msg) => {
                    tracing::error!("sidecar error: {msg}");
                }
                CommandEvent::Terminated(payload) => {
                    tracing::warn!(
                        "sidecar terminated (code={:?}, signal={:?})",
                        payload.code,
                        payload.signal
                    );
                    break;
                }
                _ => {}
            }
        }
    });

    Ok(())
}
