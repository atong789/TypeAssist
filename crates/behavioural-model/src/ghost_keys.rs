//! Ghost-key detector.
//!
//! Tallies **candidate** ghost keystrokes from three signatures, none of
//! which are certainties — they are evidence the press may have been
//! unintended:
//!
//! 1. **Short dwell** — the key was barely pressed (a graze/brush).
//!    Threshold is **adaptive**: a fraction of the user's own running mean
//!    dwell, with an absolute floor. Calibrates to the typist instead of
//!    using a fixed ms value that would be wrong for fast or slow users.
//! 2. **Rapid same-key repeat** — the same key fired again within ~80ms,
//!    far below any natural double-letter rhythm (e.g. "ll", "tt"). Almost
//!    always a chatter / sticky-key event.
//! 3. **Key immediately followed by backspace** — the user noticed and
//!    corrected the press themselves, within a typing-window of ~1.2s.
//!    Self-corrections are signal, not failure (CLAUDE.md).
//!
//! Counted per key and rolled up per finger. The per-key count is at the
//! **event** level (a single press is at most one ghost). The per-signature
//! breakdown is at the **evidence** level (the same press can trip multiple
//! signatures and contribute to all of them), which is useful for
//! diagnostics and may exceed the event-level total.
//!
//! Observe-only for this round: no influence on the L4 correction decision.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use volatility_map::{anatomical_order, finger_for, Finger, Hand};

use crate::events::InputEvent;

/// Number of Key events to observe before the adaptive dwell threshold goes
/// live. Below this we don't trust the running mean enough to flag short
/// dwells.
const WARMUP_KEYS: u64 = 20;

/// Fraction of the running mean dwell below which a press is flagged. A
/// press whose dwell is less than `max(mean × this, MIN_DWELL_FLOOR_MS)` is
/// a candidate short-dwell ghost.
const SHORT_DWELL_FRACTION: f64 = 0.35;

/// Absolute lower bound on the dwell threshold (ms). Even for very
/// light-touch typists with a low mean, anything below 20 ms is
/// suspect — that's the bottom of physically realistic press durations.
const MIN_DWELL_FLOOR_MS: f64 = 20.0;

/// Gap (ms) below which two presses of the same key are treated as a
/// chatter / double-fire. Natural same-key sequences run >= 100 ms apart
/// even for fast typists.
const RAPID_REPEAT_MS: u64 = 80;

/// Window (ms) within which a Backspace following a Key counts as the user
/// self-correcting that key. Above this, the backspace is more likely an
/// edit of older text.
const SELF_CORRECT_WINDOW_MS: u64 = 1200;

#[derive(Debug, Default)]
pub struct GhostKeyTracker {
    /// Per-key event-level + per-signature counts.
    by_key: HashMap<String, KeyGhostCounts>,
    /// Per-signature totals across all keys.
    short_dwell_total: u64,
    rapid_repeat_total: u64,
    self_corrected_total: u64,
    /// Last *Key* event we saw (cleared on any Backspace).
    last_key: Option<LastKey>,
    /// Running stats for the adaptive dwell threshold. All keys contribute,
    /// including ones we flag — over time ghosts are a minority and won't
    /// drag the mean far.
    total_keys: u64,
    total_dwell_ms: u64,
}

#[derive(Debug, Default, Clone, Copy)]
struct KeyGhostCounts {
    event_count: u64,
    short_dwell: u64,
    rapid_repeat: u64,
    self_corrected: u64,
}

#[derive(Debug, Clone)]
struct LastKey {
    key: String,
    timestamp_ms: u64,
    /// Have we already incremented event_count for this press? Stops the
    /// follow-up self-correct signature from double-counting at event level
    /// when short-dwell or rapid-repeat already fired on the same press.
    already_counted: bool,
}

impl GhostKeyTracker {
    pub fn observe(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Key {
                key,
                timestamp_ms,
                dwell_ms,
                ..
            } => self.observe_key(key, *timestamp_ms, *dwell_ms),
            InputEvent::Backspace { timestamp_ms } => self.observe_backspace(*timestamp_ms),
            _ => {}
        }
    }

    fn observe_key(&mut self, key: &str, timestamp_ms: u64, dwell_ms: u32) {
        // Update warmup stats up-front so the threshold this press is
        // compared against reflects everything we know so far (including
        // this press itself — order doesn't matter materially, and this
        // way the very first post-warmup press has a non-degenerate mean).
        self.total_keys += 1;
        self.total_dwell_ms += u64::from(dwell_ms);

        let mut short = false;
        let mut repeat = false;

        // Adaptive short-dwell check — only after enough samples to trust
        // the mean.
        if self.total_keys > WARMUP_KEYS {
            let mean = self.total_dwell_ms as f64 / self.total_keys as f64;
            let threshold = (mean * SHORT_DWELL_FRACTION).max(MIN_DWELL_FLOOR_MS);
            if (dwell_ms as f64) < threshold {
                short = true;
            }
        }

        // Rapid same-key repeat — compare against the previous Key.
        if let Some(last) = &self.last_key {
            if last.key == key && timestamp_ms.saturating_sub(last.timestamp_ms) < RAPID_REPEAT_MS {
                repeat = true;
            }
        }

        let any = short || repeat;
        if any {
            let entry = self.by_key.entry(key.to_string()).or_default();
            entry.event_count += 1;
            if short {
                entry.short_dwell += 1;
                self.short_dwell_total += 1;
            }
            if repeat {
                entry.rapid_repeat += 1;
                self.rapid_repeat_total += 1;
            }
        }

        self.last_key = Some(LastKey {
            key: key.to_string(),
            timestamp_ms,
            already_counted: any,
        });
    }

    fn observe_backspace(&mut self, timestamp_ms: u64) {
        // A Backspace clears the last_key slot either way — even if it's
        // outside the self-correct window, the user has moved on from
        // single-press follow-ups.
        if let Some(last) = self.last_key.take() {
            if timestamp_ms.saturating_sub(last.timestamp_ms) < SELF_CORRECT_WINDOW_MS {
                let entry = self.by_key.entry(last.key).or_default();
                entry.self_corrected += 1;
                self.self_corrected_total += 1;
                // Event-level count only ticks if this press wasn't already
                // flagged by short_dwell / rapid_repeat — otherwise the same
                // event would be counted twice.
                if !last.already_counted {
                    entry.event_count += 1;
                }
            }
        }
    }

    pub fn snapshot(&self) -> GhostKeysSnapshot {
        let total_ghost_events: u64 = self.by_key.values().map(|c| c.event_count).sum();

        let mut per_key: Vec<KeyGhostRow> = self
            .by_key
            .iter()
            .filter(|(_, c)| c.event_count > 0)
            .map(|(key, c)| {
                let (hand, finger) = match finger_for(key) {
                    Some((h, f)) => (Some(h), Some(f)),
                    None => (None, None),
                };
                KeyGhostRow {
                    key: key.clone(),
                    hand,
                    finger,
                    events: c.event_count,
                    short_dwell: c.short_dwell,
                    rapid_repeat: c.rapid_repeat,
                    self_corrected: c.self_corrected,
                }
            })
            .collect();
        // Worst offenders first, ties broken by key for stable ordering.
        per_key.sort_by(|a, b| b.events.cmp(&a.events).then_with(|| a.key.cmp(&b.key)));

        // Roll up per hand × per signature, so a lopsided per-finger row can
        // be debugged ("is it short-dwell that's biased, or repeats?").
        // Always emit BOTH hands so the L vs R comparison shows even when
        // one side has zero data — that itself is informative.
        let mut left_acc = HandGhostRow::empty(Hand::Left);
        let mut right_acc = HandGhostRow::empty(Hand::Right);
        for row in &per_key {
            let target = match row.hand {
                Some(Hand::Left) => &mut left_acc,
                Some(Hand::Right) => &mut right_acc,
                None => continue, // unmapped keys (Escape, etc.) — no hand
            };
            target.events += row.events;
            target.short_dwell += row.short_dwell;
            target.rapid_repeat += row.rapid_repeat;
            target.self_corrected += row.self_corrected;
        }
        let per_hand = vec![left_acc, right_acc];

        // Roll up per finger from the per-key data. Keys without a mapped
        // finger don't contribute.
        let mut by_finger: HashMap<(Hand, Finger), u64> = HashMap::new();
        for row in &per_key {
            if let (Some(h), Some(f)) = (row.hand, row.finger) {
                *by_finger.entry((h, f)).or_default() += row.events;
            }
        }
        let mut per_finger: Vec<FingerGhostRow> = by_finger
            .into_iter()
            .map(|((hand, finger), events)| FingerGhostRow {
                hand,
                finger,
                events,
            })
            .collect();
        per_finger.sort_by_key(|r| anatomical_order(r.hand, r.finger));

        // Show the threshold currently in effect so the panel can be
        // transparent about what's being flagged. 0.0 while warming up.
        let (dwell_threshold_ms, warmup_remaining) = if self.total_keys > WARMUP_KEYS {
            let mean = self.total_dwell_ms as f64 / self.total_keys as f64;
            ((mean * SHORT_DWELL_FRACTION).max(MIN_DWELL_FLOOR_MS), 0)
        } else {
            (0.0, WARMUP_KEYS.saturating_sub(self.total_keys) + 1)
        };

        GhostKeysSnapshot {
            total_ghost_events,
            short_dwell_count: self.short_dwell_total,
            rapid_repeat_count: self.rapid_repeat_total,
            self_corrected_count: self.self_corrected_total,
            per_hand,
            per_key,
            per_finger,
            dwell_threshold_ms,
            warmup_remaining,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhostKeysSnapshot {
    /// Distinct presses flagged as likely ghosts (event-level).
    pub total_ghost_events: u64,
    /// Per-signature evidence counts. Their sum may exceed
    /// `total_ghost_events` — one press can trip more than one signature.
    pub short_dwell_count: u64,
    pub rapid_repeat_count: u64,
    pub self_corrected_count: u64,
    /// Per-hand × per-signature breakdown. Always exactly two rows in
    /// `[Left, Right]` order — both present (zeroed if needed) so the L vs
    /// R comparison is visible even when one side hasn't typed anything.
    /// Useful for debugging an unexpected per-finger imbalance: it reveals
    /// which *signature* is driving the gap.
    pub per_hand: Vec<HandGhostRow>,
    /// Per-key rows, worst offenders first. Only keys with at least one
    /// flagged event appear.
    pub per_key: Vec<KeyGhostRow>,
    /// Per-finger rollup of ghost events, anatomical order.
    pub per_finger: Vec<FingerGhostRow>,
    /// Adaptive dwell cutoff (ms) currently in effect — anything below this
    /// is flagged as short-dwell. `0.0` while still warming up.
    pub dwell_threshold_ms: f64,
    /// Number of keystrokes remaining before the dwell threshold goes live.
    /// `0` once active.
    pub warmup_remaining: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyGhostRow {
    pub key: String,
    pub hand: Option<Hand>,
    pub finger: Option<Finger>,
    pub events: u64,
    pub short_dwell: u64,
    pub rapid_repeat: u64,
    pub self_corrected: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerGhostRow {
    pub hand: Hand,
    pub finger: Finger,
    pub events: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandGhostRow {
    pub hand: Hand,
    pub events: u64,
    pub short_dwell: u64,
    pub rapid_repeat: u64,
    pub self_corrected: u64,
}

impl HandGhostRow {
    fn empty(hand: Hand) -> Self {
        Self {
            hand,
            events: 0,
            short_dwell: 0,
            rapid_repeat: 0,
            self_corrected: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Modifiers;

    fn key(k: &str, ts: u64, dwell: u32) -> InputEvent {
        InputEvent::Key {
            key: k.into(),
            timestamp_ms: ts,
            modifiers: Modifiers::default(),
            dwell_ms: dwell,
        }
    }

    /// Feed `n` normal-dwell keys to get past warmup so subsequent presses
    /// are tested against a live threshold. Returns the next timestamp to
    /// use.
    fn warmup(agg: &mut GhostKeyTracker) -> u64 {
        let mut ts = 0u64;
        for i in 0..30 {
            // Vary keys so we don't trip rapid-repeat during warmup.
            let k = if i % 2 == 0 { "f" } else { "j" };
            agg.observe(&key(k, ts, 100));
            ts += 200;
        }
        ts
    }

    #[test]
    fn no_short_dwell_flagged_during_warmup() {
        let mut agg = GhostKeyTracker::default();
        // First press dwell 5ms — would be ultra-short, but we're still in
        // warmup so the threshold isn't live yet.
        agg.observe(&key("a", 0, 5));
        let snap = agg.snapshot();
        assert_eq!(snap.total_ghost_events, 0);
        assert_eq!(snap.short_dwell_count, 0);
        assert!(snap.warmup_remaining > 0);
        assert_eq!(snap.dwell_threshold_ms, 0.0);
    }

    #[test]
    fn flags_short_dwell_after_warmup() {
        let mut agg = GhostKeyTracker::default();
        let mut ts = warmup(&mut agg);
        // Adaptive threshold = max(100 * 0.35, 20) = 35 ms.
        // Dwell 10ms is well under → flagged.
        ts += 200;
        agg.observe(&key("g", ts, 10));
        let snap = agg.snapshot();
        assert_eq!(snap.short_dwell_count, 1);
        assert_eq!(snap.total_ghost_events, 1);
        let g = snap.per_key.iter().find(|r| r.key == "g").unwrap();
        assert_eq!(g.events, 1);
        assert_eq!(g.short_dwell, 1);
        // Threshold should be reported and >= the absolute floor.
        assert!(snap.dwell_threshold_ms >= MIN_DWELL_FLOOR_MS);
    }

    #[test]
    fn flags_rapid_same_key_repeat() {
        let mut agg = GhostKeyTracker::default();
        // Same-key chatter doesn't need warmup — the signature is "press
        // again within 80ms", a physical not statistical thing.
        agg.observe(&key("l", 0, 100));
        agg.observe(&key("l", 30, 100)); // 30ms gap → repeat
        let snap = agg.snapshot();
        assert_eq!(snap.rapid_repeat_count, 1);
        assert_eq!(snap.total_ghost_events, 1);
    }

    #[test]
    fn natural_double_letter_is_not_a_repeat() {
        // Normal "ll" in "hello" — typical gap > 100ms — must NOT trip.
        let mut agg = GhostKeyTracker::default();
        agg.observe(&key("l", 0, 100));
        agg.observe(&key("l", 150, 100));
        let snap = agg.snapshot();
        assert_eq!(snap.rapid_repeat_count, 0);
        assert_eq!(snap.total_ghost_events, 0);
    }

    #[test]
    fn flags_key_then_backspace_as_self_correct() {
        let mut agg = GhostKeyTracker::default();
        agg.observe(&key("p", 0, 100));
        agg.observe(&InputEvent::Backspace { timestamp_ms: 250 });
        let snap = agg.snapshot();
        assert_eq!(snap.self_corrected_count, 1);
        assert_eq!(snap.total_ghost_events, 1);
        let p = snap.per_key.iter().find(|r| r.key == "p").unwrap();
        assert_eq!(p.self_corrected, 1);
    }

    #[test]
    fn late_backspace_is_not_self_correct() {
        // Backspace far after the press — likely editing older text, not
        // catching a ghost.
        let mut agg = GhostKeyTracker::default();
        agg.observe(&key("p", 0, 100));
        agg.observe(&InputEvent::Backspace {
            timestamp_ms: SELF_CORRECT_WINDOW_MS + 100,
        });
        let snap = agg.snapshot();
        assert_eq!(snap.self_corrected_count, 0);
        assert_eq!(snap.total_ghost_events, 0);
    }

    #[test]
    fn overlapping_signatures_count_once_at_event_level() {
        // A press that's both short-dwell AND followed by a backspace
        // should count as one EVENT but two SIGNATURES.
        let mut agg = GhostKeyTracker::default();
        let mut ts = warmup(&mut agg);
        ts += 200;
        agg.observe(&key("v", ts, 8)); // short dwell → flagged
        ts += 100;
        agg.observe(&InputEvent::Backspace { timestamp_ms: ts });
        let snap = agg.snapshot();
        assert_eq!(snap.total_ghost_events, 1, "one EVENT");
        assert_eq!(snap.short_dwell_count, 1);
        assert_eq!(snap.self_corrected_count, 1);
    }

    #[test]
    fn per_finger_rollup_uses_anatomical_order_and_skips_unmapped() {
        let mut agg = GhostKeyTracker::default();
        let mut ts = warmup(&mut agg);

        // Three flagged keys: 'q' (L pinky), 'j' (R index), 'Escape' (unmapped).
        for k in ["q", "j", "Escape"] {
            ts += 200;
            agg.observe(&key(k, ts, 5)); // all ultra-short → all flagged
        }
        let snap = agg.snapshot();
        assert_eq!(snap.total_ghost_events, 3);

        // Per-finger rollup includes only mapped keys, in anatomical order.
        let order: Vec<(Hand, Finger)> = snap
            .per_finger
            .iter()
            .map(|r| (r.hand, r.finger))
            .collect();
        assert_eq!(
            order,
            vec![(Hand::Left, Finger::Pinky), (Hand::Right, Finger::Index)]
        );

        // The unmapped key still appears in the per-key list (we counted it),
        // just with no hand/finger attribution.
        let esc = snap.per_key.iter().find(|r| r.key == "Escape").unwrap();
        assert_eq!(esc.hand, None);
        assert_eq!(esc.finger, None);
    }

    #[test]
    fn backspace_clears_last_key_so_no_repeat_after_correction() {
        // Type 'q', backspace, type 'q' a few ms later — the second 'q' is
        // an intentional re-try, not a chatter repeat.
        let mut agg = GhostKeyTracker::default();
        agg.observe(&key("q", 0, 100));
        agg.observe(&InputEvent::Backspace { timestamp_ms: 100 });
        agg.observe(&key("q", 150, 100));
        let snap = agg.snapshot();
        assert_eq!(snap.rapid_repeat_count, 0);
        // self-correct fired for the FIRST q (1 event).
        assert_eq!(snap.self_corrected_count, 1);
        assert_eq!(snap.total_ghost_events, 1);
    }

    #[test]
    fn per_hand_rollup_sums_signatures_correctly() {
        let mut agg = GhostKeyTracker::default();
        let mut ts = warmup(&mut agg);

        // Left-hand ghosts: short-dwell 'q', short-dwell 'q' (same key),
        // then 'a' followed by backspace (self-corrected).
        ts += 200;
        agg.observe(&key("q", ts, 5));
        ts += 200;
        agg.observe(&key("q", ts, 5));
        ts += 200;
        agg.observe(&key("a", ts, 100));
        ts += 100;
        agg.observe(&InputEvent::Backspace { timestamp_ms: ts });

        // Right-hand ghosts: a rapid same-key repeat on 'l'.
        ts += 200;
        agg.observe(&key("l", ts, 100));
        ts += 30; // < RAPID_REPEAT_MS → repeat
        agg.observe(&key("l", ts, 100));

        let snap = agg.snapshot();
        assert_eq!(snap.per_hand.len(), 2);
        let left = &snap.per_hand[0];
        let right = &snap.per_hand[1];
        assert_eq!(left.hand, Hand::Left);
        assert_eq!(right.hand, Hand::Right);

        // Left: 2 short-dwell ('q', 'q') + 1 self-corrected ('a') = 3 events.
        assert_eq!(left.events, 3);
        assert_eq!(left.short_dwell, 2);
        assert_eq!(left.rapid_repeat, 0);
        assert_eq!(left.self_corrected, 1);

        // Right: 1 rapid-repeat ('l' second press) = 1 event.
        assert_eq!(right.events, 1);
        assert_eq!(right.short_dwell, 0);
        assert_eq!(right.rapid_repeat, 1);
        assert_eq!(right.self_corrected, 0);
    }

    #[test]
    fn per_hand_always_emits_both_rows_even_with_one_side_empty() {
        let mut agg = GhostKeyTracker::default();
        let mut ts = warmup(&mut agg);
        // Only right-hand ghost.
        ts += 200;
        agg.observe(&key("k", ts, 5));

        let snap = agg.snapshot();
        assert_eq!(snap.per_hand.len(), 2);
        // Left side is present with zeros — the L vs R comparison is the
        // data, an absent row would hide that the left was clean.
        assert_eq!(snap.per_hand[0].hand, Hand::Left);
        assert_eq!(snap.per_hand[0].events, 0);
        assert_eq!(snap.per_hand[0].short_dwell, 0);
        assert_eq!(snap.per_hand[1].hand, Hand::Right);
        assert_eq!(snap.per_hand[1].events, 1);
        assert_eq!(snap.per_hand[1].short_dwell, 1);
    }

    #[test]
    fn per_hand_excludes_unmapped_keys() {
        let mut agg = GhostKeyTracker::default();
        let mut ts = warmup(&mut agg);
        // 'Escape' is unmapped → counted in per_key/totals but not in
        // either hand's rollup.
        ts += 200;
        agg.observe(&key("Escape", ts, 5));

        let snap = agg.snapshot();
        assert_eq!(snap.total_ghost_events, 1);
        assert_eq!(snap.per_hand[0].events, 0);
        assert_eq!(snap.per_hand[1].events, 0);
        // But the press IS visible in the per-key table.
        assert!(snap.per_key.iter().any(|r| r.key == "Escape"));
    }

    #[test]
    fn no_panic_on_non_key_non_backspace_events() {
        // Sidecar lifecycle events must be ignored without state changes.
        let mut agg = GhostKeyTracker::default();
        agg.observe(&InputEvent::Ready);
        agg.observe(&InputEvent::PermissionRequired);
        agg.observe(&InputEvent::Shutdown);
        let snap = agg.snapshot();
        assert_eq!(snap.total_ghost_events, 0);
    }
}
