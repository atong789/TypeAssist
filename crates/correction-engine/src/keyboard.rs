//! Static QWERTY (US layout) keyboard geometry.
//!
//! A small lookup table used by the confidence scorer to ask "are these two
//! keys physically adjacent on the keyboard?" — the **cold-start signal**
//! for whether a substitution looks like a fat-finger slip. As the learned
//! [`volatility_map::VolatilityMap`] fills in, the scorer blends it in on
//! top; until then, this geometry carries the load.
//!
//! ## Layout assumptions
//!
//! US QWERTY, three letter rows with the standard horizontal stagger:
//! ```text
//! q w e r t y u i o p
//!  a s d f g h j k l
//!   z x c v b n m
//! ```
//!
//! Stagger is encoded as `+0.25` column offset for row 2 and `+0.75` for
//! row 3 — a rough approximation that yields the right neighbour
//! relationships (h↔j, h↔y, h↔n all adjacent; l↔s far apart).
//!
//! Non-letter keys are not modelled here. The scorer skips edits whose
//! endpoints aren't in this table.

/// Squared-distance cutoff for "adjacent". 1.5 keeps the eight letter
/// neighbours of any home-row key in (left/right same row, up-row, down-row
/// including the staggered diagonals) and rejects two-away keys.
const ADJACENCY_DISTANCE_CUTOFF: f32 = 1.5;

const ROW_TOP: &str = "qwertyuiop";
const ROW_HOME: &str = "asdfghjkl";
const ROW_BOTTOM: &str = "zxcvbnm";

/// (row, column) coordinate of a letter key, or `None` if it's not on the
/// modelled US QWERTY letter rows.
pub fn coord(c: char) -> Option<(f32, f32)> {
    let c = c.to_ascii_lowercase();
    if let Some(i) = ROW_TOP.chars().position(|ch| ch == c) {
        return Some((0.0, i as f32));
    }
    if let Some(i) = ROW_HOME.chars().position(|ch| ch == c) {
        return Some((1.0, i as f32 + 0.25));
    }
    if let Some(i) = ROW_BOTTOM.chars().position(|ch| ch == c) {
        return Some((2.0, i as f32 + 0.75));
    }
    None
}

/// Euclidean distance between two letter keys in the staggered layout.
/// `None` if either key isn't on the modelled rows.
pub fn distance(a: char, b: char) -> Option<f32> {
    let (ar, ac) = coord(a)?;
    let (br, bc) = coord(b)?;
    let dr = ar - br;
    let dc = ac - bc;
    Some((dr * dr + dc * dc).sqrt())
}

/// True iff two letter keys are physically adjacent on the modelled
/// QWERTY layout (within [`ADJACENCY_DISTANCE_CUTOFF`]). A key is **not**
/// adjacent to itself — adjacency is a *neighbour* relation, not
/// reflexive — so substitution scoring never flatters the no-op case.
pub fn are_adjacent(a: char, b: char) -> bool {
    if a.eq_ignore_ascii_case(&b) {
        return false;
    }
    distance(a, b).is_some_and(|d| d <= ADJACENCY_DISTANCE_CUTOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_adjacent(a: char, b: char) {
        assert!(are_adjacent(a, b), "expected {a:?} and {b:?} to be adjacent");
        // adjacency is symmetric.
        assert!(are_adjacent(b, a), "adjacency should be symmetric for {a:?} {b:?}");
    }

    fn assert_far(a: char, b: char) {
        assert!(!are_adjacent(a, b), "expected {a:?} and {b:?} to be far apart");
    }

    #[test]
    fn home_row_left_right_neighbours() {
        assert_adjacent('h', 'g');
        assert_adjacent('h', 'j');
        assert_adjacent('a', 's');
        assert_adjacent('k', 'l');
    }

    #[test]
    fn vertical_neighbours_with_stagger() {
        // h is below y/u and above n/b.
        assert_adjacent('h', 'y');
        assert_adjacent('h', 'u');
        assert_adjacent('h', 'n');
        assert_adjacent('h', 'b');
    }

    #[test]
    fn fat_finger_pairs_are_adjacent() {
        // Common substitution slips from real typing.
        assert_adjacent('a', 'q');
        assert_adjacent('a', 'z');
        assert_adjacent('e', 'r');
        assert_adjacent('i', 'o');
        assert_adjacent('m', 'n');
    }

    #[test]
    fn far_apart_keys_are_not_adjacent() {
        // The brief's example: l vs s — opposite ends of home row.
        assert_far('l', 's');
        assert_far('q', 'p');
        assert_far('z', 'm');
        assert_far('a', 'l');
    }

    #[test]
    fn case_insensitive() {
        // Tokenizer preserves case; scorer lowercases conceptually, so
        // adjacency must agree on either casing.
        assert!(are_adjacent('H', 'j'));
        assert!(are_adjacent('h', 'J'));
        assert!(are_adjacent('H', 'J'));
    }

    #[test]
    fn key_is_not_adjacent_to_itself() {
        // Adjacency is the *neighbour* relation. Self-adjacency would
        // make substitution scoring give a perfect-typed substitution
        // (typed K == cand K) a fat-finger boost, which is nonsense.
        assert!(!are_adjacent('h', 'h'));
        assert!(!are_adjacent('H', 'h'));
    }

    #[test]
    fn unknown_chars_return_none() {
        assert_eq!(coord(' '), None);
        assert_eq!(coord('1'), None);
        assert_eq!(coord('-'), None);
        assert_eq!(distance(' ', 'a'), None);
        assert!(!are_adjacent('1', '2'));
    }
}
