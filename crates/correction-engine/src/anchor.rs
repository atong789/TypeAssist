//! Span anchor — Component 2 of the L4 Observing brief.
//!
//! Each sealed `Word` token from [`crate::tokenizer`] gets a [`SpanAnchor`]
//! that tracks the original `[start, end)` char-span of its core across
//! subsequent edits in the same line. The anchor keeps pointing at the
//! same word as the user types / deletes around it — so a later observer
//! can read "what's at that span now?" without rescanning the whole line.
//!
//! ## Scope of this slice
//!
//! Just the tracker and the three void conditions. Explicitly NOT in this
//! component:
//! * log record / confidence score / outcome resolution
//! * mouse-click caret repositioning, paste — those need the
//!   Accessibility-read backstop (Component 5)
//! * the L4 correction decision (lives in `crate::decision`)
//!
//! ## Edit-delta model (no replay, no re-scan)
//!
//! Each edit is one of:
//! * char insert at position `p` — single char.
//! * single-char delete at position `p` (Backspace).
//!
//! For every live anchor, per edit, the brief specifies three cases on the
//! shift/resize axis. Translating (`d` = chars deleted, `i` = chars
//! inserted; here always 0/1 or 1/0):
//!
//! ```text
//! p + d <= start   →  start += (i - d); end += (i - d)   (shift)
//! p >= end         →  ignore                              (after)
//! otherwise        →  end += (i - d)                      (resize: inside)
//! ```
//!
//! On top of that, three **void** conditions promote the anchor from
//! `Tracking` to `Void(reason)` (the anchor stays in the list so the panel
//! can keep showing it, but it no longer participates in shift/resize):
//!
//! * **Split** — a whitespace character was inserted strictly inside the
//!   anchor's `[start, end)` — the word is being broken in two.
//! * **Merge** — a boundary whitespace immediately before or after the
//!   word was deleted — the word is fusing with a neighbour.
//! * **Deleted** — after a delete inside the span, `end <= start`. The
//!   whole core is gone.
//!
//! Void detection happens *before* shift/resize so the position math
//! reflects the pre-edit anchor.

use serde::{Deserialize, Serialize};

use crate::tokenizer::TokenKind;

/// A single tracked word span. `id` is monotonic per [`AnchorTracker`]; the
/// state can transition `Tracking → Void(_)` exactly once and then sticks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpanAnchor {
    pub id: u32,
    pub original_core: String,
    pub start: usize,
    pub end: usize,
    pub state: AnchorState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnchorState {
    Tracking,
    Void { reason: VoidReason },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VoidReason {
    /// A whitespace was inserted strictly inside the word.
    Split,
    /// A boundary whitespace before or after the word was deleted.
    Merge,
    /// The whole core was deleted (end ≤ start after a delete).
    Deleted,
}

#[derive(Debug, Default)]
pub struct AnchorTracker {
    anchors: Vec<SpanAnchor>,
    next_id: u32,
}

impl AnchorTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn anchors(&self) -> &[SpanAnchor] {
        &self.anchors
    }

    /// Drop every anchor — called on a true line reset (newline, special
    /// key, focus change). Does NOT reset `next_id` so anchor IDs stay
    /// unique across the session.
    pub fn clear(&mut self) {
        self.anchors.clear();
    }

    /// Register an anchor for a sealed Word token. If a live `Tracking`
    /// anchor already exists at exactly this span with the same core,
    /// returns `None` and adds nothing — dedupes replay after a tokenizer
    /// rebuild (Component 1 backspace handling).
    ///
    /// Skips non-Word kinds at the call site by the brief's contract
    /// ("ONLY Word tokens"); this method is permissive about kind, callers
    /// must filter.
    pub fn try_register(&mut self, start: usize, end: usize, core: &str) -> Option<u32> {
        let already = self.anchors.iter().any(|a| {
            matches!(a.state, AnchorState::Tracking)
                && a.start == start
                && a.end == end
                && a.original_core == core
        });
        if already {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.anchors.push(SpanAnchor {
            id,
            original_core: core.to_string(),
            start,
            end,
            state: AnchorState::Tracking,
        });
        Some(id)
    }

    /// Look up the id of the live `Tracking` anchor at exactly this
    /// span and core, if one exists. Returns `None` when there is no
    /// matching anchor or when the only match has transitioned to
    /// `Void(_)`.
    ///
    /// Use when a caller has just offered an anchor via [`try_register`]
    /// and needs the id whether the call freshly created it (`Some(id)`)
    /// or it was deduplicated as a replay (`None`). Pair them:
    ///
    /// ```ignore
    /// let id = anchors.try_register(start, end, core)
    ///     .or_else(|| anchors.find_tracking_id(start, end, core));
    /// ```
    ///
    /// Component 4's decision ledger uses this to attach `anchor_id` to
    /// every record (the bridge C5 uses to resolve outcomes), so the
    /// link survives mid-line replays after backspaces or inserts.
    pub fn find_tracking_id(&self, start: usize, end: usize, core: &str) -> Option<u32> {
        self.anchors
            .iter()
            .find(|a| {
                matches!(a.state, AnchorState::Tracking)
                    && a.start == start
                    && a.end == end
                    && a.original_core == core
            })
            .map(|a| a.id)
    }

    /// Convenience wrapper: register only if the token is a Word.
    pub fn try_register_from(
        &mut self,
        kind: TokenKind,
        start: usize,
        end: usize,
        core: &str,
    ) -> Option<u32> {
        if matches!(kind, TokenKind::Word) {
            self.try_register(start, end, core)
        } else {
            None
        }
    }

    /// Apply a single-character insert at position `p`.
    pub fn apply_insert(&mut self, p: usize, c: char) {
        let is_ws = is_whitespace(c);
        for anchor in &mut self.anchors {
            if !matches!(anchor.state, AnchorState::Tracking) {
                continue;
            }
            // Brief: p + d <= start (d = 0 here) → shift.
            if p <= anchor.start {
                anchor.start += 1;
                anchor.end += 1;
            } else if p >= anchor.end {
                // After: ignore.
            } else {
                // Inside (start < p < end).
                if is_ws {
                    anchor.state = AnchorState::Void {
                        reason: VoidReason::Split,
                    };
                } else {
                    anchor.end += 1;
                }
            }
        }
    }

    /// Apply a single-character delete at position `p`. `c` is the char
    /// that was at `p` before deletion (used to detect Merge — boundary
    /// whitespace deleted next to a word).
    ///
    /// **Merge requires a partner.** A whitespace deletion at the boundary
    /// of an anchor only fires `Void(Merge)` if there is a live `Tracking`
    /// anchor on the *other* side of the deleted space — i.e. two words are
    /// actually being fused. Deleting a trailing space at end-of-line (no
    /// word after) or a leading space at start-of-line (no word before) is
    /// just an ordinary edit and falls through to the standard shift /
    /// ignore / resize rules.
    pub fn apply_delete(&mut self, p: usize, c: char) {
        let is_ws = is_whitespace(c);

        // Pre-compute partner availability per anchor so the mutating loop
        // can run without a second borrow of the anchor list.
        // (`has_partner_before[i]`, `has_partner_after[i]`)
        let has_partner: Vec<(bool, bool)> = if is_ws {
            // Snapshot enough state to ask "is there another live Tracking
            // anchor whose end == p / start == p + 1?".
            let snap: Vec<(usize, usize, bool)> = self
                .anchors
                .iter()
                .map(|a| (a.start, a.end, matches!(a.state, AnchorState::Tracking)))
                .collect();
            self.anchors
                .iter()
                .enumerate()
                .map(|(i, anchor)| {
                    if !matches!(anchor.state, AnchorState::Tracking) {
                        return (false, false);
                    }
                    let before_boundary = p + 1 == anchor.start;
                    let after_boundary = p == anchor.end;
                    let has_before = before_boundary
                        && snap.iter().enumerate().any(|(j, &(_s, e, t))| {
                            j != i && t && e == p
                        });
                    let has_after = after_boundary
                        && snap.iter().enumerate().any(|(j, &(s, _e, t))| {
                            j != i && t && s == p + 1
                        });
                    (has_before, has_after)
                })
                .collect()
        } else {
            vec![(false, false); self.anchors.len()]
        };

        for (i, anchor) in self.anchors.iter_mut().enumerate() {
            if !matches!(anchor.state, AnchorState::Tracking) {
                continue;
            }

            let (has_before, has_after) = has_partner[i];

            // Boundary BEFORE the word: deleting the space at index
            // `anchor.start - 1` AND there's a partner anchor ending at p.
            if has_before {
                anchor.state = AnchorState::Void {
                    reason: VoidReason::Merge,
                };
                anchor.start -= 1;
                anchor.end -= 1;
                continue;
            }
            // Boundary AFTER the word: deleting the space at `anchor.end`
            // AND there's a partner anchor starting at p + 1.
            if has_after {
                anchor.state = AnchorState::Void {
                    reason: VoidReason::Merge,
                };
                continue;
            }

            // Regular shift / ignore / resize. A boundary-space deletion
            // without a partner falls through here — exactly what we want:
            // a trailing-space deletion with nothing on the far side is
            // just an `p >= end → ignore`.
            if p + 1 <= anchor.start {
                anchor.start -= 1;
                anchor.end -= 1;
            } else if p >= anchor.end {
                // After: ignore.
            } else {
                // Inside (start ≤ p < end). end -= 1; possibly Void(Deleted).
                anchor.end -= 1;
                if anchor.end <= anchor.start {
                    anchor.state = AnchorState::Void {
                        reason: VoidReason::Deleted,
                    };
                }
            }
        }
    }

    pub fn snapshot(&self) -> AnchorsSnapshot {
        let void_count = self
            .anchors
            .iter()
            .filter(|a| matches!(a.state, AnchorState::Void { .. }))
            .count() as u32;
        AnchorsSnapshot {
            anchors: self.anchors.clone(),
            void_count,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnchorsSnapshot {
    pub anchors: Vec<SpanAnchor>,
    pub void_count: u32,
}

fn is_whitespace(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\n' || c == '\r'
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> AnchorTracker {
        AnchorTracker::new()
    }

    fn tracking(a: &SpanAnchor) -> bool {
        matches!(a.state, AnchorState::Tracking)
    }

    fn void_reason(a: &SpanAnchor) -> Option<VoidReason> {
        match a.state {
            AnchorState::Void { reason } => Some(reason),
            _ => None,
        }
    }

    // ---- Register --------------------------------------------------------

    #[test]
    fn registers_word_only() {
        let mut tr = t();
        assert!(tr
            .try_register_from(TokenKind::Word, 0, 5, "hello")
            .is_some());
        assert!(tr
            .try_register_from(TokenKind::Number, 6, 10, "3.14")
            .is_none());
        assert!(tr
            .try_register_from(TokenKind::Acronym, 11, 15, "NASA")
            .is_none());
        assert!(tr
            .try_register_from(TokenKind::Url, 0, 11, "example.com")
            .is_none());
        assert!(tr
            .try_register_from(TokenKind::Email, 0, 8, "x@y.com")
            .is_none());
        assert!(tr
            .try_register_from(TokenKind::Code, 0, 6, "abc123")
            .is_none());
        assert_eq!(tr.anchors().len(), 1);
    }

    #[test]
    fn dedupes_replay_of_same_span_and_core() {
        let mut tr = t();
        let id1 = tr.try_register(0, 5, "hello").unwrap();
        // Same span + core again → no-op.
        let id2 = tr.try_register(0, 5, "hello");
        assert_eq!(id2, None);
        assert_eq!(tr.anchors()[0].id, id1);
        assert_eq!(tr.anchors().len(), 1);
    }

    #[test]
    fn find_tracking_id_returns_id_for_live_anchor() {
        let mut tr = t();
        let id = tr.try_register(0, 5, "hello").unwrap();
        // Same span/core after the fact → returns the live id.
        assert_eq!(tr.find_tracking_id(0, 5, "hello"), Some(id));
    }

    #[test]
    fn find_tracking_id_misses_on_voided_anchor() {
        // An anchor that's been voided is no longer a valid C4 attribution
        // target — find_tracking_id refuses to surface its id.
        let mut tr = t();
        tr.try_register(0, 5, "hello").unwrap();
        tr.apply_insert(2, ' '); // → Void(Split)
        assert_eq!(tr.find_tracking_id(0, 5, "hello"), None);
    }

    #[test]
    fn find_tracking_id_misses_on_unknown_span() {
        let mut tr = t();
        tr.try_register(0, 5, "hello").unwrap();
        // Different start.
        assert_eq!(tr.find_tracking_id(1, 5, "hello"), None);
        // Different end.
        assert_eq!(tr.find_tracking_id(0, 6, "hello"), None);
        // Different core.
        assert_eq!(tr.find_tracking_id(0, 5, "hallo"), None);
    }

    #[test]
    fn different_core_at_same_span_is_a_new_anchor() {
        let mut tr = t();
        tr.try_register(0, 4, "hell").unwrap();
        tr.try_register(0, 4, "hold").unwrap(); // different content same span
        assert_eq!(tr.anchors().len(), 2);
    }

    // ---- Shift -----------------------------------------------------------

    #[test]
    fn insert_before_word_shifts_right() {
        // "hello"  +  insert "x" at p=0  →  "xhello"
        // anchor [0,5) → [1,6)
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_insert(0, 'x');
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (1, 6));
        assert!(tracking(&tr.anchors()[0]));
    }

    #[test]
    fn delete_before_word_shifts_left() {
        // " hello"  + delete at p=0 (the leading space, but not at boundary)
        // anchor at [1,6) → [0,5)
        // The deleted char here is 'x' — not whitespace, not at boundary,
        // so just a plain shift.
        let mut tr = t();
        tr.try_register(1, 6, "hello");
        tr.apply_delete(0, 'x');
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 5));
        assert!(tracking(&tr.anchors()[0]));
    }

    // ---- Ignore-after ----------------------------------------------------

    #[test]
    fn insert_after_word_is_ignored() {
        // "hello world" — anchor for "hello" at [0,5). Insert at p=8 → ignored.
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_insert(8, 'x');
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 5));
        assert!(tracking(&tr.anchors()[0]));
    }

    #[test]
    fn delete_after_word_is_ignored() {
        // Delete at p=10, well past anchor [0,5). p >= end → ignore.
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_delete(10, 'x');
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 5));
        assert!(tracking(&tr.anchors()[0]));
    }

    // ---- Resize ----------------------------------------------------------

    #[test]
    fn insert_inside_grows_end() {
        // "hello" + insert "X" at p=3 → "helXlo" (anchor end grows by 1).
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_insert(3, 'X');
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 6));
        assert!(tracking(&tr.anchors()[0]));
    }

    #[test]
    fn delete_inside_shrinks_end() {
        // "hello" + delete at p=3 (deletes 'l') → "helo".
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_delete(3, 'l');
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 4));
        assert!(tracking(&tr.anchors()[0]));
    }

    // ---- Void: Split -----------------------------------------------------

    #[test]
    fn space_inserted_inside_word_voids_split() {
        // "hello" + insert ' ' at p=2 → "he llo". Anchor voids.
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_insert(2, ' ');
        assert_eq!(void_reason(&tr.anchors()[0]), Some(VoidReason::Split));
        // Position stays where it was — Void state freezes shift/resize math.
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 5));
        assert_eq!(tr.snapshot().void_count, 1);
    }

    #[test]
    fn tab_inserted_inside_word_voids_split() {
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_insert(3, '\t');
        assert_eq!(void_reason(&tr.anchors()[0]), Some(VoidReason::Split));
    }

    #[test]
    fn space_inserted_at_word_end_is_not_split() {
        // p == end → "after" branch — ignored, no void.
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_insert(5, ' ');
        assert!(tracking(&tr.anchors()[0]));
    }

    #[test]
    fn space_inserted_at_word_start_is_not_split() {
        // p == start → shift, not inside, no void.
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_insert(0, ' ');
        assert!(tracking(&tr.anchors()[0]));
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (1, 6));
    }

    // ---- Void: Merge -----------------------------------------------------

    #[test]
    fn boundary_space_between_two_words_voids_both_merge() {
        // "alpha beta" — two anchors. Delete the boundary space at p=5.
        // alpha at [0,5) sees `p == end` AND a partner ("beta") starting
        // at p+1=6 → Merge. beta at [6,10) sees `p+1 == start` AND a
        // partner ("alpha") ending at p=5 → Merge. Both void.
        let mut tr = t();
        tr.try_register(0, 5, "alpha");
        tr.try_register(6, 10, "beta");
        tr.apply_delete(5, ' ');
        assert_eq!(void_reason(&tr.anchors()[0]), Some(VoidReason::Merge));
        assert_eq!(void_reason(&tr.anchors()[1]), Some(VoidReason::Merge));
    }

    #[test]
    fn trailing_space_with_no_following_word_is_not_merge() {
        // The user's repro: "hello " then backspace the trailing space.
        // No anchor at start == 6, so no merge — just `p >= end → ignore`.
        // Anchor stays Tracking; subsequent deletes will eventually hit
        // Void(Deleted).
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_delete(5, ' ');
        assert!(tracking(&tr.anchors()[0]));
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 5));
    }

    #[test]
    fn leading_space_with_no_preceding_word_is_not_merge() {
        // " hello" with the only anchor at [1,6). Delete the leading
        // space — no partner before, so just a shift.
        let mut tr = t();
        tr.try_register(1, 6, "hello");
        tr.apply_delete(0, ' ');
        assert!(tracking(&tr.anchors()[0]));
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 5));
    }

    #[test]
    fn backspace_through_whole_word_after_trailing_space_voids_deleted() {
        // Full repro from the user's report: type "hello ", then backspace
        // the whole word. Expected end state: Void(Deleted), NOT Merge.
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        // First delete: the trailing space — no merge partner.
        tr.apply_delete(5, ' ');
        assert!(tracking(&tr.anchors()[0]));
        // Now backspace each letter, right to left.
        tr.apply_delete(4, 'o');
        tr.apply_delete(3, 'l');
        tr.apply_delete(2, 'l');
        tr.apply_delete(1, 'e');
        tr.apply_delete(0, 'h');
        assert_eq!(void_reason(&tr.anchors()[0]), Some(VoidReason::Deleted));
    }

    #[test]
    fn delete_non_boundary_space_is_not_merge() {
        // Delete a space at p=20 (far away from any anchor) → just ignore.
        let mut tr = t();
        tr.try_register(1, 6, "hello");
        tr.apply_delete(20, ' ');
        assert!(tracking(&tr.anchors()[0]));
    }

    // ---- Void: Deleted ---------------------------------------------------

    #[test]
    fn whole_core_deleted_voids() {
        // "hi" — anchor [0,2). Delete inside twice → empty.
        let mut tr = t();
        tr.try_register(0, 2, "hi");
        tr.apply_delete(1, 'i'); // [0,1)
        assert!(tracking(&tr.anchors()[0]));
        tr.apply_delete(0, 'h'); // [0,0) — end <= start → Deleted
        assert_eq!(void_reason(&tr.anchors()[0]), Some(VoidReason::Deleted));
        assert_eq!(tr.snapshot().void_count, 1);
    }

    #[test]
    fn single_char_word_one_delete_voids() {
        // Single-char word "a" — one delete → Void(Deleted).
        let mut tr = t();
        tr.try_register(0, 1, "a");
        tr.apply_delete(0, 'a');
        assert_eq!(void_reason(&tr.anchors()[0]), Some(VoidReason::Deleted));
    }

    // ---- State stickiness ------------------------------------------------

    #[test]
    fn voided_anchors_no_longer_move() {
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.apply_insert(2, ' '); // → Void(Split)
        let (s, e) = (tr.anchors()[0].start, tr.anchors()[0].end);
        // Subsequent edits should not change the voided anchor's span.
        tr.apply_insert(0, 'X');
        tr.apply_delete(0, 'X');
        tr.apply_insert(10, 'Y');
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (s, e));
        assert_eq!(void_reason(&tr.anchors()[0]), Some(VoidReason::Split));
    }

    // ---- Multi-anchor coordination --------------------------------------

    #[test]
    fn shift_affects_all_following_anchors() {
        // Three words: "the quick brown" at [0,3), [4,9), [10,15).
        // Insert 'X' at p=0 → all three shift right.
        let mut tr = t();
        tr.try_register(0, 3, "the");
        tr.try_register(4, 9, "quick");
        tr.try_register(10, 15, "brown");
        tr.apply_insert(0, 'X');
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (1, 4));
        assert_eq!((tr.anchors()[1].start, tr.anchors()[1].end), (5, 10));
        assert_eq!((tr.anchors()[2].start, tr.anchors()[2].end), (11, 16));
    }

    #[test]
    fn delete_inside_one_word_does_not_touch_others() {
        let mut tr = t();
        tr.try_register(0, 3, "the");
        tr.try_register(4, 9, "quick");
        tr.try_register(10, 15, "brown");
        // Delete inside "quick" at p=5 ('u').
        tr.apply_delete(5, 'u');
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 3));
        assert_eq!((tr.anchors()[1].start, tr.anchors()[1].end), (4, 8));
        // "brown" was at [10,15); the delete at p=5 is BEFORE its start
        // (p + d = 6 ≤ 10), so it shifts left.
        assert_eq!((tr.anchors()[2].start, tr.anchors()[2].end), (9, 14));
    }

    // ---- Clear -----------------------------------------------------------

    #[test]
    fn clear_drops_every_anchor() {
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.try_register(6, 11, "world");
        tr.clear();
        assert!(tr.anchors().is_empty());
        assert_eq!(tr.snapshot().void_count, 0);
        // IDs keep climbing — that's the contract.
        let id = tr.try_register(0, 5, "hello").unwrap();
        assert_eq!(id, 2);
    }

    // ---- End-to-end scenarios mirrored from the engine -----------------
    // These reproduce the position math the engine produces when the user
    // types the sequences described in the bug report. They don't drive a
    // real tokenizer / engine loop — that's an integration-level concern —
    // but they pin the anchor math so regressions surface here first.

    #[test]
    fn scenario_home_then_x_space_shifts_abc_by_two() {
        // Sequence: "abc " (seals "abc" → anchor at [0,3)), Home, "x ".
        // After Home the caret is at 0. The next insert is at p=0 (anchor
        // shifts right by 1); the one after that is at p=1 (anchor shifts
        // right by 1 again). Total: +2 on both ends.
        let mut tr = t();
        tr.try_register(0, 3, "abc");
        // Home is a caret move only — no anchor change.
        tr.apply_insert(0, 'x'); // [1,4)
        tr.apply_insert(1, ' '); // [2,5)
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (2, 5));
        assert!(tracking(&tr.anchors()[0]));
    }

    #[test]
    fn scenario_left_then_u_inside_word_grows_existing_anchor() {
        // Sequence: "color" (no boundary yet, no anchor) … Left, "u" …
        // boundary. In the engine, the mid-line 'u' triggers a rebuild and
        // the sealed token is "colour" at [0,6). At anchor level, the
        // closest behaviour to pin is: if an anchor for "color" had already
        // been registered (hypothetical), the mid-line insert at p=4 would
        // grow its end to 6.
        let mut tr = t();
        tr.try_register(0, 5, "color");
        tr.apply_insert(4, 'u'); // p=4 is inside [0,5) → grow end.
        assert_eq!((tr.anchors()[0].start, tr.anchors()[0].end), (0, 6));
        assert!(tracking(&tr.anchors()[0]));
    }

    // ---- Snapshot --------------------------------------------------------

    #[test]
    fn void_count_in_snapshot() {
        let mut tr = t();
        tr.try_register(0, 5, "hello");
        tr.try_register(6, 11, "world");
        // Void the first via Split.
        tr.apply_insert(2, ' ');
        // Void the second via Deleted. After the insert above, "world" sits
        // at [7,12) (shifted right by 1). Six deletes at p=6:
        //   1st: shift  →  [6,11)
        //   2nd–6th: resize-inside → end shrinks to 6 → Void(Deleted).
        for _ in 0..6 {
            tr.apply_delete(6, 'x');
        }
        let snap = tr.snapshot();
        assert_eq!(snap.void_count, 2);
    }
}
