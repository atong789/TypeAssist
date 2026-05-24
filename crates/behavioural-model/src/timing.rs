//! Per-key timing aggregator, with finger-aware rollup.
//!
//! Per-key: count, mean dwell, mean inter-key interval. Each row is tagged
//! with the touch-typing finger/hand for the key (via L3's `finger_for`) so
//! the debug view can also show a per-finger gradient. "Interval" is the gap
//! from the previous Key event to this one — Backspace events are not
//! anchors, so a long pause to correct doesn't show up as a long interval on
//! the *next* key.
//!
//! Only `timing` has real logic for now; the other four aggregators are
//! no-ops until their round lands.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use volatility_map::{finger_for, Finger, Hand};

use crate::events::InputEvent;

#[derive(Debug, Default)]
pub struct TimingAggregator {
    keys: HashMap<String, KeyTiming>,
    /// Timestamp of the previous Key event we observed, used to compute the
    /// inter-key interval for the next one. None until the first Key arrives.
    last_key_timestamp_ms: Option<u64>,
}

#[derive(Debug, Default, Clone, Copy)]
struct KeyTiming {
    count: u64,
    dwell_sum_ms: u64,
    /// Sum of intervals attributed to *this* key (i.e. measured from the
    /// previous Key event to this Key event). `interval_count` is separate
    /// from `count` because the very first occurrence of a key has no prior
    /// Key event to measure against.
    interval_sum_ms: u64,
    interval_count: u64,
}

impl TimingAggregator {
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

        let entry = self.keys.entry(key.clone()).or_default();
        entry.count += 1;
        entry.dwell_sum_ms += u64::from(*dwell_ms);

        if let Some(prev) = self.last_key_timestamp_ms {
            // Guard against out-of-order or duplicate timestamps from the
            // sidecar — saturating_sub keeps the sum monotonic instead of
            // wrapping to a huge value.
            entry.interval_sum_ms += timestamp_ms.saturating_sub(prev);
            entry.interval_count += 1;
        }
        self.last_key_timestamp_ms = Some(*timestamp_ms);
    }

    pub fn snapshot(&self) -> TimingSnapshot {
        TimingSnapshot {
            per_key: self.per_key_snapshot(),
            per_finger: self.per_finger_snapshot(),
        }
    }

    fn per_key_snapshot(&self) -> Vec<KeyTimingSnapshot> {
        let mut rows: Vec<KeyTimingSnapshot> = self
            .keys
            .iter()
            .map(|(key, k)| {
                let (hand, finger) = match finger_for(key) {
                    Some((h, f)) => (Some(h), Some(f)),
                    None => (None, None),
                };
                KeyTimingSnapshot {
                    key: key.clone(),
                    count: k.count,
                    avg_dwell_ms: mean(k.dwell_sum_ms, k.count),
                    avg_interval_ms: mean(k.interval_sum_ms, k.interval_count),
                    hand,
                    finger,
                }
            })
            .collect();
        // Stable ordering for the debug view: most-typed keys first, ties
        // broken by key so the table doesn't reshuffle on every refresh.
        rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
        rows
    }

    fn per_finger_snapshot(&self) -> Vec<FingerTimingSnapshot> {
        // (count, dwell_sum, interval_sum, interval_count) grouped per finger.
        let mut by_finger: HashMap<(Hand, Finger), (u64, u64, u64, u64)> = HashMap::new();
        for (key, k) in &self.keys {
            let Some((hand, finger)) = finger_for(key) else {
                continue;
            };
            let entry = by_finger.entry((hand, finger)).or_default();
            entry.0 += k.count;
            entry.1 += k.dwell_sum_ms;
            entry.2 += k.interval_sum_ms;
            entry.3 += k.interval_count;
        }
        let mut rows: Vec<FingerTimingSnapshot> = by_finger
            .into_iter()
            .map(
                |((hand, finger), (count, dwell_sum, interval_sum, interval_count))| {
                    FingerTimingSnapshot {
                        hand,
                        finger,
                        total_count: count,
                        avg_dwell_ms: mean(dwell_sum, count),
                        avg_interval_ms: mean(interval_sum, interval_count),
                    }
                },
            )
            .collect();
        // Anatomical order left-pinky → left-thumb → right-thumb → right-pinky
        // so the per-finger gradient reads left-to-right like a keyboard.
        rows.sort_by_key(|r| anatomical_order(r.hand, r.finger));
        rows
    }
}

fn mean(sum: u64, count: u64) -> f64 {
    if count == 0 {
        0.0
    } else {
        sum as f64 / count as f64
    }
}

fn anatomical_order(hand: Hand, finger: Finger) -> u8 {
    match (hand, finger) {
        (Hand::Left, Finger::Pinky) => 0,
        (Hand::Left, Finger::Ring) => 1,
        (Hand::Left, Finger::Middle) => 2,
        (Hand::Left, Finger::Index) => 3,
        (Hand::Left, Finger::Thumb) => 4,
        (Hand::Right, Finger::Thumb) => 5,
        (Hand::Right, Finger::Index) => 6,
        (Hand::Right, Finger::Middle) => 7,
        (Hand::Right, Finger::Ring) => 8,
        (Hand::Right, Finger::Pinky) => 9,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingSnapshot {
    pub per_key: Vec<KeyTimingSnapshot>,
    pub per_finger: Vec<FingerTimingSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyTimingSnapshot {
    pub key: String,
    pub count: u64,
    pub avg_dwell_ms: f64,
    pub avg_interval_ms: f64,
    /// `None` for keys outside the US-QWERTY touch-typing map
    /// (function keys, arrows, sidecar control chars).
    pub hand: Option<Hand>,
    pub finger: Option<Finger>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FingerTimingSnapshot {
    pub hand: Hand,
    pub finger: Finger,
    pub total_count: u64,
    pub avg_dwell_ms: f64,
    pub avg_interval_ms: f64,
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
    fn counts_and_averages_per_key() {
        let mut agg = TimingAggregator::default();
        agg.observe(&key("a", 100, 80));
        agg.observe(&key("b", 250, 90)); // interval 150 attributed to "b"
        agg.observe(&key("a", 400, 100)); // interval 150 attributed to "a"
        agg.observe(&key("a", 500, 60)); // interval 100 attributed to "a"

        let snap = agg.snapshot();
        let a = snap.per_key.iter().find(|r| r.key == "a").unwrap();
        assert_eq!(a.count, 3);
        assert!((a.avg_dwell_ms - 80.0).abs() < 1e-9);
        // Two intervals for "a": 150 + 100 = 250 → mean 125.
        assert!((a.avg_interval_ms - 125.0).abs() < 1e-9);
        assert_eq!(a.hand, Some(Hand::Left));
        assert_eq!(a.finger, Some(Finger::Pinky));

        let b = snap.per_key.iter().find(|r| r.key == "b").unwrap();
        assert_eq!(b.count, 1);
        assert_eq!(b.hand, Some(Hand::Left));
        assert_eq!(b.finger, Some(Finger::Index));
    }

    #[test]
    fn per_finger_rollup_weights_by_count() {
        let mut agg = TimingAggregator::default();
        // Both "j" and "k" map to right hand; j=index, k=middle.
        agg.observe(&key("j", 100, 60)); // right index — no interval (first key)
        agg.observe(&key("j", 200, 80)); // right index — interval 100
        agg.observe(&key("k", 350, 100)); // right middle — interval 150

        let snap = agg.snapshot();

        let idx = snap
            .per_finger
            .iter()
            .find(|r| r.hand == Hand::Right && r.finger == Finger::Index)
            .unwrap();
        assert_eq!(idx.total_count, 2);
        // (60 + 80) / 2 = 70
        assert!((idx.avg_dwell_ms - 70.0).abs() < 1e-9);
        // One interval attributed to "j" (100); mean = 100
        assert!((idx.avg_interval_ms - 100.0).abs() < 1e-9);

        let mid = snap
            .per_finger
            .iter()
            .find(|r| r.hand == Hand::Right && r.finger == Finger::Middle)
            .unwrap();
        assert_eq!(mid.total_count, 1);
        assert!((mid.avg_dwell_ms - 100.0).abs() < 1e-9);
        assert!((mid.avg_interval_ms - 150.0).abs() < 1e-9);
    }

    #[test]
    fn per_finger_order_is_anatomical() {
        let mut agg = TimingAggregator::default();
        // Type one key per finger across both hands, in jumbled order.
        let keys = ["p", "a", "j", " ", "f", "k", "l", ";", "q", "s"];
        for (i, k) in keys.iter().enumerate() {
            agg.observe(&key(k, 100 * (i as u64 + 1), 70));
        }
        let snap = agg.snapshot();
        let order: Vec<(Hand, Finger)> = snap
            .per_finger
            .iter()
            .map(|r| (r.hand, r.finger))
            .collect();
        // Left-pinky → left-thumb → right-thumb → right-pinky.
        // (No left-thumb key in this set, so it just isn't present.)
        assert_eq!(
            order,
            vec![
                (Hand::Left, Finger::Pinky),
                (Hand::Left, Finger::Ring),
                (Hand::Left, Finger::Index),
                (Hand::Right, Finger::Thumb),
                (Hand::Right, Finger::Index),
                (Hand::Right, Finger::Middle),
                (Hand::Right, Finger::Ring),
                (Hand::Right, Finger::Pinky),
            ]
        );
    }

    #[test]
    fn first_key_has_no_interval_sample() {
        let mut agg = TimingAggregator::default();
        agg.observe(&key("z", 100, 50));
        let snap = agg.snapshot();
        let z = snap.per_key.iter().find(|r| r.key == "z").unwrap();
        assert_eq!(z.count, 1);
        assert_eq!(z.avg_interval_ms, 0.0);
    }

    #[test]
    fn non_key_events_are_ignored() {
        let mut agg = TimingAggregator::default();
        agg.observe(&InputEvent::Backspace { timestamp_ms: 50 });
        agg.observe(&InputEvent::Ready);
        let snap = agg.snapshot();
        assert!(snap.per_key.is_empty());
        assert!(snap.per_finger.is_empty());
    }

    #[test]
    fn backspace_between_keys_does_not_anchor_interval() {
        let mut agg = TimingAggregator::default();
        agg.observe(&key("a", 100, 70));
        agg.observe(&InputEvent::Backspace { timestamp_ms: 200 });
        agg.observe(&key("b", 400, 70)); // interval should still be 400-100 = 300
        let snap = agg.snapshot();
        let b = snap.per_key.iter().find(|r| r.key == "b").unwrap();
        assert!((b.avg_interval_ms - 300.0).abs() < 1e-9);
    }

    #[test]
    fn keys_outside_us_qwerty_map_have_no_finger() {
        let mut agg = TimingAggregator::default();
        agg.observe(&key("Escape", 100, 30));
        let snap = agg.snapshot();
        let row = snap.per_key.iter().find(|r| r.key == "Escape").unwrap();
        assert_eq!(row.hand, None);
        assert_eq!(row.finger, None);
        // And the per-finger rollup is empty (no rows contributed to any finger).
        assert!(snap.per_finger.is_empty());
    }
}
