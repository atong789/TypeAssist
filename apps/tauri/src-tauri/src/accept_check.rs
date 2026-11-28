//! Engine side of the **accept check**: what the sidecar needs to verify a word's
//! on-screen length before deleting it, the counters that reconcile every request
//! with its answer (Principle #7), and the surface-verdict rule Early space-drift
//! gates on. The measuring itself is L1 (`AcceptGuard.swift`). Counts only.

use std::collections::BTreeMap;

use behavioural_model::events::{InjectStatus, InputEvent};

/// How long to wait for the sidecar's answer before giving a request up.
pub(crate) const INJECT_RESULT_TIMEOUT_MS: u64 = 3_000;

/// `(expected, alternate)` on-screen lengths from the word's start — just after
/// the last whitespace — to the caret at rest.
///
/// `expected`: the field still holds what was typed. `alternate`: the host already
/// rewrote the word to the target, `len_change` characters longer or shorter.
pub(crate) fn segment_lengths(
    line: &[char],
    word_start: usize,
    word_end: usize,
    outer_trail_len: usize,
    len_change: isize,
) -> (u32, u32) {
    let chunk_start = line[..word_start.min(line.len())]
        .iter()
        .rposition(|c| c.is_whitespace())
        .map_or(0, |i| i + 1);
    // word + closing punctuation + the sealing boundary
    let expected = (word_end + outer_trail_len + 1).saturating_sub(chunk_start);
    let alternate = (expected as isize + len_change).max(0) as usize;
    (expected as u32, alternate as u32)
}

/// The surface verdict after one L1 event. Only a `WebHost` report changes it:
/// the sidecar reports the surface BEFORE `Ready`, and re-sends only on a change,
/// so clearing it on `Ready` left it unknown until the user visited a browser.
/// (A respawn clears it at the spawn site.)
pub(crate) fn surface_after(current: Option<bool>, event: &InputEvent) -> Option<bool> {
    match event {
        InputEvent::WebHost { web } => Some(*web),
        _ => current,
    }
}

/// `requested = applied + adjusted + unverified + Σ aborted + pending`.
#[derive(Debug, Default)]
pub(crate) struct AcceptLedger {
    requested: u64,
    applied: u64,
    adjusted: u64,
    unverified: u64,
    aborted: BTreeMap<String, u64>,
}

impl AcceptLedger {
    pub fn note_requested(&mut self) {
        self.requested += 1;
    }

    pub fn note_result(&mut self, status: InjectStatus, reason: Option<&str>) {
        match status {
            InjectStatus::Applied => self.applied += 1,
            InjectStatus::Adjusted => self.adjusted += 1,
            InjectStatus::Unverified => self.unverified += 1,
            InjectStatus::Aborted => {
                *self
                    .aborted
                    .entry(reason.unwrap_or("unknown").to_string())
                    .or_default() += 1;
            }
        }
    }

    /// `Some(description)` when the counters don't reconcile.
    pub fn mismatch(&self, pending: bool) -> Option<String> {
        let aborted: u64 = self.aborted.values().sum();
        let answered = self.applied + self.adjusted + self.unverified + aborted;
        (self.requested != answered + u64::from(pending)).then(|| {
            format!(
                "requested={} answered={answered} pending={pending}",
                self.requested
            )
        })
    }

    pub fn dump(&self, pending: bool) {
        tracing::info!(
            "INJECT_DUMP requested={} applied={} adjusted={} unverified={} aborted={:?}",
            self.requested,
            self.applied,
            self.adjusted,
            self.unverified,
            self.aborted
        );
        if let Some(d) = self.mismatch(pending) {
            tracing::warn!("INJECT_INTEGRITY_BROKEN {d}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn the_surface_report_survives_the_ready_that_follows_it() {
        // The sidecar's real startup order.
        let mut surface = None;
        for e in [InputEvent::WebHost { web: false }, InputEvent::Ready] {
            surface = surface_after(surface, &e);
        }
        assert_eq!(surface, Some(false));
    }

    #[test]
    fn segment_runs_from_the_last_whitespace_to_the_caret() {
        // "No tthink " — the right word of a re-split, and the one-word case.
        assert_eq!(segment_lengths(&chars("No tthink "), 3, 9, 0, -1), (7, 6));
        // Leading punctuation belongs to the on-screen segment.
        assert_eq!(segment_lengths(&chars("a (teh "), 3, 6, 0, 0), (5, 5));
        // Start of line, trailing quote, longer target.
        assert_eq!(segment_lengths(&chars("wrd\" "), 0, 3, 1, 1), (5, 6));
    }

    #[test]
    fn ledger_reconciles_every_request_with_an_answer() {
        let mut l = AcceptLedger::default();
        for s in [
            InjectStatus::Applied,
            InjectStatus::Adjusted,
            InjectStatus::Aborted,
        ] {
            l.note_requested();
            assert!(l.mismatch(true).is_none());
            l.note_result(s, Some("length_changed"));
            assert!(l.mismatch(false).is_none());
        }
        l.note_requested();
        assert!(
            l.mismatch(false).is_some(),
            "an unanswered request is a gap"
        );
    }
}
