//! Coordination/Precision classification of a `typed → target` slip.
//!
//! The Progress view's "what the slip rate is made of" splits each motor slip
//! into one of two shapes (see the build brief):
//!
//! - **Coordination** — a *transposition*: the right letters in the wrong
//!   order, an adjacent pair swapped (`teh → the`, `adn → and`). The hand knew
//!   the keys; the sequencing slipped.
//! - **Precision** — a *substitution* (wrong adjacent key, `wprd → word`),
//!   *insertion* (doubling/extra char, `worrd → word`), or *omission* (missing
//!   key, `wrd → word`). The hand mis-hit a single target.
//!
//! This is a **pure function of the pair** — no state, no timing — so it can be
//! *derived on read* wherever a `typed → target` is in hand (the Impact ledger
//! tag, the daily same-day counters). It is deliberately **not persisted** into
//! `word_patterns.json`: the on-disk store keeps its format, and the class is
//! recomputed when needed (it never changes for a given pair).
//!
//! `None` means the pair is **not a single motor slip** — identical strings, or
//! an edit too large to be a typo (a semantic rewrite). The motor-range guard
//! mirrors the word-pattern store's ([`MAX_PATTERN_EDIT_DISTANCE`] /
//! [`MAX_PATTERN_LENGTH_DIFF`]) so the same pairs the store treats as typos
//! classify here, and rewrites fall through to `None` in both.

use crate::word_pattern::{MAX_PATTERN_EDIT_DISTANCE, MAX_PATTERN_LENGTH_DIFF};

/// Shape version — bump alongside any change to the classification rule, so a
/// rolled-up snapshot count can be reconciled against the rule that produced it.
pub const SLIP_CLASS_VERSION: u32 = 1;

/// The two shapes a motor slip can take. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlipClass {
    /// Adjacent transposition — right letters, wrong order.
    Coordination,
    /// Substitution / insertion / omission — a single mis-hit key.
    Precision,
}

impl SlipClass {
    /// Short tag for the UI / serialized counts (`"coord"` / `"precis"`),
    /// matching the brief's row tags.
    pub fn as_tag(self) -> &'static str {
        match self {
            SlipClass::Coordination => "coord",
            SlipClass::Precision => "precis",
        }
    }
}

/// Classify a `typed → target` slip, or `None` if the pair isn't a single
/// motor slip (identical, or a semantic rewrite beyond the typo guard).
///
/// Coordination is checked first: an adjacent transposition has Levenshtein
/// distance 2 (two substitutions), so it would otherwise be swept into the
/// Precision branch.
pub fn classify_slip(typed: &str, target: &str) -> Option<SlipClass> {
    let t: Vec<char> = typed.chars().collect();
    let g: Vec<char> = target.chars().collect();

    if t == g {
        return None; // clean — not a slip
    }

    // Coordination: same length, exactly one adjacent pair swapped.
    if t.len() == g.len() && is_adjacent_transposition(&t, &g) {
        return Some(SlipClass::Coordination);
    }

    // Precision: substitution / insertion / omission, bounded to the same
    // motor-typo range the word-pattern store uses. Anything larger is a
    // semantic rewrite, not a slip.
    let len_diff = t.len().abs_diff(g.len());
    if len_diff <= MAX_PATTERN_LENGTH_DIFF && levenshtein(&t, &g) <= MAX_PATTERN_EDIT_DISTANCE {
        return Some(SlipClass::Precision);
    }

    None
}

/// True iff `t` and `g` are equal length and differ by exactly one swap of two
/// *adjacent* characters (a classic transposition): they disagree at exactly
/// two positions `i, i+1`, with `t[i] == g[i+1]` and `t[i+1] == g[i]`.
fn is_adjacent_transposition(t: &[char], g: &[char]) -> bool {
    if t.len() != g.len() {
        return false;
    }
    let diffs: Vec<usize> = (0..t.len()).filter(|&i| t[i] != g[i]).collect();
    if diffs.len() != 2 {
        return false;
    }
    let (i, j) = (diffs[0], diffs[1]);
    j == i + 1 && t[i] == g[j] && t[j] == g[i]
}

/// Plain Levenshtein edit distance (substitution = insertion = deletion = 1).
/// Words are short, so the simple O(n·m) row-DP is ample. Matches the metric
/// the word-pattern store guards on, so the two agree on what counts as a typo.
fn levenshtein(a: &[char], b: &[char]) -> usize {
    let n = b.len();
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transpositions_are_coordination() {
        assert_eq!(classify_slip("teh", "the"), Some(SlipClass::Coordination));
        assert_eq!(classify_slip("adn", "and"), Some(SlipClass::Coordination));
        assert_eq!(classify_slip("form", "from"), Some(SlipClass::Coordination));
    }

    #[test]
    fn substitution_is_precision() {
        // single adjacent-key substitution
        assert_eq!(classify_slip("wprd", "word"), Some(SlipClass::Precision));
    }

    #[test]
    fn insertion_is_precision() {
        // doubled / extra char
        assert_eq!(classify_slip("worrd", "word"), Some(SlipClass::Precision));
    }

    #[test]
    fn omission_is_precision() {
        // missing key
        assert_eq!(classify_slip("wrd", "word"), Some(SlipClass::Precision));
    }

    #[test]
    fn identical_is_not_a_slip() {
        assert_eq!(classify_slip("word", "word"), None);
    }

    #[test]
    fn semantic_rewrite_is_not_a_slip() {
        // edit distance / length diff beyond the typo guard
        assert_eq!(classify_slip("teh", "definitely"), None);
        assert_eq!(classify_slip("cat", "dog"), None); // 3 subs
    }

    #[test]
    fn non_adjacent_swap_is_not_coordination() {
        // first/last swapped (distance 2) but NOT adjacent → falls to the
        // precision guard, which rejects it (distance 2 ok, but it's two subs
        // at non-adjacent positions: still within dist 2, len_diff 0 → Precision)
        assert_eq!(classify_slip("dab", "bad"), Some(SlipClass::Precision));
    }

    #[test]
    fn tags_match_the_brief() {
        assert_eq!(SlipClass::Coordination.as_tag(), "coord");
        assert_eq!(SlipClass::Precision.as_tag(), "precis");
    }
}
