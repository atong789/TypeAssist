//! On-device backup bundle — the single OS-portable, versioned file behind the
//! Settings **Back up / Restore / Delete** flow.
//!
//! **RULE 1 — OS-portable.** The bundle holds ONLY the user's learned data: each
//! store's JSON content *verbatim*, plus the dated history keyed by **date**
//! (never by file path). There are **no machine paths and no OS-specific
//! fields**, so a backup made on macOS restores into a future Windows/Android
//! build — the runtime data dir is resolved by the caller at read/write time and
//! never stored inside the file.
//!
//! **RULE 2 — versioned.** [`CURRENT_SCHEMA_VERSION`] is a real integer stamped
//! in the file. Restore reads it and handles older versions gracefully (missing
//! fields fill sensible defaults via serde), refuses a *newer*-than-known file
//! cleanly (never a silent half-load), and validates the WHOLE bundle before any
//! write (validate-all-then-apply ⇒ no partial restore).
//!
//! Decay is preserved by construction: each store serializes its `last_now` /
//! per-entry `last_update` timestamps, so copying the JSON verbatim means the
//! 30-day decay **continues the existing curve** after restore rather than
//! resetting to "now" or treating the map as fully stale.
//!
//! All disk I/O lives here (Rust), per the app's architecture: the webview never
//! touches the filesystem. Writes into `~/.typeassist` go through the engine task
//! (the sole writer); this module is the shared file-layout knowledge both the
//! `create_backup` command (read-only) and the engine's restore/delete handlers
//! (write) call.

use correction_engine::persist::durable_write;
use correction_engine::{MotorMap, WordFreq, WordPatternStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

/// The current backup ENVELOPE schema version. Distinct from each embedded
/// store's own internal `version` field — both are honored on restore.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Identity guard so we never try to restore an unrelated JSON file.
pub const BACKUP_FORMAT: &str = "tencalmdigits-backup";

// On-device file/dir names. The ONLY place the layout is spelled out; resolved
// against a runtime-supplied data dir so nothing OS-specific lands in the file.
const MOTOR_MAP_FILE: &str = "motor_map.json";
const WORD_PATTERNS_FILE: &str = "word_patterns.json";
const WORD_FREQ_FILE: &str = "word_freq.json";
const PROGRESS_SNAPSHOTS_FILE: &str = "progress_snapshots.json";
const GUESS_ACCURACY_FILE: &str = "guess_accuracy.json";
const SHADOW_LOG_FILE: &str = "shadow_suggestions.log";
const SNAPSHOTS_DIR: &str = "snapshots";
const WORD_FREQ_SNAPSHOTS_DIR: &str = "word_freq_snapshots";

/// The backup envelope. Embedded stores are kept as raw [`Value`] so the bytes
/// (and the decay timestamps inside them) round-trip untouched.
#[derive(Debug, Serialize, Deserialize)]
pub struct BackupBundle {
    /// Always [`BACKUP_FORMAT`]. Checked on restore.
    pub format: String,
    /// Integer envelope version (RULE 2).
    pub schema_version: u32,
    /// Epoch millis when written — OS-neutral, informational (no date lib).
    #[serde(default)]
    pub created_ms: u64,
    /// App version that wrote it — informational only.
    #[serde(default)]
    pub app_version: String,
    /// The learned data. Missing members default to absent/empty.
    #[serde(default)]
    pub data: BackupData,
}

/// The learned-data payload. Every member is optional so an older backup that
/// predates a store still restores (the missing store becomes empty — a true
/// replace, never a merge).
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct BackupData {
    #[serde(default)]
    pub motor_map: Option<Value>,
    #[serde(default)]
    pub word_patterns: Option<Value>,
    #[serde(default)]
    pub word_freq: Option<Value>,
    #[serde(default)]
    pub progress_snapshots: Option<Value>,
    /// `"YYYY-MM-DD"` → that day's motor-map snapshot JSON.
    #[serde(default)]
    pub motor_snapshots: BTreeMap<String, Value>,
    /// `"YYYY-MM-DD"` → that day's word-freq snapshot JSON.
    #[serde(default)]
    pub word_freq_snapshots: BTreeMap<String, Value>,
}

/// A small read-only summary for the restore confirm dialog — lets the UI show
/// what it's about to overwrite *before* anything is touched.
#[derive(Serialize)]
pub struct RestoreSummary {
    pub schema_version: u32,
    pub created_ms: u64,
    pub motor_keys: usize,
    pub word_patterns: usize,
    pub vocab_words: usize,
    pub history_days: usize,
}

// ---- Build (back up) — read-only on the data dir -------------------------

/// Read a JSON file into a [`Value`], or `None` if it's absent or unparseable.
/// Backup is best-effort per file: a single corrupt archive shouldn't sink the
/// whole backup (the engine quarantines corrupt live stores anyway).
fn read_json(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Read every `*.json` in a dated-archive dir into a `date → Value` map. The
/// filename stem is the date (`YYYY-MM-DD`); the path itself never enters the
/// bundle (RULE 1).
fn read_dated_dir(dir: &Path) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(v) = read_json(&path) {
            out.insert(stem.to_string(), v);
        }
    }
    out
}

/// Assemble the bundle from the on-device data dir and serialize it to pretty
/// JSON bytes. Read-only — never writes into `data_dir`.
pub fn build_bundle(data_dir: &Path, app_version: &str, created_ms: u64) -> Result<Vec<u8>, String> {
    let bundle = BackupBundle {
        format: BACKUP_FORMAT.to_string(),
        schema_version: CURRENT_SCHEMA_VERSION,
        created_ms,
        app_version: app_version.to_string(),
        data: BackupData {
            motor_map: read_json(&data_dir.join(MOTOR_MAP_FILE)),
            word_patterns: read_json(&data_dir.join(WORD_PATTERNS_FILE)),
            word_freq: read_json(&data_dir.join(WORD_FREQ_FILE)),
            progress_snapshots: read_json(&data_dir.join(PROGRESS_SNAPSHOTS_FILE)),
            motor_snapshots: read_dated_dir(&data_dir.join(SNAPSHOTS_DIR)),
            word_freq_snapshots: read_dated_dir(&data_dir.join(WORD_FREQ_SNAPSHOTS_DIR)),
        },
    };
    let mut bytes =
        serde_json::to_vec_pretty(&bundle).map_err(|e| format!("could not serialize backup: {e}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

// ---- Parse + validate (restore) — read-only -------------------------------

/// Parse the bytes of a backup file and validate the envelope (RULE 2).
///
/// * wrong `format` → reject (not one of ours);
/// * `schema_version` **>** [`CURRENT_SCHEMA_VERSION`] → reject cleanly ("made by
///   a newer version") — honest, never a silent half-load;
/// * `schema_version` **≤** current → accept; missing members already defaulted
///   by serde.
pub fn parse_bundle(bytes: &[u8]) -> Result<BackupBundle, String> {
    let bundle: BackupBundle = serde_json::from_slice(bytes)
        .map_err(|_| "This file isn't a TenCalmDigits backup.".to_string())?;
    if bundle.format != BACKUP_FORMAT {
        return Err("This file isn't a TenCalmDigits backup.".to_string());
    }
    if bundle.schema_version > CURRENT_SCHEMA_VERSION {
        return Err(
            "This backup was made by a newer version of TenCalmDigits. Update the app to restore it."
                .to_string(),
        );
    }
    Ok(bundle)
}

/// Confirm the three core stores actually deserialize into their real types, so
/// restore is all-or-nothing: if a core store is malformed we abort *before* any
/// write. Absent cores are fine (they restore as empty — a true replace).
pub fn validate_core(bundle: &BackupBundle) -> Result<(), String> {
    if let Some(v) = &bundle.data.motor_map {
        serde_json::from_value::<MotorMap>(v.clone())
            .map_err(|e| format!("backup's motor map is unreadable: {e}"))?;
    }
    if let Some(v) = &bundle.data.word_patterns {
        serde_json::from_value::<WordPatternStore>(v.clone())
            .map_err(|e| format!("backup's word patterns are unreadable: {e}"))?;
    }
    if let Some(v) = &bundle.data.word_freq {
        serde_json::from_value::<WordFreq>(v.clone())
            .map_err(|e| format!("backup's vocabulary is unreadable: {e}"))?;
    }
    Ok(())
}

/// A glanceable summary for the confirm dialog. Counts are best-effort (a key
/// count off by a field doesn't matter); the point is to show the user roughly
/// what they're restoring.
pub fn summarize(bundle: &BackupBundle) -> RestoreSummary {
    let count_obj = |v: &Option<Value>, key: &str| -> usize {
        v.as_ref()
            .and_then(|v| v.get(key))
            .and_then(|m| m.as_object().map(|o| o.len()).or_else(|| m.as_array().map(|a| a.len())))
            .unwrap_or(0)
    };
    let days = bundle
        .data
        .progress_snapshots
        .as_ref()
        .and_then(|v| v.get("days"))
        .and_then(|d| d.as_array().map(|a| a.len()))
        .unwrap_or(0);
    RestoreSummary {
        schema_version: bundle.schema_version,
        created_ms: bundle.created_ms,
        motor_keys: count_obj(&bundle.data.motor_map, "dists"),
        word_patterns: count_obj(&bundle.data.word_patterns, "patterns"),
        vocab_words: count_obj(&bundle.data.word_freq, "counts"),
        history_days: days.max(bundle.data.motor_snapshots.len()),
    }
}

// ---- Apply (restore) + delete — WRITE into the data dir -------------------
// Called ONLY from the engine task (the sole writer of ~/.typeassist).

fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn remove_dir_if_present(dir: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Clear the **learned + history** stores only (the restore footprint): the
/// three core files, the progress history, and both dated-archive dirs. Leaves
/// diagnostics (`guess_accuracy`, the shadow log) and config (`allow_list`)
/// untouched — restore replaces *learning*, not the machine's config/diagnostics.
fn clear_learned(data_dir: &Path) -> io::Result<()> {
    remove_if_present(&data_dir.join(MOTOR_MAP_FILE))?;
    remove_if_present(&data_dir.join(WORD_PATTERNS_FILE))?;
    remove_if_present(&data_dir.join(WORD_FREQ_FILE))?;
    remove_if_present(&data_dir.join(PROGRESS_SNAPSHOTS_FILE))?;
    remove_dir_if_present(&data_dir.join(SNAPSHOTS_DIR))?;
    remove_dir_if_present(&data_dir.join(WORD_FREQ_SNAPSHOTS_DIR))?;
    Ok(())
}

fn write_value(path: &Path, v: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(v).map_err(|e| format!("serialize {path:?}: {e}"))?;
    durable_write(path, &bytes).map_err(|e| format!("write {path:?}: {e}"))
}

fn write_dated(dir: &Path, files: &BTreeMap<String, Value>) -> Result<(), String> {
    if files.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create {dir:?}: {e}"))?;
    for (date, v) in files {
        // `date` is a bare `YYYY-MM-DD` stem from the bundle; guard against any
        // path separators so a crafted key can't escape the archive dir.
        if date.contains(['/', '\\']) || date.contains("..") {
            return Err(format!("invalid snapshot date key: {date:?}"));
        }
        write_value(&dir.join(format!("{date}.json")), v)?;
    }
    Ok(())
}

/// Apply a validated bundle into `data_dir`: clear the learned/history footprint,
/// then write whatever the bundle carries (absent members stay cleared ⇒ a true
/// replace, never a merge). Call [`parse_bundle`] + [`validate_core`] first.
pub fn apply_bundle(bundle: &BackupBundle, data_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|e| format!("create {data_dir:?}: {e}"))?;
    clear_learned(data_dir).map_err(|e| format!("clear before restore: {e}"))?;

    if let Some(v) = &bundle.data.motor_map {
        write_value(&data_dir.join(MOTOR_MAP_FILE), v)?;
    }
    if let Some(v) = &bundle.data.word_patterns {
        write_value(&data_dir.join(WORD_PATTERNS_FILE), v)?;
    }
    if let Some(v) = &bundle.data.word_freq {
        write_value(&data_dir.join(WORD_FREQ_FILE), v)?;
    }
    if let Some(v) = &bundle.data.progress_snapshots {
        write_value(&data_dir.join(PROGRESS_SNAPSHOTS_FILE), v)?;
    }
    write_dated(&data_dir.join(SNAPSHOTS_DIR), &bundle.data.motor_snapshots)?;
    write_dated(&data_dir.join(WORD_FREQ_SNAPSHOTS_DIR), &bundle.data.word_freq_snapshots)?;
    Ok(())
}

/// Erase **all** on-device learning + history + diagnostics so the app returns to
/// a first-launch-clean disk. Does NOT touch `allow_list.json` — the engine
/// handler resets the correction gate to off in memory and persists that
/// separately, so the live in-memory state and the file stay consistent.
pub fn delete_all_data(data_dir: &Path) -> Result<(), String> {
    clear_learned(data_dir).map_err(|e| format!("delete learned: {e}"))?;
    remove_if_present(&data_dir.join(GUESS_ACCURACY_FILE)).map_err(|e| format!("delete diagnostics: {e}"))?;
    remove_if_present(&data_dir.join(SHADOW_LOG_FILE)).map_err(|e| format!("delete log: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write(dir: &Path, name: &str, v: &Value) {
        std::fs::write(dir.join(name), serde_json::to_vec_pretty(v).unwrap()).unwrap();
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("tcd-backup-test-{tag}-{:?}", std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    // A motor map carries decay timestamps; they must survive the round-trip
    // verbatim (so decay continues the curve after restore, not reset to now).
    fn motor_json() -> Value {
        json!({
            "version": 2,
            "dists": { "r": { "correct_count": 12.5, "incorrect": {}, "last_update": 1700000000000u64 } },
            "last_now": 1700000000000u64,
            "total_observations": 7
        })
    }

    #[test]
    fn build_then_parse_roundtrips_and_preserves_timestamps() {
        let dir = temp_dir("rt");
        write(&dir, MOTOR_MAP_FILE, &motor_json());
        write(&dir, WORD_FREQ_FILE, &json!({ "version": 1, "counts": { "need": 7.0 }, "last_update": 0 }));
        std::fs::create_dir_all(dir.join(SNAPSHOTS_DIR)).unwrap();
        write(&dir.join(SNAPSHOTS_DIR), "2026-06-28.json", &motor_json());

        let bytes = build_bundle(&dir, "test", 123).unwrap();
        let bundle = parse_bundle(&bytes).unwrap();
        assert_eq!(bundle.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(bundle.created_ms, 123);
        // timestamps intact
        assert_eq!(bundle.data.motor_map.as_ref().unwrap()["last_now"], json!(1700000000000u64));
        assert_eq!(bundle.data.motor_snapshots.len(), 1);
        assert!(bundle.data.motor_snapshots.contains_key("2026-06-28"));
        validate_core(&bundle).unwrap();
    }

    #[test]
    fn rejects_newer_schema_and_foreign_files() {
        let newer = serde_json::to_vec(&json!({
            "format": BACKUP_FORMAT, "schema_version": CURRENT_SCHEMA_VERSION + 1, "data": {}
        })).unwrap();
        assert!(parse_bundle(&newer).unwrap_err().contains("newer version"));

        let foreign = serde_json::to_vec(&json!({ "hello": "world" })).unwrap();
        assert!(parse_bundle(&foreign).is_err());
    }

    #[test]
    fn apply_replaces_never_merges() {
        // Source backup has ONLY a motor map + one dated snapshot.
        let src = temp_dir("src");
        write(&src, MOTOR_MAP_FILE, &motor_json());
        std::fs::create_dir_all(src.join(SNAPSHOTS_DIR)).unwrap();
        write(&src.join(SNAPSHOTS_DIR), "2026-06-28.json", &motor_json());
        let bundle = parse_bundle(&build_bundle(&src, "test", 1).unwrap()).unwrap();

        // Destination has DIFFERENT stale data the restore must not keep.
        let dst = temp_dir("dst");
        write(&dst, WORD_FREQ_FILE, &json!({ "version": 1, "counts": { "stale": 9.0 }, "last_update": 0 }));
        std::fs::create_dir_all(dst.join(SNAPSHOTS_DIR)).unwrap();
        write(&dst.join(SNAPSHOTS_DIR), "2025-01-01.json", &motor_json());

        apply_bundle(&bundle, &dst).unwrap();

        // The backup's data is present...
        assert!(dst.join(MOTOR_MAP_FILE).exists());
        assert!(dst.join(SNAPSHOTS_DIR).join("2026-06-28.json").exists());
        // ...and the stale destination data is gone (replace, never merge).
        assert!(!dst.join(WORD_FREQ_FILE).exists());
        assert!(!dst.join(SNAPSHOTS_DIR).join("2025-01-01.json").exists());
    }

    #[test]
    fn delete_clears_learned_and_diagnostics_but_not_allow_list() {
        let dir = temp_dir("del");
        write(&dir, MOTOR_MAP_FILE, &motor_json());
        write(&dir, GUESS_ACCURACY_FILE, &json!({ "version": 1 }));
        std::fs::write(dir.join(SHADOW_LOG_FILE), b"line\n").unwrap();
        write(&dir, "allow_list.json", &json!({ "version": 1, "correction_enabled": true, "patterns": [] }));

        delete_all_data(&dir).unwrap();

        assert!(!dir.join(MOTOR_MAP_FILE).exists());
        assert!(!dir.join(GUESS_ACCURACY_FILE).exists());
        assert!(!dir.join(SHADOW_LOG_FILE).exists());
        // allow_list is config — the engine resets it separately, not here.
        assert!(dir.join("allow_list.json").exists());
    }
}
