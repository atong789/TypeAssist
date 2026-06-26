//! Correction allow-list (M3 correction Step 1) — the **manual** opt-in set
//! that gates live correction.
//!
//! This is the source of truth for *what the engine is allowed to correct* and
//! the global *master gate* (`correction_enabled`). It is deliberately separate
//! from the observe-only learning machinery:
//!
//! * [`crate::engine`]'s `WordPatternStore` / `kill_switch` *learn and classify*
//!   `typed → target` patterns automatically (the future automatic heuristic).
//!   Step 1 does **not** use that to gate corrections.
//! * This allow-list is **manually curated** — nothing is enabled by default,
//!   the user opts each pattern in one at a time, and the master gate ships
//!   **off**. Empty list OR gate off ⇒ the engine corrects nothing.
//!
//! ## Ownership & persistence
//!
//! The **engine task is the sole writer** (`~/.typeassist/allow_list.json`),
//! mutating its in-memory copy in response to `EngineControl` messages and the
//! Escape-undo teach-stop, then flushing atomically. The UI reads the file
//! read-only for display. One writer avoids a two-writer race on recovery-data
//! files (Principle #6/#8). `TYPEASSIST_DATA_DIR` redirects the file for dev
//! safety, exactly like the motor map / word patterns.
//!
//! ## Matching
//!
//! Patterns are stored normalized lowercase (`teh → the`). A typed word matches
//! case-insensitively. Step 1 replaces with the stored target verbatim (so a
//! capitalised `Teh` corrects to lowercase `the`); casing preservation is a
//! deliberate later refinement, not needed for the plumbing pattern.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// On-disk shape version. Bump alongside any change to [`AllowList`] /
/// [`AllowPattern`] so a loader can refuse or migrate a stale file.
pub const ALLOW_LIST_VERSION: u32 = 1;

/// One manually-enabled correction. Both fields are normalized lowercase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowPattern {
    /// The word as the user mistypes it, e.g. `teh`.
    pub typed: String,
    /// The word to replace it with, e.g. `the`.
    pub target: String,
}

/// The manual correction allow-list + the global master gate.
///
/// Defaults are the safe-dark state the feature ships in: gate **off**, **no**
/// patterns — so a fresh install (or a missing file) corrects nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowList {
    /// Shape version (see [`ALLOW_LIST_VERSION`]).
    pub version: u32,
    /// **Master gate.** While `false`, the engine corrects nothing regardless
    /// of the pattern list — the instant global off. Default `false`.
    pub correction_enabled: bool,
    /// The enabled patterns. Empty ⇒ no corrections even when the gate is on.
    pub patterns: Vec<AllowPattern>,
}

impl Default for AllowList {
    fn default() -> Self {
        Self {
            version: ALLOW_LIST_VERSION,
            correction_enabled: false,
            patterns: Vec::new(),
        }
    }
}

impl AllowList {
    pub fn new() -> Self {
        Self::default()
    }

    /// The target this typed word should be corrected to, **iff** the master
    /// gate is on and `typed` matches an enabled pattern (case-insensitive).
    ///
    /// NOTE: unused since the bubble fires from the engine's classifier/bold lane
    /// rather than the manual allow-list (whose per-pattern enable was retired —
    /// it's always empty). Only `correction_enabled` (the master gate) is still
    /// read. Retained with `enable`/`disable` for the planned ignore-based
    /// back-off; remove the whole `patterns` mechanism if that doesn't land.
    #[allow(dead_code)]
    pub fn target_for(&self, typed: &str) -> Option<&str> {
        if !self.correction_enabled {
            return None;
        }
        let typed = normalize_word(typed);
        if typed.is_empty() {
            return None;
        }
        self.patterns
            .iter()
            .find(|p| p.typed == typed)
            .map(|p| p.target.as_str())
    }

    /// Set the master gate. Returns `true` if the value changed.
    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        let changed = self.correction_enabled != enabled;
        self.correction_enabled = enabled;
        changed
    }

    /// Enable a pattern (idempotent — re-enabling updates the target). A
    /// no-op-after-normalize pair (empty, or `typed == target`) is rejected so
    /// the list can't hold a pattern that would never fire or would loop.
    /// Returns `true` if the list changed.
    ///
    /// NOTE: the manual per-pattern "enable" UI was removed in the frontend
    /// redesign — the engine now decides what to suggest and `disable` is the
    /// teach-stop. This is kept (and exercised by tests) for the M3 §5 bubble
    /// pass, which re-wires the fire decision onto the classifier.
    #[allow(dead_code)]
    pub fn enable(&mut self, typed: &str, target: &str) -> bool {
        let typed = normalize_word(typed);
        let target = normalize_word(target);
        if typed.is_empty() || target.is_empty() || typed == target {
            return false;
        }
        if let Some(p) = self.patterns.iter_mut().find(|p| p.typed == typed) {
            if p.target == target {
                return false;
            }
            p.target = target;
            return true;
        }
        self.patterns.push(AllowPattern { typed, target });
        true
    }

    /// Disable (remove) a pattern by its typed form. Returns `true` if a pattern
    /// was removed.
    ///
    /// NOTE: currently unused — the Escape-undo teach-stop was dropped (undo now
    /// only reverts; v42). Retained (and tested) for the planned ignore-based
    /// back-off (a pattern the user keeps ignoring stops being suggested), which
    /// is firing-logic work outside the bubble task. Mirrors [`enable`].
    #[allow(dead_code)]
    pub fn disable(&mut self, typed: &str) -> bool {
        let typed = normalize_word(typed);
        let before = self.patterns.len();
        self.patterns.retain(|p| p.typed != typed);
        self.patterns.len() != before
    }

    // --- Persistence (atomic, mirrors WordPatternStore) ---------------------

    /// Load from `path`. A missing file is **not** an error — it's the
    /// shipped-dark default (gate off, no patterns). A **corrupt** file is
    /// quarantined aside to `<stem>.corrupt-<ts>.json` (never wiped — the
    /// curated list is preserved for recovery) and the safe-dark default is
    /// returned, rather than erroring; a warning is logged so the reset is
    /// visible.
    pub fn load_from(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(al) => Ok(al),
                Err(e) => {
                    correction_engine::persist::quarantine_corrupt(path, e);
                    Ok(Self::default())
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Serialize to `path` durably and atomically (temp → fsync → rename →
    /// fsync dir), so a reader never sees a half-written file and a crash can't
    /// leave a zero-length or torn allow-list.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        correction_engine::persist::durable_write(path, json.as_bytes())
    }
}

/// Lowercase + keep only printable ASCII — the same normalization the word
/// pattern store uses, so an allow-list entry matches a sealed token's core the
/// same way the learner folds case.
fn normalize_word(s: &str) -> String {
    s.chars()
        .filter_map(|c| {
            let c = c.to_ascii_lowercase();
            c.is_ascii_graphic().then_some(c)
        })
        .collect()
}

// Sanity at compile time.
const _: () = assert!(ALLOW_LIST_VERSION >= 1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_shipped_dark() {
        let al = AllowList::new();
        assert!(!al.correction_enabled);
        assert!(al.patterns.is_empty());
        // Gate off ⇒ no correction even if we ask about nothing.
        assert_eq!(al.target_for("teh"), None);
    }

    #[test]
    fn gate_off_suppresses_an_enabled_pattern() {
        let mut al = AllowList::new();
        al.enable("teh", "the");
        // Still off by default.
        assert_eq!(al.target_for("teh"), None);
        al.set_enabled(true);
        assert_eq!(al.target_for("teh"), Some("the"));
    }

    #[test]
    fn matching_is_case_insensitive_and_normalized() {
        let mut al = AllowList::new();
        al.set_enabled(true);
        al.enable("teh", "the");
        assert_eq!(al.target_for("Teh"), Some("the"));
        assert_eq!(al.target_for("TEH"), Some("the"));
        assert_eq!(al.target_for("the"), None);
    }

    #[test]
    fn enable_rejects_noop_and_empty_pairs() {
        let mut al = AllowList::new();
        assert!(!al.enable("the", "the"), "self-map must be rejected");
        assert!(!al.enable("", "the"));
        assert!(!al.enable("teh", ""));
        assert!(al.patterns.is_empty());
    }

    #[test]
    fn enable_is_idempotent_but_updates_target() {
        let mut al = AllowList::new();
        assert!(al.enable("teh", "the"));
        assert!(!al.enable("teh", "the"), "same pair → no change");
        assert!(al.enable("teh", "ten"), "new target → change");
        assert_eq!(al.patterns.len(), 1);
    }

    #[test]
    fn disable_removes_a_pattern() {
        let mut al = AllowList::new();
        al.set_enabled(true);
        al.enable("teh", "the");
        assert!(al.disable("Teh"), "disable normalizes the key");
        assert_eq!(al.target_for("teh"), None);
        assert!(!al.disable("teh"), "second disable is a no-op");
    }

    #[test]
    fn set_enabled_reports_change() {
        let mut al = AllowList::new();
        assert!(al.set_enabled(true));
        assert!(!al.set_enabled(true), "no change on same value");
        assert!(al.set_enabled(false));
    }

    #[test]
    fn missing_file_loads_as_default() {
        let path =
            std::env::temp_dir().join(format!("ta_allow_missing_{}.json", std::process::id()));
        let _ = fs::remove_file(&path);
        let al = AllowList::load_from(&path).unwrap();
        assert!(!al.correction_enabled);
        assert!(al.patterns.is_empty());
    }

    #[test]
    fn save_and_load_round_trips() {
        let path = std::env::temp_dir().join(format!("ta_allow_rt_{}.json", std::process::id()));
        let mut al = AllowList::new();
        al.set_enabled(true);
        al.enable("teh", "the");
        al.enable("adn", "and");
        al.save_to(&path).unwrap();

        let loaded = AllowList::load_from(&path).unwrap();
        assert_eq!(loaded.version, ALLOW_LIST_VERSION);
        assert!(loaded.correction_enabled);
        assert_eq!(loaded.patterns.len(), 2);
        assert_eq!(loaded.target_for("teh"), Some("the"));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn corrupt_file_is_quarantined_and_loads_default() {
        // A corrupt allow-list must NOT error (which would block startup) and
        // must NOT silently wipe the user's curated list: it's moved aside and
        // the safe-dark default (gate off, no patterns) is returned.
        let path =
            std::env::temp_dir().join(format!("ta_allow_corrupt_{}.json", std::process::id()));
        fs::write(&path, b"}{ not valid allow-list json").unwrap();

        let al = AllowList::load_from(&path).expect("corrupt load must not error");
        assert!(!al.correction_enabled, "comes up shipped-dark");
        assert!(al.patterns.is_empty());

        assert!(!path.exists(), "corrupt file moved aside, not wiped");
        let dir = path.parent().unwrap();
        let quarantined: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with(&format!("ta_allow_corrupt_{}", std::process::id()))
                    && n.contains(".corrupt-")
            })
            .collect();
        assert_eq!(
            quarantined.len(),
            1,
            "exactly one quarantine file preserved"
        );
        let _ = fs::remove_file(quarantined[0].path());
    }
}
