//! Per-hand timing aggregator.
//!
//! Rolls per-key/per-finger observations up to the two hands and measures the
//! imbalance between them — the same primitive shapes as `timing` (count,
//! mean dwell, mean inter-key interval), plus dwell/interval ratios and an
//! overall asymmetry score.
//!
//! Observe-only for this round, same contract as [`timing`](crate::timing):
//! Backspace events are not interval anchors (a long pause to self-correct
//! doesn't show up as a long interval on the next key), and keys outside the
//! US-QWERTY touch-typing map (`Escape`, `ArrowLeft`, …) don't contribute to
//! either hand.
//!
//! Why a separate aggregator instead of deriving from `TimingAggregator`'s
//! snapshot: this owns its own running sums, so snapshotting is O(1) per
//! event instead of O(unique-keys). Cheap enough to stay in the keystroke
//! latency budget as L2 grows.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use volatility_map::{finger_for, Hand};

use crate::events::InputEvent;
use crate::MAX_TYPING_INTERVAL_MS;

#[derive(Debug, Default)]
pub struct AsymmetryTracker {
    by_hand: HashMap<Hand, HandState>,
    /// Timestamp of the previous mapped Key event, used to attribute the
    /// inter-key interval to the *destination* hand. None until the first
    /// mapped key arrives.
    last_key_timestamp_ms: Option<u64>,
}

#[derive(Debug, Default, Clone, Copy)]
struct HandState {
    count: u64,
    dwell_sum_ms: u64,
    interval_sum_ms: u64,
    interval_count: u64,
}

impl AsymmetryTracker {
    pub fn observe(&mut self, event: &InputEvent) {
        let InputEvent::Key {
            key,
            timestamp_ms,
            dwell_ms,
            ..
        } = event
        else {
            return;
        };
        // Unmapped keys (function keys, arrows, …) don't belong to a hand —
        // they shouldn't anchor the interval either, otherwise an Escape
        // press in the middle of typing would zero out the next real
        // interval.
        let Some((hand, _)) = finger_for(key) else {
            return;
        };

        let entry = self.by_hand.entry(hand).or_default();
        entry.count += 1;
        entry.dwell_sum_ms += u64::from(*dwell_ms);
        if let Some(prev) = self.last_key_timestamp_ms {
            let gap = timestamp_ms.saturating_sub(prev);
            // Drop pause-length gaps (see crate::MAX_TYPING_INTERVAL_MS).
            // Anchor still advances below, so the next real interval is
            // measured cleanly.
            if gap <= MAX_TYPING_INTERVAL_MS {
                entry.interval_sum_ms += gap;
                entry.interval_count += 1;
            }
        }
        self.last_key_timestamp_ms = Some(*timestamp_ms);
    }

    pub fn snapshot(&self) -> AsymmetrySnapshot {
        let left = self.by_hand.get(&Hand::Left).copied().unwrap_or_default();
        let right = self.by_hand.get(&Hand::Right).copied().unwrap_or_default();

        let left_stats = HandStats {
            hand: Hand::Left,
            count: left.count,
            avg_dwell_ms: mean(left.dwell_sum_ms, left.count),
            avg_interval_ms: mean(left.interval_sum_ms, left.interval_count),
        };
        let right_stats = HandStats {
            hand: Hand::Right,
            count: right.count,
            avg_dwell_ms: mean(right.dwell_sum_ms, right.count),
            avg_interval_ms: mean(right.interval_sum_ms, right.interval_count),
        };

        // Slower-to-faster ratios. Always >= 1.0 when both sides have data;
        // 1.0 when either side has no samples so the panel renders neutrally
        // instead of NaN.
        let dwell_ratio = ratio(left_stats.avg_dwell_ms, right_stats.avg_dwell_ms);
        let interval_ratio = ratio(left_stats.avg_interval_ms, right_stats.avg_interval_ms);

        // Score is the **dwell** ratio. Dwell is the clean motor signal —
        // press-to-release on a single key — and isn't contaminated by
        // pauses or reading time the way interval can be even after gap
        // filtering. Interval is exposed in the snapshot for diagnostics
        // but deliberately doesn't drive the score.
        let overall_score = dwell_ratio;

        // "Steadier" decided on dwell alone, for the same reason. Only
        // meaningful when both hands have samples.
        let steadier_hand = if left_stats.count > 0 && right_stats.count > 0 {
            if left_stats.avg_dwell_ms < right_stats.avg_dwell_ms {
                Some(Hand::Left)
            } else if right_stats.avg_dwell_ms < left_stats.avg_dwell_ms {
                Some(Hand::Right)
            } else {
                None
            }
        } else {
            None
        };

        AsymmetrySnapshot {
            left: left_stats,
            right: right_stats,
            dwell_ratio,
            interval_ratio,
            overall_score,
            steadier_hand,
        }
    }
}

fn mean(sum: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        sum as f64 / count as f64
    }
}

/// Ratio of the larger to the smaller. Always >= 1.0 when both inputs are
/// positive. Falls back to 1.0 when either input is 0 so callers don't see
/// NaN or Inf before the second hand has typed anything.
fn ratio(a: f64, b: f64) -> f64 {
    if a <= 0.0 || b <= 0.0 {
        1.0
    } else {
        a.max(b) / a.min(b)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandStats {
    pub hand: Hand,
    pub count: u64,
    pub avg_dwell_ms: f64,
    pub avg_interval_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsymmetrySnapshot {
    pub left: HandStats,
    pub right: HandStats,
    /// Ratio of slower to faster avg dwell across the two hands.
    /// Always >= 1.0; equals 1.0 when both means are equal *or* when either
    /// side has no samples yet.
    pub dwell_ratio: f64,
    /// Same shape, for inter-key interval.
    pub interval_ratio: f64,
    /// Larger of `dwell_ratio` and `interval_ratio`. The single "how
    /// imbalanced is this user" number. 1.0 = perfectly symmetric.
    pub overall_score: f64,
    /// Hand with lower combined (avg_dwell + avg_interval). `None` when
    /// either side has no samples — we can't say which is steadier yet.
    pub steadier_hand: Option<Hand>,
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

    #[test]
    fn counts_dwell_and_interval_per_hand() {
        let mut agg = AsymmetryTracker::default();
        // Left-hand keys: 'a' (pinky), 's' (ring); right-hand: 'j', 'k'.
        agg.observe(&key("a", 100, 80)); // L, no interval (first)
        agg.observe(&key("j", 200, 90)); // R, interval 100
        agg.observe(&key("s", 350, 100)); // L, interval 150
        agg.observe(&key("k", 500, 70)); // R, interval 150

        let snap = agg.snapshot();
        assert_eq!(snap.left.count, 2);
        assert_eq!(snap.right.count, 2);
        // L dwell: (80+100)/2 = 90; R dwell: (90+70)/2 = 80
        assert!((snap.left.avg_dwell_ms - 90.0).abs() < 1e-9);
        assert!((snap.right.avg_dwell_ms - 80.0).abs() < 1e-9);
        // L intervals: only the s→… one (150); R intervals: 100 + 150 = 250 over 2 = 125
        assert!((snap.left.avg_interval_ms - 150.0).abs() < 1e-9);
        assert!((snap.right.avg_interval_ms - 125.0).abs() < 1e-9);
    }

    #[test]
    fn score_is_one_when_symmetric() {
        let mut agg = AsymmetryTracker::default();
        // Same dwell, same intervals on both hands.
        agg.observe(&key("a", 100, 80));
        agg.observe(&key("j", 200, 80)); // interval 100 → right
        agg.observe(&key("s", 300, 80)); // interval 100 → left
        agg.observe(&key("k", 400, 80)); // interval 100 → right
        let snap = agg.snapshot();
        assert!((snap.dwell_ratio - 1.0).abs() < 1e-9);
        assert!((snap.interval_ratio - 1.0).abs() < 1e-9);
        assert!((snap.overall_score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn score_is_the_dwell_ratio() {
        let mut agg = AsymmetryTracker::default();
        agg.observe(&key("a", 100, 60)); // L dwell 60
        agg.observe(&key("j", 200, 120)); // R dwell 120, interval 100
        let snap = agg.snapshot();
        // Dwell ratio = 120/60 = 2.0
        assert!((snap.dwell_ratio - 2.0).abs() < 1e-9);
        // Only right has an interval sample → interval_ratio falls back to 1.0
        assert!((snap.interval_ratio - 1.0).abs() < 1e-9);
        // Score follows dwell alone now (was max of the two).
        assert!((snap.overall_score - 2.0).abs() < 1e-9);
    }

    #[test]
    fn score_ignores_interval_imbalance() {
        // Dwell symmetric, interval lopsided — score should stay at 1.0
        // because interval doesn't drive it. Demonstrates the fix for the
        // pause-time bug that pulled the score (and steadier_hand) the
        // wrong way.
        let mut agg = AsymmetryTracker::default();
        agg.observe(&key("a", 0, 80)); // L
        agg.observe(&key("j", 100, 80)); // R, interval 100
        agg.observe(&key("s", 1_000, 80)); // L, interval 900 (a long-but-kept interval)
        agg.observe(&key("k", 1_100, 80)); // R, interval 100
        let snap = agg.snapshot();
        assert!((snap.dwell_ratio - 1.0).abs() < 1e-9);
        assert!(snap.interval_ratio > 1.0, "interval IS lopsided here");
        assert!((snap.overall_score - 1.0).abs() < 1e-9);
        // With equal dwell on both hands, no hand is steadier.
        assert_eq!(snap.steadier_hand, None);
    }

    #[test]
    fn steadier_hand_is_lower_avg_dwell() {
        // Left has higher dwell — right is steadier — even though interval
        // happens to favour left. Mirrors the real-world bug: pause-time
        // pollutes interval; dwell is the trustworthy motor signal.
        let mut agg = AsymmetryTracker::default();
        agg.observe(&key("a", 0, 120)); // L dwell 120
        agg.observe(&key("j", 100, 60)); // R dwell 60, interval 100
        agg.observe(&key("s", 200, 110)); // L dwell 110, interval 100
        agg.observe(&key("k", 300, 70)); // R dwell 70, interval 100
        let snap = agg.snapshot();
        assert_eq!(snap.steadier_hand, Some(Hand::Right));
    }

    #[test]
    fn pause_length_gaps_are_excluded_from_intervals() {
        let mut agg = AsymmetryTracker::default();
        agg.observe(&key("a", 100, 70)); // L
                                         // 5 second pause — should NOT contribute to right's avg interval.
        agg.observe(&key("j", 5_100, 70)); // R, gap = 5000ms → dropped
                                           // Fast follow-up: this 200ms gap IS counted for left.
        agg.observe(&key("s", 5_300, 70)); // L, gap = 200ms → kept
        let snap = agg.snapshot();
        // Right has 1 keystroke but 0 interval samples because the only gap
        // landing on R (5000ms) was a pause.
        assert_eq!(snap.right.count, 1);
        assert_eq!(snap.right.avg_interval_ms, 0.0);
        // Left's second occurrence kept the 200ms interval.
        assert!((snap.left.avg_interval_ms - 200.0).abs() < 1e-9);
    }

    #[test]
    fn steadier_hand_is_none_when_one_side_empty() {
        let mut agg = AsymmetryTracker::default();
        agg.observe(&key("a", 100, 80));
        agg.observe(&key("s", 200, 80));
        let snap = agg.snapshot();
        assert_eq!(snap.right.count, 0);
        // Can't say which is steadier without right-hand data.
        assert_eq!(snap.steadier_hand, None);
        // Both ratios default to 1.0 (neutral) rather than NaN/Inf.
        assert!((snap.dwell_ratio - 1.0).abs() < 1e-9);
        assert!((snap.interval_ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn non_key_events_are_ignored() {
        let mut agg = AsymmetryTracker::default();
        agg.observe(&InputEvent::Backspace { timestamp_ms: 50 });
        agg.observe(&InputEvent::Ready);
        let snap = agg.snapshot();
        assert_eq!(snap.left.count, 0);
        assert_eq!(snap.right.count, 0);
    }

    #[test]
    fn backspace_between_keys_does_not_anchor_interval() {
        let mut agg = AsymmetryTracker::default();
        agg.observe(&key("a", 100, 70)); // L
        agg.observe(&InputEvent::Backspace { timestamp_ms: 200 });
        agg.observe(&key("j", 400, 70)); // R, interval should be 400-100 = 300
        let snap = agg.snapshot();
        assert!((snap.right.avg_interval_ms - 300.0).abs() < 1e-9);
    }

    #[test]
    fn unmapped_keys_dont_anchor_interval() {
        // If Escape anchored the interval, the next real key would attribute
        // a too-small interval to its hand.
        let mut agg = AsymmetryTracker::default();
        agg.observe(&key("a", 100, 70)); // L
        agg.observe(&key("Escape", 200, 30)); // unmapped — ignored
        agg.observe(&key("j", 500, 70)); // R, interval = 500-100 = 400
        let snap = agg.snapshot();
        assert!((snap.right.avg_interval_ms - 400.0).abs() < 1e-9);
        // And Escape doesn't contribute to either hand.
        assert_eq!(snap.left.count, 1);
        assert_eq!(snap.right.count, 1);
    }
}
