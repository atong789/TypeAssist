//! Durable, crash-safe persistence shared by every on-disk JSON store under the
//! data dir (`typeassist_dir`): the motor map, word-pattern store, guesser
//! ledger, allow-list, progress snapshots, and the dated snapshot archives.
//!
//! Two guarantees, both load-bearing for a recovery-tracking app where a lost
//! or truncated store is the same failure class as a silent capture drop
//! (CLAUDE.md Principles #6/#7):
//!
//! * **Durable atomic write** ([`durable_write`]) — temp file → `fsync` temp →
//!   rename over target → `fsync` the parent directory. A crash or power-loss
//!   between steps can't leave a zero-length or stale file: either the old
//!   complete file or the new complete file survives, never a torn one.
//! * **Quarantine on corruption** ([`quarantine_corrupt`]) — a store file that
//!   exists but fails to parse is moved aside to `<stem>.corrupt-<unix_ms>.json`
//!   and the caller starts empty. Never an error, never a silent wipe: the bad
//!   bytes are preserved for inspection and the app comes up usable.
//!
//! It lives in L4 (the lowest crate every store-owner can reach) rather than in
//! a store: the Tauri host's allow-list and progress writers use it too. It
//! touches only `std::fs` — no OS-specific APIs — so L4 stays portable.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Atomically and durably write `bytes` to `path`.
///
/// Sequence: ensure the parent dir exists → write a sibling `*.json.tmp` →
/// [`File::sync_all`] it (fsync data + metadata) → rename over `path` → fsync
/// the parent directory so the rename itself is durable. The parent-dir fsync
/// is best-effort (some filesystems error on opening a directory for sync); the
/// temp-file fsync + atomic rename are the parts the durability guarantee rests
/// on, so a dir-fsync failure does not fail the write.
///
/// [`File::sync_all`]: std::fs::File::sync_all
pub fn durable_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?; // fsync the temp file before it becomes the target
    }
    fs::rename(&tmp, path)?;
    sync_parent_dir(path);
    Ok(())
}

/// Best-effort fsync of `path`'s parent directory, so the preceding rename
/// survives power-loss. Failure is ignored (directory fsync is not supported on
/// every filesystem, and the rename + temp fsync already give atomicity).
fn sync_parent_dir(path: &Path) {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if let Ok(dir) = fs::File::open(&parent) {
        let _ = dir.sync_all();
    }
}

/// Move a corrupt store file aside to `<stem>.corrupt-<unix_ms>.json` and log a
/// warning. Returns the quarantine path on success. **Never deletes data** — the
/// moved-aside file is preserved for recovery/inspection; the caller then starts
/// the store empty. A failure to rename (e.g. permissions) is logged and yields
/// `None`, still without erroring the load.
pub fn quarantine_corrupt(path: &Path, parse_err: impl std::fmt::Display) -> Option<PathBuf> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("store");
    let dest = path.with_file_name(format!("{stem}.corrupt-{}.json", unix_millis()));
    match fs::rename(path, &dest) {
        Ok(()) => {
            tracing::warn!(
                "STORE_QUARANTINE path={:?} moved_to={:?} reason={}",
                path,
                dest,
                parse_err
            );
            Some(dest)
        }
        Err(rename_err) => {
            tracing::warn!(
                "STORE_QUARANTINE_FAILED path={:?} reason={} rename_err={}",
                path,
                parse_err,
                rename_err
            );
            None
        }
    }
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ta_persist_{}_{}", std::process::id(), name))
    }

    #[test]
    fn durable_write_then_read_round_trips() {
        let path = scratch("durable.json");
        let _ = fs::remove_file(&path);
        durable_write(&path, b"{\"hello\":1}").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"{\"hello\":1}");
        // No leftover temp file beside it.
        assert!(!path.with_extension("json.tmp").exists());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn durable_write_overwrites_existing_atomically() {
        let path = scratch("overwrite.json");
        durable_write(&path, b"old").unwrap();
        durable_write(&path, b"new-and-longer").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new-and-longer");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn quarantine_moves_file_aside_and_preserves_bytes() {
        let path = scratch("corrupt.json");
        fs::write(&path, b"not json at all").unwrap();
        let dest = quarantine_corrupt(&path, "deliberate parse error").expect("renamed");
        // Original is gone, quarantine carries the original bytes (never wiped).
        assert!(!path.exists(), "corrupt file moved aside");
        assert!(dest.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"not json at all");
        assert!(dest
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(".corrupt-"));
        let _ = fs::remove_file(&dest);
    }
}
