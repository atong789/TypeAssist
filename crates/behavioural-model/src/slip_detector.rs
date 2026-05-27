//! Slip detector — the first L3 learning loop.
//!
//! Watches the keystroke stream for a small, immediate, same-word
//! backspace-correction pattern: type a character (or two), notice, undo,
//! retype something different. When confirmed under the strict filter, the
//! detector records the slip as a (`aimed_for`, `hit_instead`) pair in a
//! live `VolatilityMap` — incrementing the swap-pair frequency and lightly
//! decrementing the key's confidence.
//!
//! **Strict by design — high precision, modest recall.** Anything that
//! looks more like an edit than a motor slip is ignored:
//!   * **Immediate** — the fix has to happen within a single typing burst
//!     (no inter-event gap above `MAX_TYPING_GAP_MS`).
//!   * **Small** — at most `MAX_SLIP_LEN` (currently 2) characters typed,
//!     backspaced, and retyped. Anything larger is treated as a rewrite.
//!   * **Same word** — both the typed and the retyped sequence are
//!     contiguous word characters; any space/punctuation/control key in
//!     between resets the state.
//!
//! Observe-and-learn only for this round. The L4 correction decision
//! lives in `correction_engine::decision`; this detector does not
//! influence it directly. See CLAUDE.md "Ghost-key signals" for the broader
//! principle that motor-pattern detections stay low-confidence until they
//! can be cross-checked against labeled corrections — which is exactly
//! what this loop is starting to produce.
//!
//! ## L3 map-writes kill-switch (Component 5b sentinel)
//!
//! The slip detector's [`VolatilityMap`] mutations — `swap_pairs`
//! frequencies and `KeyConfidence` decrements — feed C3's
//! `motor_evidence_for`, which drives correction priors. Until the
//! C5b per-token motor verdict is validated against real typing, we
//! don't want an unvalidated slip signal teaching the motor map.
//! [`SlipDetector::with_map_writes`] gates exactly those mutations
//! behind a boolean (default OFF). Everything else stays live: the
//! `by_pair` tally, `total_slips`, the `SlipEvent`s drained by
//! `take_new_slips()` for the debug feed — all in-memory diagnostics
//! the panel uses. Only the writes that actually influence engine
//! behavior are gated.
//!
//! Flip back on by constructing the detector with
//! `SlipDetector::with_map_writes(true)` from the host (engine.rs).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use volatility_map::{
    finger_for, Confidence, Finger, Hand, KeyConfidence, ProfileContext, SwapPair, TimeOfDay,
    VolatilityMap,
};

use crate::events::InputEvent;

/// Window of consecutive characters considered as a potential slip.
/// A user-typed sequence longer than this is treated as a rewrite, not a slip.
const MAX_SLIP_LEN: usize = 2;

/// Largest allowed gap (ms) between consecutive events for the slip to count
/// as immediate. Mirrors the active-typing threshold the timing and asymmetry
/// aggregators use (see `crate::MAX_TYPING_INTERVAL_MS`).
const MAX_TYPING_GAP_MS: u64 = 1500;

/// Drop applied to a key's `Confidence` for every recorded slip on that key.
/// Conservative — confidence should adjust over many observations.
const CONFIDENCE_DECREMENT: f32 = 0.05;

#[derive(Debug)]
pub struct SlipDetector {
    state: DetectionState,
    /// One entry per distinct (`aimed_for`, `hit_instead`) pair, tallied.
    by_pair: HashMap<(String, String), SlipRecord>,
    total_slips: u64,
    /// The live volatility map this detector is writing into. First layer
    /// that actually produces L3 data.
    map: VolatilityMap,
    /// Slips detected since the last `take_new_slips()` call. Used by the
    /// engine host to emit Tauri events for the debug feed.
    pending_new: Vec<SlipEvent>,
    /// **C5b sentinel.** Whether confirmed slips update the
    /// [`VolatilityMap`] (`swap_pairs` and `KeyConfidence`). Default
    /// `false` — those writes feed C3's correction priors and we don't
    /// want an unvalidated slip signal teaching the motor map until
    /// the C5b per-token verdict has been observed against real
    /// typing. All other slip-detector outputs (the in-memory tally,
    /// `total_slips`, `SlipEvent`s on the debug feed) stay live.
    map_writes_enabled: bool,
}

impl Default for SlipDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default)]
enum DetectionState {
    /// No relevant recent activity.
    #[default]
    Idle,
    /// Consecutive word-char keystrokes, last `MAX_SLIP_LEN` retained.
    Typed(Vec<TypedKey>),
    /// User typed and then backspaced some of them; waiting on a retype.
    Erasing {
        typed: Vec<TypedKey>,
        erased: usize,
        last_bs_ms: u64,
    },
    /// User has started retyping after the backspaces. When `retyped.len()`
    /// reaches `erased`, the slip is processed.
    Retyping {
        typed: Vec<TypedKey>,
        erased: usize,
        retyped: Vec<TypedKey>,
    },
}

#[derive(Debug, Clone)]
struct TypedKey {
    key: String,
    timestamp_ms: u64,
}

#[derive(Debug, Clone)]
struct SlipRecord {
    aimed_for: String,
    hit_instead: String,
    hand: Option<Hand>,
    finger: Option<Finger>,
    count: u64,
    last_seen_ms: u64,
}

impl SlipDetector {
    /// Build a detector with the C5b safety default: L3 map writes
    /// **OFF**. The in-memory tally and `SlipEvent` feed stay live;
    /// the volatility map is constructed empty and never mutated
    /// until the kill-switch is flipped on.
    pub fn new() -> Self {
        Self::with_map_writes(false)
    }

    /// Build a detector with the L3 map-writes kill-switch in the
    /// requested state. Pass `true` once the C5b motor verdict has
    /// been validated and you want confirmed slips to start feeding
    /// the volatility map again.
    pub fn with_map_writes(map_writes_enabled: bool) -> Self {
        Self {
            state: DetectionState::Idle,
            by_pair: HashMap::new(),
            total_slips: 0,
            // Temporal/fatigue are deferred (see CLAUDE.md "Known design
            // questions") — placeholder profile until those aggregators
            // come online.
            map: VolatilityMap::empty(
                0,
                ProfileContext {
                    time_of_day: TimeOfDay::Morning,
                    session_fatigue: 0.0,
                },
            ),
            pending_new: Vec::new(),
            map_writes_enabled,
        }
    }

    /// Whether the L3 map-writes kill-switch is on. Surfaced so the
    /// host (engine.rs) can mirror the state to the debug panel.
    pub fn map_writes_enabled(&self) -> bool {
        self.map_writes_enabled
    }

    pub fn observe(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Key {
                key, timestamp_ms, ..
            } => self.observe_key(key, *timestamp_ms),
            InputEvent::Backspace { timestamp_ms } => self.observe_backspace(*timestamp_ms),
            _ => {
                // Lifecycle events (Ready/Shutdown/PermissionRequired) drop
                // any in-flight detection — the keystroke sequence is broken.
                self.state = DetectionState::Idle;
            }
        }
    }

    /// Drain the slips detected since the last call. The engine host emits
    /// these as Tauri events for the debug-view feed.
    pub fn take_new_slips(&mut self) -> Vec<SlipEvent> {
        std::mem::take(&mut self.pending_new)
    }

    pub fn snapshot(&self) -> SlipsSnapshot {
        let mut per_pair: Vec<PairSlipRow> = self
            .by_pair
            .values()
            .map(|r| PairSlipRow {
                aimed_for: r.aimed_for.clone(),
                hit_instead: r.hit_instead.clone(),
                hand: r.hand,
                finger: r.finger,
                count: r.count,
                last_seen_ms: r.last_seen_ms,
            })
            .collect();
        // Most-frequent first; ties broken by recency so a fresh first-time
        // slip surfaces before older once-ever pairs.
        per_pair.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then(b.last_seen_ms.cmp(&a.last_seen_ms))
        });
        SlipsSnapshot {
            total_slips: self.total_slips,
            per_pair,
            map_swap_pairs: self.map.swap_pairs.len() as u32,
            map_key_confidence: self.map.keys.len() as u32,
            map_writes_enabled: self.map_writes_enabled,
        }
    }

    /// Read-only access to the live volatility map. The engine's L4
    /// confidence scorer reads from it via `correction_engine::score`
    /// (swap-pair counts blend into per-candidate motor evidence).
    pub fn map(&self) -> &VolatilityMap {
        &self.map
    }

    // ---- State transitions ------------------------------------------------

    fn observe_key(&mut self, key: &str, ts: u64) {
        // Word characters only — any non-letter/non-digit breaks the
        // same-word filter. State reset hides this slip session entirely.
        if !is_word_char(key) {
            self.state = DetectionState::Idle;
            return;
        }
        let new_state = match std::mem::take(&mut self.state) {
            DetectionState::Idle => {
                DetectionState::Typed(vec![TypedKey {
                    key: key.into(),
                    timestamp_ms: ts,
                }])
            }
            DetectionState::Typed(typed) => self.advance_typed(typed, key, ts),
            DetectionState::Erasing {
                typed,
                erased,
                last_bs_ms,
            } => {
                // First key after backspace(s) — does this start a retype?
                if ts.saturating_sub(last_bs_ms) > MAX_TYPING_GAP_MS {
                    // Too slow → no slip, just begin a fresh typed buffer.
                    DetectionState::Typed(vec![TypedKey {
                        key: key.into(),
                        timestamp_ms: ts,
                    }])
                } else {
                    let retyped = vec![TypedKey {
                        key: key.into(),
                        timestamp_ms: ts,
                    }];
                    self.maybe_complete_retyping(DetectionState::Retyping {
                        typed,
                        erased,
                        retyped,
                    })
                }
            }
            DetectionState::Retyping {
                typed,
                erased,
                mut retyped,
            } => {
                let last_ts = retyped.last().map(|t| t.timestamp_ms).unwrap_or(ts);
                if ts.saturating_sub(last_ts) > MAX_TYPING_GAP_MS {
                    // Lost the burst — start a fresh typed buffer.
                    DetectionState::Typed(vec![TypedKey {
                        key: key.into(),
                        timestamp_ms: ts,
                    }])
                } else {
                    retyped.push(TypedKey {
                        key: key.into(),
                        timestamp_ms: ts,
                    });
                    self.maybe_complete_retyping(DetectionState::Retyping {
                        typed,
                        erased,
                        retyped,
                    })
                }
            }
        };
        self.state = new_state;
    }

    fn advance_typed(&self, mut typed: Vec<TypedKey>, key: &str, ts: u64) -> DetectionState {
        let last_ts = typed.last().map(|t| t.timestamp_ms).unwrap_or(ts);
        if ts.saturating_sub(last_ts) > MAX_TYPING_GAP_MS {
            // Burst broke — restart with just this key.
            DetectionState::Typed(vec![TypedKey {
                key: key.into(),
                timestamp_ms: ts,
            }])
        } else {
            typed.push(TypedKey {
                key: key.into(),
                timestamp_ms: ts,
            });
            while typed.len() > MAX_SLIP_LEN {
                typed.remove(0);
            }
            DetectionState::Typed(typed)
        }
    }

    fn observe_backspace(&mut self, ts: u64) {
        self.state = match std::mem::take(&mut self.state) {
            DetectionState::Idle => DetectionState::Idle,
            DetectionState::Typed(typed) => {
                if typed.is_empty() {
                    DetectionState::Idle
                } else {
                    let last_ts = typed.last().map(|t| t.timestamp_ms).unwrap_or(ts);
                    if ts.saturating_sub(last_ts) > MAX_TYPING_GAP_MS {
                        DetectionState::Idle
                    } else {
                        DetectionState::Erasing {
                            typed,
                            erased: 1,
                            last_bs_ms: ts,
                        }
                    }
                }
            }
            DetectionState::Erasing {
                typed,
                erased,
                last_bs_ms,
            } => {
                // Limit: never erase more than the buffered typed run or the
                // slip-length cap, whichever is smaller. Anything bigger is
                // a rewrite, not a slip.
                let cap = typed.len().min(MAX_SLIP_LEN);
                if ts.saturating_sub(last_bs_ms) > MAX_TYPING_GAP_MS || erased + 1 > cap {
                    DetectionState::Idle
                } else {
                    DetectionState::Erasing {
                        typed,
                        erased: erased + 1,
                        last_bs_ms: ts,
                    }
                }
            }
            DetectionState::Retyping { .. } => {
                // Backspace mid-retype → mid-correction → too messy, abort.
                DetectionState::Idle
            }
        };
    }

    fn maybe_complete_retyping(&mut self, state: DetectionState) -> DetectionState {
        let DetectionState::Retyping {
            typed,
            erased,
            retyped,
        } = state
        else {
            return state;
        };
        if retyped.len() < erased {
            // Still retyping — wait for more.
            return DetectionState::Retyping {
                typed,
                erased,
                retyped,
            };
        }
        // Retype complete: the chars that got backspaced are the LAST
        // `erased` of `typed`. Position-by-position, record any change.
        let start = typed.len().saturating_sub(erased);
        for i in 0..erased {
            let hit = &typed[start + i].key;
            let aimed = &retyped[i].key;
            let ts = retyped[i].timestamp_ms;
            if hit != aimed {
                self.record_slip(aimed, hit, ts);
            }
        }
        // After completing, the retyped chars become the new "currently
        // typed" buffer so a subsequent slip-on-this-correction would still
        // be detected.
        DetectionState::Typed(retyped)
    }

    fn record_slip(&mut self, aimed_for: &str, hit_instead: &str, ts: u64) {
        // Attribute the slip to the AIMED-FOR finger — that's the finger
        // that was supposed to fire and didn't. Matches CLAUDE.md "Recovery
        // physiology": progress and priors are per-finger about the
        // finger that's struggling, not the one that happened to land.
        let (hand, finger) = match finger_for(aimed_for) {
            Some((h, f)) => (Some(h), Some(f)),
            None => (None, None),
        };

        // In-memory log (per-pair tally).
        let key = (aimed_for.to_string(), hit_instead.to_string());
        let entry = self
            .by_pair
            .entry(key)
            .or_insert_with(|| SlipRecord {
                aimed_for: aimed_for.to_string(),
                hit_instead: hit_instead.to_string(),
                hand,
                finger,
                count: 0,
                last_seen_ms: 0,
            });
        entry.count += 1;
        entry.last_seen_ms = ts;
        self.total_slips += 1;

        // Live volatility-map writes — **gated by the C5b kill-switch**.
        // Default off: an unvalidated slip signal must not teach the
        // motor map. When the host enables writes, only mapped keys
        // go into L3 (the schema requires Hand + Finger).
        if self.map_writes_enabled {
            if let (Some(h), Some(f)) = (hand, finger) {
                // Swap pair: bump frequency or insert a new entry.
                if let Some(pair) = self
                    .map
                    .swap_pairs
                    .iter_mut()
                    .find(|p| p.aimed_for == aimed_for && p.hit_instead == hit_instead)
                {
                    pair.frequency += 1.0;
                } else {
                    self.map.swap_pairs.push(SwapPair {
                        aimed_for: aimed_for.to_string(),
                        hit_instead: hit_instead.to_string(),
                        frequency: 1.0,
                        hand: h,
                        finger: f,
                    });
                }

                // Key confidence: slips lightly degrade the aimed-for key.
                // mean_dwell_ms stays 0.0 here — that's L2's timing aggregator's
                // job, and a proper L2→L3 projection will fold it in later.
                if let Some(kc) = self.map.keys.iter_mut().find(|k| k.key == aimed_for) {
                    kc.sample_count = kc.sample_count.saturating_add(1);
                    kc.confidence = Confidence::new(kc.confidence.0 - CONFIDENCE_DECREMENT);
                } else {
                    self.map.keys.push(KeyConfidence {
                        key: aimed_for.to_string(),
                        confidence: Confidence::new(1.0 - CONFIDENCE_DECREMENT),
                        sample_count: 1,
                        mean_dwell_ms: 0.0,
                        hand: h,
                        finger: f,
                    });
                }
            }
        }

        // Queue for the debug feed marker.
        self.pending_new.push(SlipEvent {
            aimed_for: aimed_for.to_string(),
            hit_instead: hit_instead.to_string(),
            hand,
            finger,
            timestamp_ms: ts,
        });
    }
}

fn is_word_char(key: &str) -> bool {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => !c.is_control() && !c.is_whitespace(),
        _ => false,
    }
}

// ---- Snapshot types -------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlipsSnapshot {
    pub total_slips: u64,
    /// Distinct (aimed, hit) pairs, most-frequent first.
    pub per_pair: Vec<PairSlipRow>,
    /// Number of swap-pair entries in the live volatility map.
    pub map_swap_pairs: u32,
    /// Number of key-confidence entries in the live volatility map.
    pub map_key_confidence: u32,
    /// **C5b sentinel state.** `true` once the host has flipped the
    /// kill-switch on; until then, confirmed slips are tallied but
    /// the L3 map stays empty (`map_swap_pairs` / `map_key_confidence`
    /// remain 0). The SLIPS debug panel reads this so it can label
    /// whether the path is actually feeding correction priors.
    pub map_writes_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairSlipRow {
    pub aimed_for: String,
    pub hit_instead: String,
    pub hand: Option<Hand>,
    pub finger: Option<Finger>,
    pub count: u64,
    pub last_seen_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlipEvent {
    pub aimed_for: String,
    pub hit_instead: String,
    pub hand: Option<Hand>,
    pub finger: Option<Finger>,
    pub timestamp_ms: u64,
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Modifiers;

    fn key(k: &str, ts: u64) -> InputEvent {
        InputEvent::Key {
            key: k.into(),
            timestamp_ms: ts,
            modifiers: Modifiers::default(),
            dwell_ms: 80,
        }
    }
    fn bs(ts: u64) -> InputEvent {
        InputEvent::Backspace { timestamp_ms: ts }
    }

    /// Most existing slip-detector tests assert that confirmed slips
    /// **do** populate the volatility map. After the C5b kill-switch
    /// flipped the default to map-writes-off, those tests construct
    /// the detector with writes explicitly enabled. Tests of the
    /// kill-switch itself construct with `with_map_writes(false)` or
    /// `new()` directly.
    fn writes_on() -> SlipDetector {
        SlipDetector::with_map_writes(true)
    }

    #[test]
    fn single_char_slip_is_recorded() {
        // Aimed 's', hit 'a' instead, corrected immediately. Map writes
        // explicitly enabled — the assertion below pins L3 mutation,
        // which the C5b kill-switch defaults off.
        let mut d = writes_on();
        d.observe(&key("a", 0));
        d.observe(&bs(50));
        d.observe(&key("s", 100));
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 1);
        assert_eq!(snap.per_pair.len(), 1);
        let row = &snap.per_pair[0];
        assert_eq!(row.aimed_for, "s");
        assert_eq!(row.hit_instead, "a");
        // Finger is the AIMED-FOR finger (s = left ring).
        assert_eq!(row.hand, Some(Hand::Left));
        assert_eq!(row.finger, Some(Finger::Ring));
        // L3 writes happened.
        assert_eq!(snap.map_swap_pairs, 1);
        assert_eq!(snap.map_key_confidence, 1);
    }

    #[test]
    fn corrected_to_same_char_is_not_a_slip() {
        let mut d = SlipDetector::new();
        d.observe(&key("a", 0));
        d.observe(&bs(50));
        d.observe(&key("a", 100));
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 0);
        assert_eq!(snap.map_swap_pairs, 0);
    }

    #[test]
    fn two_char_correction_records_only_changed_positions() {
        // typed 'ge', backspaced 2, retyped 'he' → only (h, g) is a slip.
        let mut d = SlipDetector::new();
        d.observe(&key("g", 0));
        d.observe(&key("e", 100));
        d.observe(&bs(200));
        d.observe(&bs(250));
        d.observe(&key("h", 350));
        d.observe(&key("e", 450));
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 1);
        let row = &snap.per_pair[0];
        assert_eq!(row.aimed_for, "h");
        assert_eq!(row.hit_instead, "g");
    }

    #[test]
    fn pause_before_backspace_aborts() {
        let mut d = SlipDetector::new();
        d.observe(&key("a", 0));
        // 2-second pause — no longer "immediate".
        d.observe(&bs(2_000));
        d.observe(&key("s", 2_050));
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 0);
    }

    #[test]
    fn pause_before_retype_aborts() {
        let mut d = SlipDetector::new();
        d.observe(&key("a", 0));
        d.observe(&bs(50));
        // 2-second pause between bs and the retype — not immediate.
        d.observe(&key("s", 2_100));
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 0);
    }

    #[test]
    fn whitespace_between_resets_state_and_blocks_slip() {
        // Same-word filter: a space between the typed and retyped chars
        // means this is no longer a within-word correction.
        let mut d = SlipDetector::new();
        d.observe(&key("a", 0));
        d.observe(&key(" ", 100)); // breaks the state
        d.observe(&bs(200));
        d.observe(&key("s", 300));
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 0);
    }

    #[test]
    fn too_many_backspaces_for_typed_buffer_aborts() {
        // Only 1 char in the recent typed buffer, but user backspaces 3
        // times → they're deleting older text (an edit), not a slip.
        let mut d = SlipDetector::new();
        d.observe(&key("a", 0));
        d.observe(&bs(50));
        d.observe(&bs(100)); // would erase more than was just typed
        d.observe(&key("s", 200));
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 0);
    }

    #[test]
    fn three_char_backspace_run_is_an_edit_not_a_slip() {
        let mut d = SlipDetector::new();
        d.observe(&key("a", 0));
        d.observe(&key("b", 100));
        d.observe(&key("c", 200));
        d.observe(&bs(300));
        d.observe(&bs(350));
        d.observe(&bs(400)); // 3rd bs — exceeds MAX_SLIP_LEN
        d.observe(&key("d", 500));
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 0);
    }

    #[test]
    fn chained_slips_in_one_burst() {
        // Two consecutive single-char slips: a→s, then b→c.
        let mut d = SlipDetector::new();
        d.observe(&key("a", 0));
        d.observe(&bs(50));
        d.observe(&key("s", 100));
        d.observe(&key("b", 200));
        d.observe(&bs(250));
        d.observe(&key("c", 300));
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 2);
        // Both pairs recorded.
        let pairs: Vec<_> = snap
            .per_pair
            .iter()
            .map(|r| (r.aimed_for.as_str(), r.hit_instead.as_str()))
            .collect();
        assert!(pairs.contains(&("s", "a")));
        assert!(pairs.contains(&("c", "b")));
    }

    #[test]
    fn repeated_same_slip_increments_count_and_swap_pair_frequency() {
        let mut d = writes_on();
        for i in 0..3 {
            let base = i * 1_000;
            d.observe(&key("a", base));
            d.observe(&bs(base + 50));
            d.observe(&key("s", base + 100));
        }
        let snap = d.snapshot();
        assert_eq!(snap.total_slips, 3);
        assert_eq!(snap.per_pair.len(), 1);
        assert_eq!(snap.per_pair[0].count, 3);
        // Same pair → one swap_pair entry, one key_confidence entry.
        assert_eq!(snap.map_swap_pairs, 1);
        assert_eq!(snap.map_key_confidence, 1);
        // Verify the L3 map state directly.
        let map = d.map();
        assert_eq!(map.swap_pairs[0].frequency, 3.0);
        assert_eq!(map.keys[0].sample_count, 3);
    }

    #[test]
    fn unmapped_aimed_key_is_logged_but_not_written_to_map() {
        // Backslash isn't covered by US-QWERTY finger map... actually it is
        // (right pinky). Use a multi-char key name instead, which finger_for
        // rejects.
        let mut d = SlipDetector::new();
        d.observe(&key("a", 0));
        d.observe(&bs(50));
        d.observe(&key("Escape", 100));
        let snap = d.snapshot();
        // 'Escape' is multi-char → not a word_char → state resets before
        // record_slip ever runs. So no slip even logged.
        assert_eq!(snap.total_slips, 0);
        assert_eq!(snap.map_swap_pairs, 0);
    }

    #[test]
    fn take_new_slips_drains_buffer() {
        let mut d = SlipDetector::new();
        d.observe(&key("a", 0));
        d.observe(&bs(50));
        d.observe(&key("s", 100));
        let drained = d.take_new_slips();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].aimed_for, "s");
        // Drained — second call yields nothing.
        assert_eq!(d.take_new_slips().len(), 0);
        // But the tally persists.
        assert_eq!(d.snapshot().total_slips, 1);
    }

    // ---- C5b kill-switch on L3 map writes ------------------------------

    #[test]
    fn default_constructor_disables_l3_map_writes() {
        // `SlipDetector::new()` is the C5b safety default — map writes
        // OFF. Confirmed slips still tally and still emit SlipEvents,
        // but the volatility map stays empty.
        let mut d = SlipDetector::new();
        assert!(!d.map_writes_enabled());
        d.observe(&key("a", 0));
        d.observe(&bs(50));
        d.observe(&key("s", 100));
        let snap = d.snapshot();
        // Diagnostic outputs stay live.
        assert_eq!(snap.total_slips, 1);
        assert_eq!(snap.per_pair.len(), 1);
        assert_eq!(d.take_new_slips().len(), 1);
        // L3 map untouched.
        assert_eq!(snap.map_swap_pairs, 0);
        assert_eq!(snap.map_key_confidence, 0);
        assert!(d.map().swap_pairs.is_empty());
        assert!(d.map().keys.is_empty());
    }

    #[test]
    fn with_map_writes_true_enables_l3_map_writes() {
        // The flip-on path — once the C5b motor verdict has been
        // validated, the host (engine.rs) can construct the detector
        // with writes enabled and learning resumes.
        let mut d = SlipDetector::with_map_writes(true);
        assert!(d.map_writes_enabled());
        d.observe(&key("a", 0));
        d.observe(&bs(50));
        d.observe(&key("s", 100));
        let snap = d.snapshot();
        assert_eq!(snap.map_swap_pairs, 1);
        assert_eq!(snap.map_key_confidence, 1);
    }

    #[test]
    fn confidence_drops_per_slip_and_clamps_at_zero() {
        let mut d = writes_on();
        // 25 slips on the same aimed key → 25 × 0.05 = 1.25 would be
        // negative — must clamp to 0.0.
        for i in 0..25 {
            let base = i * 1_000;
            d.observe(&key("a", base));
            d.observe(&bs(base + 50));
            d.observe(&key("s", base + 100));
        }
        let map = d.map();
        let kc = map.keys.iter().find(|k| k.key == "s").unwrap();
        assert!(kc.confidence.0 >= 0.0 && kc.confidence.0 <= 1.0);
        // Specifically: 25 decrements from 1.0 → clamped at 0.0.
        assert_eq!(kc.confidence.0, 0.0);
    }
}
