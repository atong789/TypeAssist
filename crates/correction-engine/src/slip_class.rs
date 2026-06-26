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

use crate::keyboard;
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

    // A motor slip is a mis-hit of LETTER keys. An edit that only adds or removes
    // a non-letter (an apostrophe: `dont → don't`; punctuation; a digit) or only
    // flips letter case (`The → the`) is a grammar/format fix — autocorrect's
    // domain, not a motor slip. Filtered here, the single place the class is
    // derived, so every caller (the slip-rate tally, the Impact "Ready" group,
    // the kill-switch) inherits the same motor-only definition.
    if !is_letter_motor_edit(&t, &g) {
        return None;
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

/// One concrete single-motor-error shape — the *only* shapes a `typed → target`
/// pair may take to be admitted as a correction candidate (the kill-switch
/// motor-only nomination filter). See [`single_motor_edit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotorEdit {
    /// One letter substituted for a *physically adjacent* key (`wprd → word`,
    /// the `p`/`o` fat-finger). A non-adjacent substitution is NOT a motor
    /// error — it's a different word the hand never aimed at.
    AdjacentSubstitution,
    /// One adjacent pair swapped (`teh → the`). A single physical action even
    /// though its Levenshtein distance is 2.
    Transposition,
    /// One letter dropped — target is the source with exactly one letter
    /// inserted (`wrd → word`, `te → the`).
    DroppedLetter,
    /// One extra letter — target is the source with exactly one letter removed
    /// (`worrd → word`, the doubled `r`).
    ExtraLetter,
}

/// Stricter sibling of [`classify_slip`]: admit a `typed → target` pair **only**
/// when the target is reachable from the source by a *single* motor error —
/// one adjacent-key substitution, one transposition, one dropped letter, or one
/// extra letter — and return *which*. `None` otherwise.
///
/// This is the **correction-nomination** filter (kill-switch), deliberately
/// tighter than [`classify_slip`] (the Progress *display* classifier). The
/// display side may tag a pair Coordination/Precision for the slip-rate mirror
/// while this side refuses to ever steer toward it. The gap matters because
/// `classify_slip`'s edit-distance ≤ 2 admits *two* independent edits — a
/// substitution **plus** an insertion (`so → for`, `to → for`, `of → for`) or
/// two non-adjacent substitutions (`dab → bad`) — which are word-swaps /
/// "changed my mind" rewrites, not a single slip of the hand. We must never
/// nominate those as a fix.
///
/// The letter-only guard from [`classify_slip`] is reused, so apostrophe /
/// punctuation / casing edits (`dont → don't`, `The → the`) fall through to
/// `None` here exactly as they do there.
///
/// Single-letter sources/targets (`s → is`, `f → of`) *can* satisfy this filter
/// (a dropped letter), so the kill-switch applies its own single-letter guard on
/// top — intent-ambiguity is a separate concern from motor shape.
pub fn single_motor_edit(typed: &str, target: &str) -> Option<MotorEdit> {
    let t: Vec<char> = typed.chars().collect();
    let g: Vec<char> = target.chars().collect();

    if t == g {
        return None; // no edit at all
    }
    // Apostrophe / punctuation / casing fixes are not motor slips (shared
    // definition with `classify_slip`), so they can never be nominated.
    if !is_letter_motor_edit(&t, &g) {
        return None;
    }

    match (t.len(), g.len()) {
        // Same length: a substitution (1 differing position) or a transposition
        // (2 adjacent, swapped). Anything with more differing positions is
        // multiple independent edits — not a single motor error.
        (a, b) if a == b => {
            let diffs: Vec<usize> = (0..a).filter(|&i| t[i] != g[i]).collect();
            match diffs.len() {
                1 => {
                    let i = diffs[0];
                    // Substitution only counts if the two keys are physically
                    // adjacent — the fat-finger fingerprint. A far-apart swap is
                    // a different intended word, not a slip.
                    keyboard::are_adjacent(t[i], g[i]).then_some(MotorEdit::AdjacentSubstitution)
                }
                2 => {
                    let (i, j) = (diffs[0], diffs[1]);
                    (j == i + 1 && t[i] == g[j] && t[j] == g[i]).then_some(MotorEdit::Transposition)
                }
                _ => None,
            }
        }
        // Target one longer: a single dropped letter — but only if the edit is a
        // *pure* one-character insertion (distance 1), not an insertion riding
        // alongside a substitution (`so → for` is a sub + an insert).
        (a, b) if a + 1 == b => (levenshtein(&t, &g) == 1).then_some(MotorEdit::DroppedLetter),
        // Source one longer: a single extra letter, same pure-indel requirement.
        (a, b) if a == b + 1 => (levenshtein(&t, &g) == 1).then_some(MotorEdit::ExtraLetter),
        // Length differs by more than one: never a single motor error.
        _ => None,
    }
}

/// Length-scaled generalisation of [`single_motor_edit`]: admit a `typed →
/// target` pair when the target is reachable from the source by **at most
/// `budget` motor-class operations** — each one an adjacent-key substitution, an
/// adjacent transposition, a single insertion, or a single deletion — and return
/// the minimum such operation count. `None` if it can't be done within `budget`
/// using only motor-class ops (e.g. it needs a *non-adjacent* substitution — the
/// fingerprint of a word-swap / "different word", never a slip of the hand).
///
/// This is the **shadow-curve** nomination filter: the M3 brief's per-length
/// motor-edit budget (short words 1, longer words up to 2). It is deliberately
/// kept *separate* from [`single_motor_edit`] (budget fixed at 1) so the live
/// Impact ledger and the KILL_SWITCH_CLASSIFY debug line are unchanged; only the
/// shadow dry-run reads this. At `budget == 1` it agrees with
/// [`single_motor_edit`] on letter pairs (an adjacency-restricted edit distance
/// of 1 is exactly one of the four single-motor shapes).
///
/// The same letter-only guard as [`single_motor_edit`] / [`classify_slip`]
/// applies, so apostrophe / punctuation / casing edits fall through to `None`.
/// Non-adjacent substitutions are disallowed (modelled as an over-budget cost),
/// so a pair that genuinely needs one is rejected rather than counted as a slip.
pub fn motor_edit_within_budget(typed: &str, target: &str, budget: usize) -> Option<usize> {
    let t: Vec<char> = typed.chars().collect();
    let g: Vec<char> = target.chars().collect();
    if t == g {
        return None; // no edit at all
    }
    if !is_letter_motor_edit(&t, &g) {
        return None; // apostrophe / punctuation / casing — not a motor slip
    }
    let d = restricted_damerau(&t, &g, budget);
    (d > 0 && d <= budget).then_some(d)
}

/// Generate every string reachable from `typed` by **exactly one** motor-class
/// edit — an adjacent-key substitution, an adjacent transposition, a single
/// insertion, or a single deletion. This is the generative inverse of
/// [`motor_edit_within_budget`] at `budget == 1`.
///
/// **Superset guarantee (the load-bearing property).** For any `w`,
/// `motor_edit_within_budget(typed, w, 1) == Some(1)` ⇒ `w` is produced here.
/// Proof sketch: a restricted-Damerau distance of exactly 1 means `w` differs
/// from `typed` by precisely one of the four cost-1 operations the metric
/// allows (adjacent substitution, adjacent transposition, insertion, deletion),
/// and this enumerates all four exhaustively — every adjacent key for each
/// substitution position, every letter for each insertion gap, every swap, every
/// deletion. So no in-budget candidate is ever missed.
///
/// The reverse does **not** hold: this may emit strings that are *not* valid
/// motor edits (e.g. an inserted letter that, combined with `typed`, trips the
/// `is_letter_motor_edit` guard, or a regenerated copy of `typed`). Callers MUST
/// re-validate each candidate with [`motor_edit_within_budget`]; superset
/// generation + exact re-validation yields precisely the same set as scanning the
/// whole dictionary, but at a cost that scales with the word, not the lexicon.
///
/// Substitutions/insertions only ever use ASCII letters `a..=z`: the metric's
/// `is_letter_motor_edit` guard rejects any edit whose changed span touches a
/// non-letter, so a non-letter candidate could never validate anyway. Duplicates
/// are possible; collect into a set if uniqueness matters.
pub fn motor_edit_neighborhood(typed: &str) -> Vec<String> {
    let t: Vec<char> = typed.chars().collect();
    let n = t.len();
    let mut out: Vec<String> = Vec::new();
    // No early-out for the empty string: its only in-budget edits are single-char
    // insertions, which the insertion loop below (`0..=0`) generates correctly —
    // matching the full scan's single-letter candidates. (Production never feeds
    // an empty token here; the parity helpers must still agree for all inputs.)
    let alpha = || 'a'..='z';

    // Adjacent-key substitutions: replace t[i] with each physically adjacent key.
    for i in 0..n {
        for c in alpha() {
            if keyboard::are_adjacent(t[i], c) {
                let mut w = t.clone();
                w[i] = c;
                out.push(w.into_iter().collect());
            }
        }
    }
    // Adjacent transpositions: swap t[i] and t[i+1] when they differ (a swap of
    // equal chars is a no-op, never a distinct candidate).
    for i in 0..n.saturating_sub(1) {
        if t[i] != t[i + 1] {
            let mut w = t.clone();
            w.swap(i, i + 1);
            out.push(w.into_iter().collect());
        }
    }
    // Single insertions: insert each letter at every gap (0..=n).
    for i in 0..=n {
        for c in alpha() {
            let mut w = t.clone();
            w.insert(i, c);
            out.push(w.into_iter().collect());
        }
    }
    // Single deletions: drop each position.
    for i in 0..n {
        let mut w = t.clone();
        w.remove(i);
        out.push(w.into_iter().collect());
    }
    out
}

/// Damerau–Levenshtein distance where a **substitution counts only between
/// physically adjacent keys** (a non-adjacent substitution is modelled as cost
/// `budget + 1`, i.e. never chosen within budget — it is not a motor error) and
/// an **adjacent transposition** costs 1. Insertions/deletions cost 1. Bounds the
/// search to `budget`: any cell that can only exceed the budget is irrelevant, so
/// the returned value is meaningful exactly when `≤ budget`. Words are short, so
/// the simple full O(n·m) table is ample.
fn restricted_damerau(t: &[char], g: &[char], budget: usize) -> usize {
    let n = t.len();
    let m = g.len();
    // A cost that stands in for "disallowed / past budget" without overflow.
    let blocked = budget + 1;
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub_cost = if t[i - 1] == g[j - 1] {
                0
            } else if keyboard::are_adjacent(t[i - 1], g[j - 1]) {
                1
            } else {
                blocked // non-adjacent substitution is not a motor error
            };
            let mut best = (d[i - 1][j - 1] + sub_cost)
                .min(d[i - 1][j] + 1) // deletion
                .min(d[i][j - 1] + 1); // insertion
            // Adjacent transposition (Damerau): the two swapped chars cost 1.
            if i >= 2 && j >= 2 && t[i - 1] == g[j - 2] && t[i - 2] == g[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[n][m]
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

/// True iff the change from `t` (typed) to `g` (target) is a mis-hit of *letter*
/// keys — the only thing a motor slip can be. False when the edit only changes
/// letter case (`The → the`) or its changed span touches a non-letter (an
/// apostrophe/punctuation/digit — `dont → don't`): those are grammar/format
/// fixes, not motor slips. The changed span is the same longest-common
/// prefix/suffix diff [`involved_keys`] / [`corrected_target_indices`] use, so
/// the three agree on which characters a correction touched.
fn is_letter_motor_edit(t: &[char], g: &[char]) -> bool {
    // Pure casing change — same letters, only case differs — is not a motor slip.
    if t.len() == g.len() && t.iter().zip(g).all(|(a, b)| a.eq_ignore_ascii_case(b)) {
        return false;
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
    // Every character the edit touches, on either side, must be a letter.
    t[prefix..t.len() - suffix]
        .iter()
        .chain(g[prefix..g.len() - suffix].iter())
        .all(|c| c.is_ascii_alphabetic())
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
    fn apostrophe_or_punctuation_fix_is_not_a_motor_slip() {
        // Adding a missing apostrophe is a grammar fix (autocorrect's domain),
        // not a mis-hit of letter keys — even though the edit distance is 1.
        assert_eq!(classify_slip("dont", "don't"), None);
        assert_eq!(classify_slip("cant", "can't"), None);
        assert_eq!(classify_slip("wont", "won't"), None);
    }

    #[test]
    fn casing_fix_is_not_a_motor_slip() {
        assert_eq!(classify_slip("the", "The"), None);
        assert_eq!(classify_slip("iphone", "iPhone"), None);
    }

    #[test]
    fn letter_only_dropped_key_stays_precision() {
        // Genuine dropped-LETTER slips remain motor (the s→is / f→of / ad→and
        // class) — only non-letter/casing edits are filtered out.
        assert_eq!(classify_slip("s", "is"), Some(SlipClass::Precision));
        assert_eq!(classify_slip("f", "of"), Some(SlipClass::Precision));
        assert_eq!(classify_slip("ad", "and"), Some(SlipClass::Precision));
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

    // ---- single_motor_edit — the strict nomination filter ----------------

    #[test]
    fn single_motor_edit_admits_each_single_error_shape() {
        // adjacent-key substitution: p/o are neighbours on QWERTY.
        assert_eq!(
            single_motor_edit("wprd", "word"),
            Some(MotorEdit::AdjacentSubstitution)
        );
        // o/l are neighbours too (the couod → could canonical).
        assert_eq!(
            single_motor_edit("couod", "could"),
            Some(MotorEdit::AdjacentSubstitution)
        );
        // adjacent transposition.
        assert_eq!(
            single_motor_edit("teh", "the"),
            Some(MotorEdit::Transposition)
        );
        // one dropped letter.
        assert_eq!(
            single_motor_edit("wrd", "word"),
            Some(MotorEdit::DroppedLetter)
        );
        assert_eq!(
            single_motor_edit("te", "the"),
            Some(MotorEdit::DroppedLetter)
        );
        // one extra letter (doubling).
        assert_eq!(
            single_motor_edit("worrd", "word"),
            Some(MotorEdit::ExtraLetter)
        );
    }

    #[test]
    fn single_motor_edit_rejects_non_adjacent_substitution() {
        // q and m are far apart — substituting one for the other is a different
        // intended word, not a fat-finger slip.
        assert_eq!(single_motor_edit("qame", "mame"), None);
        // dab → bad: two non-adjacent substitutions (dist 2) — classify_slip
        // calls this Precision, but it is NOT a single motor error.
        assert_eq!(classify_slip("dab", "bad"), Some(SlipClass::Precision));
        assert_eq!(single_motor_edit("dab", "bad"), None);
    }

    #[test]
    fn single_motor_edit_drops_word_swaps_that_are_not_near_misses() {
        // The exact word-swaps the brief calls out — a substitution PLUS an
        // insertion, two edits, never one slip of the hand.
        assert_eq!(single_motor_edit("so", "for"), None);
        assert_eq!(single_motor_edit("to", "for"), None);
        assert_eq!(single_motor_edit("of", "for"), None);
    }

    #[test]
    fn neighborhood_is_a_superset_of_every_in_budget_edit() {
        // The load-bearing property the convergence-scan optimisation rests on:
        // anything one motor edit away from `typed` is produced by
        // `motor_edit_neighborhood`. Check it directly across all four shapes for
        // a spread of words, including the live repro cases.
        use std::collections::HashSet;
        for typed in [
            "teh",
            "antiicipated",
            "anticiipated",
            "whcih",
            "word",
            "a",
            "recieve",
            "xq",
        ] {
            let nbr: HashSet<String> = motor_edit_neighborhood(typed).into_iter().collect();
            let t: Vec<char> = typed.chars().collect();
            // Reconstruct every in-budget edit independently and assert membership.
            for i in 0..t.len() {
                // adjacent substitutions
                for c in 'a'..='z' {
                    if keyboard::are_adjacent(t[i], c) {
                        let mut w = t.clone();
                        w[i] = c;
                        let w: String = w.into_iter().collect();
                        assert!(
                            nbr.contains(&w),
                            "missing adjacent-sub {w:?} of {typed:?}"
                        );
                    }
                }
                // deletions
                let mut w = t.clone();
                w.remove(i);
                let w: String = w.into_iter().collect();
                assert!(nbr.contains(&w), "missing deletion {w:?} of {typed:?}");
            }
            // transpositions
            for i in 0..t.len().saturating_sub(1) {
                if t[i] != t[i + 1] {
                    let mut w = t.clone();
                    w.swap(i, i + 1);
                    let w: String = w.into_iter().collect();
                    assert!(nbr.contains(&w), "missing transposition {w:?} of {typed:?}");
                }
            }
            // insertions
            for i in 0..=t.len() {
                for c in 'a'..='z' {
                    let mut w = t.clone();
                    w.insert(i, c);
                    let w: String = w.into_iter().collect();
                    assert!(nbr.contains(&w), "missing insertion {w:?} of {typed:?}");
                }
            }
        }
    }

    #[test]
    fn single_motor_edit_rejects_non_motor_edits() {
        // apostrophe / punctuation / casing — not motor slips (shared with
        // classify_slip's letter-only guard).
        assert_eq!(single_motor_edit("dont", "don't"), None);
        assert_eq!(single_motor_edit("the", "The"), None);
        // identical.
        assert_eq!(single_motor_edit("word", "word"), None);
        // length diff > 1.
        assert_eq!(single_motor_edit("wd", "word"), None);
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
        assert_eq!(
            corrected_target_indices("worrd", "word"),
            Vec::<usize>::new()
        );
        // Identical pair: nothing to mark.
        assert_eq!(
            corrected_target_indices("word", "word"),
            Vec::<usize>::new()
        );
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

    // ---- motor_edit_within_budget — the length-scaled shadow filter -------

    #[test]
    fn budget_one_agrees_with_single_motor_edit() {
        // On letter pairs, an adjacency-restricted distance of 1 is exactly one
        // of the four single-motor shapes, so budget 1 ≡ single_motor_edit.is_some().
        for (t, g) in [
            ("wprd", "word"),   // adjacent sub
            ("couod", "could"), // adjacent sub
            ("teh", "the"),     // transposition
            ("wrd", "word"),    // dropped letter
            ("worrd", "word"),  // extra letter
            ("qame", "mame"),   // non-adjacent sub → both None
            ("dab", "bad"),     // two non-adjacent subs → both None
            ("so", "for"),      // word-swap → both None
            ("dont", "don't"),  // apostrophe → both None
            ("the", "The"),     // casing → both None
            ("word", "word"),   // identical → both None
            ("wd", "word"),     // len diff 2 → both None
        ] {
            assert_eq!(
                motor_edit_within_budget(t, g, 1).is_some(),
                single_motor_edit(t, g).is_some(),
                "budget-1 disagreement on {t}->{g}"
            );
        }
    }

    #[test]
    fn budget_two_admits_two_adjacent_motor_errors() {
        // Two adjacent-key substitutions on a longer word: rejected at budget 1,
        // admitted at budget 2 (distance 2).
        assert_eq!(motor_edit_within_budget("wprf", "word", 1), None);
        assert_eq!(motor_edit_within_budget("wprf", "word", 2), Some(2));
        // A transposition plus a dropped letter (two motor ops).
        assert_eq!(motor_edit_within_budget("teh", "the", 2), Some(1));
    }

    #[test]
    fn budget_two_still_rejects_word_swaps_and_far_subs() {
        // Even with budget 2, a non-adjacent substitution is not a motor error,
        // so word-swaps / different-word pairs stay rejected.
        assert_eq!(motor_edit_within_budget("dab", "bad", 2), None);
        assert_eq!(motor_edit_within_budget("cat", "dog", 2), None);
        // Apostrophe / casing remain non-motor at any budget.
        assert_eq!(motor_edit_within_budget("dont", "don't", 2), None);
        assert_eq!(motor_edit_within_budget("the", "The", 2), None);
        // Identical is never an edit.
        assert_eq!(motor_edit_within_budget("word", "word", 2), None);
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
