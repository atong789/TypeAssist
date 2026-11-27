//! **Early space-drift ledger (Build 3a)** — the engine-side owner of the two things
//! the drift check needs beside the pure detector in
//! `correction_engine::space_drift`:
//!
//! 1. **The per-user record** ([`DriftStats`]: shown / taken / ignored — counts
//!    only, no words, no timing) that moves the detector's bar, persisted as
//!    `space_drift.json`. The engine task is its sole writer (Principle #6/#8).
//! 2. **The reconciliation counters** (Principle #7). Every pass through the
//!    detector is counted: `checked = offered + gate_off + Σ rejected`, and every
//!    offer ends exactly once: `offered = taken + ignored + (one still pending)`.
//!    A gap logs `DRIFT_INTEGRITY_BROKEN` — a silent drop is not allowed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use correction_engine::{
    load_space_drift_stats, save_space_drift_stats, DriftBar, DriftReject, DriftStats,
    SPACE_DRIFT_FILE,
};

#[derive(Debug, Default)]
pub struct DriftLedger {
    path: Option<PathBuf>,
    stats: DriftStats,
    // ---- session counters (reconciled; not persisted) ----
    checked: u64,
    rejected: BTreeMap<&'static str, u64>,
    gate_off: u64,
    offered: u64,
    taken: u64,
    ignored: u64,
    undone: u64,
    save_failures: u64,
}

impl DriftLedger {
    /// Open against a data dir (`None` = disabled: counts still reconcile in memory
    /// but nothing touches disk — tests and other callers using `Default`).
    pub fn open(dir: Option<PathBuf>) -> Self {
        let path = dir.map(|d| d.join(SPACE_DRIFT_FILE));
        let stats = path
            .as_deref()
            .map(load_space_drift_stats)
            .unwrap_or_default();
        Self {
            path,
            stats,
            ..Self::default()
        }
    }

    /// The bar for the next offer, from this user's own record.
    pub fn bar(&self) -> DriftBar {
        self.stats.bar()
    }

    #[cfg(test)]
    pub fn stats(&self) -> DriftStats {
        self.stats
    }

    /// Re-read the record from disk (after a backup restore wrote it). A true
    /// replace — the session reconcile counters are left alone.
    pub fn reload_record(&mut self) {
        self.stats = self
            .path
            .as_deref()
            .map(load_space_drift_stats)
            .unwrap_or_default();
    }

    /// The detector ran and declined, for a named reason.
    pub fn note_rejected(&mut self, r: DriftReject) {
        self.checked += 1;
        *self.rejected.entry(r.tag()).or_insert(0) += 1;
    }

    /// The detector found a drift but the Corrections master gate is off, so no
    /// bubble is shown.
    pub fn note_gate_off(&mut self) {
        self.checked += 1;
        self.gate_off += 1;
    }

    /// An offer was put on screen.
    pub fn note_offered(&mut self) {
        self.checked += 1;
        self.offered += 1;
        self.stats.shown += 1;
        self.persist();
    }

    /// The user accepted the offer.
    pub fn note_taken(&mut self) {
        self.taken += 1;
        self.stats.taken += 1;
        self.persist();
    }

    /// The offer ended without an accept.
    pub fn note_ignored(&mut self) {
        self.ignored += 1;
        self.stats.ignored += 1;
        self.persist();
    }

    /// The user took the fix and then reverted it with Esc. The session `taken`
    /// count is untouched (the accept happened); the *record* — the signal for the
    /// bar — moves it from taken to ignored, because the user did not want it.
    pub fn note_undone(&mut self) {
        self.undone += 1;
        self.stats.taken = self.stats.taken.saturating_sub(1);
        self.stats.ignored += 1;
        self.persist();
    }

    /// Forget everything — Delete everything. Removes the file.
    pub fn reset(&mut self) {
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
        let path = self.path.take();
        *self = Self {
            path,
            ..Self::default()
        };
    }

    fn persist(&mut self) {
        let Some(p) = &self.path else { return };
        if let Err(e) = save_space_drift_stats(p, &self.stats) {
            self.save_failures += 1;
            tracing::warn!("DRIFT_STATS_SAVE_FAILED path={:?} err={}", p, e);
        }
    }

    /// `Some(description)` when the counters don't reconcile. `pending` = whether an
    /// offer is on screen right now (decided by neither accept nor ignore yet).
    pub fn mismatch(&self, pending: bool) -> Option<String> {
        let rejected: u64 = self.rejected.values().sum();
        let stage1 = self.checked == self.offered + self.gate_off + rejected;
        let stage2 = self.offered == self.taken + self.ignored + u64::from(pending);
        (!(stage1 && stage2)).then(|| {
            format!(
                "checked={} offered={} gate_off={} rejected={} taken={} ignored={} pending={}",
                self.checked,
                self.offered,
                self.gate_off,
                rejected,
                self.taken,
                self.ignored,
                pending
            )
        })
    }

    /// Reconcile + log at the funnel-dump cadence (content-free counts).
    pub fn dump(&self, pending: bool) {
        tracing::info!(
            "DRIFT_DUMP checked={} offered={} taken={} ignored={} undone={} gate_off={} rejected={:?} record={:?} save_failures={}",
            self.checked,
            self.offered,
            self.taken,
            self.ignored,
            self.undone,
            self.gate_off,
            self.rejected,
            self.stats,
            self.save_failures
        );
        if let Some(d) = self.mismatch(pending) {
            tracing::warn!("DRIFT_INTEGRITY_BROKEN {d}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "tcd-driftledger-{name}-{:?}",
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn counters_reconcile_through_a_full_life() {
        let mut l = DriftLedger::default();
        l.note_rejected(DriftReject::LeftGain);
        l.note_gate_off();
        l.note_offered();
        assert!(l.mismatch(true).is_none(), "one offer pending reconciles");
        assert!(
            l.mismatch(false).is_some(),
            "a pending offer unaccounted for is a gap"
        );
        l.note_taken();
        assert!(l.mismatch(false).is_none());
        l.note_offered();
        l.note_ignored();
        assert!(l.mismatch(false).is_none());
    }

    #[test]
    fn a_lost_decision_is_a_visible_gap() {
        let mut l = DriftLedger::default();
        l.note_offered();
        // The offer ended but nothing recorded how — the reconcile must catch it.
        assert!(l.mismatch(false).is_some());
    }

    #[test]
    fn the_record_persists_and_moves_the_bar() {
        let d = tmp("persist");
        let mut l = DriftLedger::open(Some(d.clone()));
        assert_eq!(
            l.bar(),
            DriftBar::Ratio(correction_engine::space_drift::BAR_START)
        );
        for _ in 0..5 {
            l.note_offered();
            l.note_taken();
        }
        assert_eq!(
            l.bar(),
            DriftBar::Ratio(correction_engine::space_drift::BAR_FLOOR)
        );
        // Survives a restart.
        let again = DriftLedger::open(Some(d.clone()));
        assert_eq!(again.stats(), l.stats());
        assert_eq!(again.bar(), l.bar());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn undo_moves_the_record_from_taken_to_ignored() {
        let mut l = DriftLedger::default();
        l.note_offered();
        l.note_taken();
        l.note_undone();
        assert_eq!(l.stats().taken, 0);
        assert_eq!(l.stats().ignored, 1);
        assert_eq!(l.taken, 1, "the session accept still happened");
        assert!(l.mismatch(false).is_none());
    }

    #[test]
    fn reset_removes_the_file_and_the_record() {
        let d = tmp("reset");
        let mut l = DriftLedger::open(Some(d.clone()));
        l.note_offered();
        l.note_ignored();
        assert!(d.join(SPACE_DRIFT_FILE).exists());
        l.reset();
        assert!(!d.join(SPACE_DRIFT_FILE).exists());
        assert_eq!(l.stats(), DriftStats::default());
        let _ = std::fs::remove_dir_all(&d);
    }
}
