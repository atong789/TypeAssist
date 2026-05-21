//! Walking-skeleton daemon — the thinnest end-to-end thread.
//!
//! Spawns the macOS Swift sidecar (L1), reads its key events over the stdio
//! JSON bridge, assembles the current word, and on the single hardcoded typo
//! `"tge"` sends back an `inject_correction` command. That's the whole job:
//! prove the Swift↔Rust bridge and keystroke injection work.
//!
//! No learning, no L2 aggregation, no L3 volatility map, no lexicon. See
//! CLAUDE.md "walking skeleton" — everything here is glue around
//! `correction_engine::skeleton_lookup`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use behavioural_model::{InputEvent, OutboundCommand};
use correction_engine::skeleton_lookup;

fn main() {
    let raw_path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: skeleton <path-to-swift-sidecar>");
        std::process::exit(64);
    });

    // Absolute path: used both to spawn the sidecar and (on the permission
    // error) to tell the user exactly which binary to add in System Settings.
    let sidecar_path = std::fs::canonicalize(&raw_path)
        .map(|p| p.display().to_string())
        .unwrap_or(raw_path);

    let mut child = Command::new(&sidecar_path)
        // Skeleton-only: ask the sidecar to pop the macOS Accessibility dialog
        // if the permission is missing. Production L1 leaves the prompt to L5.
        .env("TYPEASSIST_AX_PROMPT", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| {
            eprintln!("[skeleton] failed to launch sidecar at {sidecar_path}: {e}");
            std::process::exit(1);
        });

    let mut sidecar_stdin = child.stdin.take().expect("piped stdin");
    let reader = BufReader::new(child.stdout.take().expect("piped stdout"));

    // The word currently being typed, assembled from key events.
    let mut word = String::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        // Anything that isn't a known event (or our own future event types) is
        // ignored rather than fatal — the bridge stays forgiving.
        let Ok(event) = serde_json::from_str::<InputEvent>(&line) else {
            continue;
        };

        match event {
            InputEvent::Ready => {
                eprintln!("[skeleton] sidecar ready — type \"tge \" (with a space) in any app.");
            }
            InputEvent::PermissionRequired => {
                print_permission_help(&sidecar_path);
                break;
            }
            InputEvent::Shutdown => break,
            InputEvent::Backspace { .. } => {
                word.pop();
            }
            InputEvent::Key { key, .. } => {
                if let Some(boundary) = boundary_char(&key) {
                    if let Some(replacement) = skeleton_lookup(&word) {
                        // Delete the typed word + the boundary char, retype both.
                        let delete_count = word.chars().count() as u32 + 1;
                        send(
                            &mut sidecar_stdin,
                            &OutboundCommand::InjectCorrection {
                                delete_count,
                                replacement: format!("{replacement}{boundary}"),
                            },
                        );
                        eprintln!("[skeleton] corrected \"{word}\" -> \"{replacement}\"");
                    }
                    word.clear();
                } else if is_word_char(&key) {
                    word.push_str(&key);
                } else {
                    // A key we don't model (arrows, escape, …) ends the word.
                    word.clear();
                }
            }
        }
    }

    let _ = child.wait();
}

/// If `key` is a single word-boundary character, return it; else `None`.
fn boundary_char(key: &str) -> Option<char> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    c.is_whitespace().then_some(c)
}

/// A single printable, non-whitespace, non-control character belongs to a word.
fn is_word_char(key: &str) -> bool {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => !c.is_control() && !c.is_whitespace(),
        _ => false,
    }
}

fn send(stdin: &mut impl Write, cmd: &OutboundCommand) {
    if let Ok(mut json) = serde_json::to_string(cmd) {
        json.push('\n');
        let _ = stdin.write_all(json.as_bytes());
        let _ = stdin.flush();
    }
}

fn print_permission_help(sidecar_path: &str) {
    eprintln!();
    eprintln!("──────────────────────────────────────────────────────────────");
    eprintln!(" TypeAssist needs Accessibility permission to watch and correct");
    eprintln!(" your typing. macOS just showed (or will show) a permission dialog.");
    eprintln!();
    eprintln!(" To grant it:");
    eprintln!("   1. In the dialog, click \"Open System Settings\".");
    eprintln!("      (If you dismissed it: Apple menu  > System Settings >");
    eprintln!("       Privacy & Security > Accessibility.)");
    eprintln!("   2. Find \"typeassist-input-macos\" in the list and turn it ON.");
    eprintln!("      If it isn't listed, click \"+\", press Cmd+Shift+G, and paste:");
    eprintln!("        {sidecar_path}");
    eprintln!("   3. Re-run:  just skeleton");
    eprintln!("──────────────────────────────────────────────────────────────");
    // Best-effort: open the right Settings pane for them.
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status();
}
