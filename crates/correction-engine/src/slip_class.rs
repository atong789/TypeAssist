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

/// For an adjacent transposition `typed → target`, the two **intended** keys
/// whose order slipped — the `target` characters at the swap positions, in
/// `target` order. `None` when the pair is not a single adjacent transposition
/// (i.e. exactly when [`classify_slip`] would not call it `Coordination`).
///
/// Used by the Progress keyboard view to attribute one coordination slip to the
/// specific keys it implicates: `teh → the` ⇒ `('h', 'e')` (the hand had `h`
/// and `e`; their sequencing slipped). Both keys carry the slip's weight.
pub fn transposition_keys(typed: &str, target: &str) -> Option<(char, char)> {
    let t: Vec<char> = typed.chars().collect();
    let g: Vec<char> = target.chars().collect();
    if t.len() != g.len() || !is_adjacent_transposition(&t, &g) {
        return None;
    }
    // The first disagreeing position is `i`; the swap is over `i, i+1`.
    let i = (0..t.len()).find(|&i| t[i] != g[i])?;
    Some((g[i], g[i + 1]))
}

/// The indices, in `target`, of the character(s) the correction changed — used
/// to softly mark the corrected letter(s) in the Impact view (`couod → could`
/// marks the `l`; `teh → the` marks the `h` and `e`).
///
/// Computed by stripping the longest common prefix and suffix the two strings
/// share; whatever remains in the middle of `target` is the corrected span. This
/// one rule covers every slip shape:
/// - **substitution** (`couod → could`) ⇒ the single replaced letter,
/// - **omission** (`wrd → word`) ⇒ the restored letter,
/// - **transposition** (`teh → the`) ⇒ both swapped letters,
/// - **doubling removed** (`worrd → word`) ⇒ empty (no `target` letter is "the
///   fix" — the correction only *deleted* a key), which the UI renders as a
///   plain pair with nothing highlighted.
///
/// Returns an empty vec for an identical pair. Indices are into `target.chars()`
/// (char positions, not bytes), matching how the UI iterates the string.
pub fn corrected_target_indices(typed: &str, target: &str) -> Vec<usize> {
    let t: Vec<char> = typed.chars().collect();
    let g: Vec<char> = target.chars().collect();
    if t == g {
        return Vec::new();
    }
    let max_common = t.len().min(g.len());
    let mut prefix = 0;
    while prefix < max_common && t[prefix] == g[prefix] {
        prefix += 1;
    }
    // Cap the suffix to what's left after the prefix so the two ranges can't
    // overlap (e.g. `worrd → word` then yields an empty span, not a stray index).
    let mut suffix = 0;
    while suffix < max_common - prefix && t[t.len() - 1 - suffix] == g[g.len() - 1 - suffix] {
        suffix += 1;
    }
    // `prefix <= g.len() - suffix` always holds (see the cap above), so this
    // range is well-formed and empty when the correction only removed a key.
    (prefix..g.len() - suffix).collect()
}

/// The keyboard keys a `typed → target` correction **involves**, so the
/// Progress map can associate each correction with the key(s) behind its
/// shading — selecting a key filters the panel to every correction touching it.
///
/// Same prefix/suffix diff as [`corrected_target_indices`], but unioning the
/// changed chars from BOTH sides (deduped, typed-side first):
/// - **Coordination** (`teh → the`) ⇒ the transposed pair `['e','h']`,
/// - **Precision substitution** (`couod → could`) ⇒ wrong + intended `['o','l']`,
/// - **Precision omission** (`wrd → word`) ⇒ the intended `['o']`,
/// - **Precision doubling** (`worrd → word`) ⇒ the doubled `['r']`.
///
/// Chars are returned as-is (lowercase letters in practice); the map matches them
/// against its single-letter keys. Empty for an identical pair.
pub fn involved_keys(typed: &str, target: &str) -> Vec<char> {
    let t: Vec<char> = typed.chars().collect();
    let g: Vec<char> = target.chars().collect();
    if t == g {
        return Vec::new();
    }
    let max_common = t.len().min(g.len());
    let mut prefix = 0;
    while prefix < max_common && t[prefix] == g[prefix] {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < max_common - prefix && t[t.len() - 1 - suffix] == g[g.len() - 1 - suffix] {
        suffix += 1;
    }
    let mut keys: Vec<char> = Vec::new();
    let changed = t[prefix..t.len() - suffix]
        .iter()
        .chain(g[prefix..g.len() - suffix].iter());
    for &c in changed {
        if !keys.contains(&c) {
            keys.push(c);
        }
    }
    keys
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
    fn transposition_keys_are_the_intended_pair() {
        // `teh → the`: the `h` and `e` are the keys whose order slipped.
        assert_eq!(transposition_keys("teh", "the"), Some(('h', 'e')));
        assert_eq!(transposition_keys("adn", "and"), Some(('n', 'd')));
        assert_eq!(transposition_keys("form", "from"), Some(('r', 'o')));
        // Not an adjacent transposition → no key pair (matches classify_slip).
        assert_eq!(transposition_keys("wprd", "word"), None);
        assert_eq!(transposition_keys("word", "word"), None);
        assert_eq!(transposition_keys("dab", "bad"), None);
    }

    #[test]
    fn tags_match_the_brief() {
        assert_eq!(SlipClass::Coordination.as_tag(), "coord");
        assert_eq!(SlipClass::Precision.as_tag(), "precis");
    }

    #[test]
    fn corrected_indices_mark_the_changed_letters() {
        // Substitution: the single replaced letter (design example couod → could).
        assert_eq!(corrected_target_indices("couod", "could"), vec![3]); // the `l`
        assert_eq!(corrected_target_indices("wprd", "word"), vec![1]); // the `o`
        // Omission: the restored letter.
        assert_eq!(corrected_target_indices("wrd", "word"), vec![1]); // the `o`
        // Transposition: BOTH swapped letters, in target order.
        assert_eq!(corrected_target_indices("teh", "the"), vec![1, 2]); // `h`,`e`
        // Doubling removed: no target letter is "the fix" → empty span.
        assert_eq!(corrected_target_indices("worrd", "word"), Vec::<usize>::new());
        // Identical pair: nothing to mark.
        assert_eq!(corrected_target_indices("word", "word"), Vec::<usize>::new());
    }

    #[test]
    fn involved_keys_associate_corrections_with_keys() {
        // Coordination: the transposed pair (order doesn't matter for filtering).
        let k = involved_keys("teh", "the");
        assert!(k.contains(&'e') && k.contains(&'h') && k.len() == 2);
        // Precision substitution: wrong + intended.
        let k = involved_keys("couod", "could");
        assert!(k.contains(&'o') && k.contains(&'l') && k.len() == 2);
        // Precision omission: the intended (restored) key.
        assert_eq!(involved_keys("wrd", "word"), vec!['o']);
        // Precision doubling: the doubled key.
        assert_eq!(involved_keys("worrd", "word"), vec!['r']);
        // Identical pair: nothing involved.
        assert_eq!(involved_keys("word", "word"), Vec::<char>::new());
    }

    #[test]
    fn corrected_indices_are_in_target_bounds() {
        // A guard against off-by-one / underflow on every classified pair.
        for (typed, target) in [
            ("couod", "could"),
            ("teh", "the"),
            ("wrd", "word"),
            ("worrd", "word"),
            ("adn", "and"),
            ("form", "from"),
        ] {
            let len = target.chars().count();
            for i in corrected_target_indices(typed, target) {
                assert!(i < len, "{typed}->{target}: index {i} out of bounds {len}");
            }
        }
    }
}
