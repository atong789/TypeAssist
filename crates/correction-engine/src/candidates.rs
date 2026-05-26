//! Candidate generation — Component 3b of the L4 brief.
//!
//! Norvig-style edit-distance-1 generator over `a–z`, filtered to the
//! lexicon ([`crate::Lexicon`]) and ranked by raw unigram frequency. **Still
//! read-only**: no confidence score, no motor/spatial weighting, no
//! tier-aware bias, no correction or injection. Component 3c (the confidence
//! score) will consume `ranked_known_candidates` and apply the volatility
//! map.
//!
//! ## Scope
//!
//! * Only unknown words get candidates. Known words are *protected* — we
//!   never propose changing a word the bundled lexicon recognises (that's
//!   how `is_known` "fences off" the user's vocabulary today, until the
//!   live-learning overlay arrives).
//! * Candidate generation is standard Norvig edit-1: **deletes, transposes,
//!   replaces over `a..=z`, inserts over `a..=z` at every position**. Spatial
//!   bias and per-finger priors come in 3c.
//! * Filter to known: a candidate survives iff [`Lexicon::is_known`] is
//!   true. Unknown candidates are discarded — they can't be evidence for
//!   anything yet.
//! * Rank by frequency desc, with an alphabetical tiebreak so two
//!   equal-frequency candidates surface in deterministic order (the
//!   `seed_proper_nouns` collide at frequency = 1, for instance).
//!
//! ## Versioning
//!
//! Bump [`CANDIDATES_VERSION`] on any change to the generator or ranker.
//! Same contract as [`crate::LEXICON_VERSION`] / [`crate::TOKENIZER_VERSION`].
//!
//! ## Lexicon v2: membership is now spelling-clean
//!
//! The clean spelling dictionary (SCOWL en_US + contractions) gates
//! membership; the Norvig web-frequency table only ranks candidates of
//! unknown words. Two behaviours this guarantees:
//!
//! * `teh` is unknown → its edit-1 set yields known neighbours; **`the`
//!   tops the ranking by ~23B Norvig count.**
//! * `asdf` is unknown → its edit-1 set has no clean-dict members; the
//!   honest "(no known candidates within edit-1)" outcome holds.
//!
//! The old v1 corpus-quirks notes (typos passing as known, web noise
//! posing as candidates) are resolved by the lexicon split; see
//! [`crate::lexicon`] module docs.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::Lexicon;

/// Version of the candidate generator + ranker.
pub const CANDIDATES_VERSION: u32 = 1;

/// A known-lexicon candidate together with its raw unigram frequency.
/// The pair the panel renders directly; the score / tier comes later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownCandidate {
    pub word: String,
    pub frequency: u64,
}

/// Generate the full Norvig edit-1 set for `word`, deduplicated.
///
/// Lowercases the input first (the lexicon is lowercase, and so are the
/// generated edits). The input itself is excluded from the result.
///
/// For a length-`n` ASCII word the set has at most `n` deletes,
/// `n.saturating_sub(1)` transposes, `25*n` replaces, and `26*(n+1)`
/// inserts — `~54n + 25` before dedup. Cheap for word-length inputs.
pub fn edit1(word: &str) -> Vec<String> {
    let lower = word.to_ascii_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let n = chars.len();
    let mut out: HashSet<String> = HashSet::with_capacity(64 * n.max(1));

    // Deletes: drop each character.
    for i in 0..n {
        let mut s = String::with_capacity(n.saturating_sub(1));
        s.extend(chars[..i].iter());
        s.extend(chars[i + 1..].iter());
        out.insert(s);
    }

    // Transposes: swap each adjacent pair.
    for i in 0..n.saturating_sub(1) {
        let mut s = String::with_capacity(n);
        s.extend(chars[..i].iter());
        s.push(chars[i + 1]);
        s.push(chars[i]);
        s.extend(chars[i + 2..].iter());
        out.insert(s);
    }

    // Replaces: substitute each position with every other a-z.
    for i in 0..n {
        for c in b'a'..=b'z' {
            let c = c as char;
            if c == chars[i] {
                continue;
            }
            let mut s = String::with_capacity(n);
            s.extend(chars[..i].iter());
            s.push(c);
            s.extend(chars[i + 1..].iter());
            out.insert(s);
        }
    }

    // Inserts: insert every a-z at every position (0..=n).
    for i in 0..=n {
        for c in b'a'..=b'z' {
            let mut s = String::with_capacity(n + 1);
            s.extend(chars[..i].iter());
            s.push(c as char);
            s.extend(chars[i..].iter());
            out.insert(s);
        }
    }

    // The input itself may have round-tripped (insert+delete identity) —
    // drop it so we never propose the user's own word back to them.
    out.remove(&lower);

    out.into_iter().collect()
}

/// Generate edit-1 candidates, filter to known words, and rank by frequency
/// descending (alphabetical tiebreak for determinism). Empty result on:
///
/// * `word` is empty
/// * `word` is itself in the lexicon (known words are protected — see
///   module docs)
/// * no edit-1 neighbour is in the lexicon (legitimate non-word inputs
///   like `asdf` — the brief calls this out: "(no known candidates within
///   edit-1)" must be a real outcome, not a forced suggestion)
pub fn ranked_known_candidates(
    word: &str,
    lexicon: &Lexicon,
    limit: usize,
) -> Vec<KnownCandidate> {
    if word.is_empty() || lexicon.is_known(word) || limit == 0 {
        return Vec::new();
    }
    let mut hits: Vec<KnownCandidate> = edit1(word)
        .into_iter()
        .filter_map(|cand| {
            // Membership gates inclusion; frequency only ranks.
            // Under lexicon v2 these are different sets — a candidate
            // can have a nonzero Norvig count yet not be a real word
            // (e.g. `asf` / `adf` / `sdf` as edit-1 of `asdf`). We must
            // not propose those, even though they have web counts.
            if !lexicon.is_known(&cand) {
                return None;
            }
            Some(KnownCandidate {
                frequency: lexicon.frequency(&cand),
                word: cand,
            })
        })
        .collect();
    // Frequency desc, then alphabetical asc — deterministic order across
    // runs and across the OS HashMap iteration nondeterminism inside `edit1`.
    hits.sort_by(|a, b| b.frequency.cmp(&a.frequency).then(a.word.cmp(&b.word)));
    hits.truncate(limit);
    hits
}

// Sanity at compile time — matches the LEXICON_VERSION pattern.
const _: () = assert!(CANDIDATES_VERSION >= 1);

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn lex() -> &'static Lexicon {
        Lexicon::shared()
    }

    // ---- edit1 generator -------------------------------------------------

    #[test]
    fn edit1_does_not_include_the_input_itself() {
        let edits = edit1("the");
        assert!(
            !edits.iter().any(|s| s == "the"),
            "edit1('the') leaked the input itself"
        );
    }

    #[test]
    fn edit1_includes_each_kind_of_neighbour() {
        let edits: HashSet<String> = edit1("the").into_iter().collect();
        // Delete: "he", "te", "th"
        assert!(edits.contains("he"));
        assert!(edits.contains("te"));
        assert!(edits.contains("th"));
        // Transpose: "hte", "teh"
        assert!(edits.contains("hte"));
        assert!(edits.contains("teh"));
        // Replace one position (`t` -> `s` -> "she")
        assert!(edits.contains("she"));
        // Insert at end (e.g. "they")
        assert!(edits.contains("they"));
        // Insert at start (e.g. "athe")
        assert!(edits.contains("athe"));
    }

    #[test]
    fn edit1_count_is_roughly_54n_plus_25_after_dedup() {
        // Loose upper bound — exact count depends on dedup collisions.
        // For "the" (n=3): 3 deletes + 2 transposes + 75 replaces + 104
        // inserts = 184 before dedup; pin a generous range.
        let edits = edit1("the");
        assert!(
            (150..=200).contains(&edits.len()),
            "edit1('the').len() = {} outside expected range",
            edits.len()
        );
    }

    #[test]
    fn edit1_is_deterministic_in_set_content() {
        // Vec order isn't guaranteed (HashSet under the hood), but the *set*
        // must be identical across runs.
        let a: HashSet<String> = edit1("hello").into_iter().collect();
        let b: HashSet<String> = edit1("hello").into_iter().collect();
        assert_eq!(a, b);
    }

    #[test]
    fn edit1_empty_input_yields_a_few_single_letter_inserts() {
        // Special case: n=0. Only the 26 single-letter inserts survive
        // (deletes/transposes/replaces have nothing to act on). The
        // self-input "" is removed at the end, so we expect exactly 26.
        let edits = edit1("");
        assert_eq!(edits.len(), 26);
        assert!(edits.iter().any(|s| s == "a"));
        assert!(edits.iter().any(|s| s == "z"));
    }

    #[test]
    fn edit1_lowercases_the_input() {
        // "THE" should produce the same set as "the" — the lexicon is
        // lowercase and so are the generated candidates.
        let upper: HashSet<String> = edit1("THE").into_iter().collect();
        let lower: HashSet<String> = edit1("the").into_iter().collect();
        assert_eq!(upper, lower);
    }

    // ---- ranked_known_candidates ----------------------------------------

    #[test]
    fn known_word_is_protected_no_candidates() {
        // "the" is in the bundled list — we never propose changing it.
        let cands = ranked_known_candidates("the", lex(), 5);
        assert!(
            cands.is_empty(),
            "known word should be protected, got {cands:?}"
        );
    }

    #[test]
    fn teh_proposes_the_at_the_top() {
        // The canonical example from the brief. Works under lexicon v2:
        // `teh` is not in the clean SCOWL dict, so it reaches candidate
        // generation; `the` wins by ~23B Norvig count over the other
        // clean-dict edit-1 neighbours (`eh`, `meh`, `tea`, `tee`, `ten`).
        let cands = ranked_known_candidates("teh", lex(), 5);
        assert!(
            !cands.is_empty(),
            "expected at least one candidate for 'teh'"
        );
        assert_eq!(
            cands[0].word, "the",
            "expected 'the' to top the ranking, got {cands:?}"
        );
        assert!(cands[0].frequency > 1_000_000_000);
    }

    #[test]
    fn recieve_proposes_receive() {
        // Brief's other canonical example: classic i-before-e typo.
        // Clean-dict edit-1 neighbours are `receive` (transpose) and
        // `relieve` (replace 'c'→'l'); `receive` wins on frequency.
        let cands = ranked_known_candidates("recieve", lex(), 5);
        assert!(!cands.is_empty(), "expected at least one candidate for 'recieve'");
        assert_eq!(
            cands[0].word, "receive",
            "expected 'receive' to top the ranking, got {cands:?}"
        );
    }

    #[test]
    fn ranking_is_strictly_frequency_descending() {
        let cands = ranked_known_candidates("teh", lex(), 10);
        for pair in cands.windows(2) {
            assert!(
                pair[0].frequency >= pair[1].frequency,
                "ranking broken: {pair:?}"
            );
        }
    }

    #[test]
    fn asdf_has_no_known_candidates() {
        // The brief's example. Works under lexicon v2: the web-noise
        // neighbours (`asf`, `adf`, `sdf`, `asd`) that polluted v1 are
        // NOT in the SCOWL clean dict, so `asdf` correctly surfaces
        // "(no known candidates within edit-1)".
        let cands = ranked_known_candidates("asdf", lex(), 5);
        assert!(
            cands.is_empty(),
            "asdf should have no clean-dict edit-1 neighbours, got {cands:?}"
        );
    }

    #[test]
    fn nonsense_consonant_string_has_no_candidates() {
        // Belt-and-suspenders against `asdf`'s specific neighbours
        // re-entering some future dict: a string of low-frequency
        // letters is statistically guaranteed to have no clean-dict
        // edit-1 neighbour.
        let cands = ranked_known_candidates("qzxjvk", lex(), 5);
        assert!(
            cands.is_empty(),
            "expected zero candidates for 'qzxjvk', got {cands:?}"
        );
    }

    #[test]
    fn empty_word_returns_empty() {
        assert!(ranked_known_candidates("", lex(), 5).is_empty());
    }

    #[test]
    fn limit_truncates_after_ranking_not_before() {
        // Pulling the top-1 must give the SAME #1 as pulling the top-10's
        // first row — proves we sort, then truncate, never the reverse.
        let top10 = ranked_known_candidates("teh", lex(), 10);
        let top1 = ranked_known_candidates("teh", lex(), 1);
        assert_eq!(top1.len(), 1);
        assert_eq!(top1[0], top10[0]);
    }

    #[test]
    fn zero_limit_returns_empty() {
        assert!(ranked_known_candidates("teh", lex(), 0).is_empty());
    }

    #[test]
    fn results_are_deterministic_across_runs() {
        // Equal-frequency candidates get an alphabetical tiebreak so the
        // HashMap iteration order inside `edit1` never leaks into the
        // emitted ranking.
        let a = ranked_known_candidates("teh", lex(), 10);
        let b = ranked_known_candidates("teh", lex(), 10);
        assert_eq!(a, b);
    }

    #[test]
    fn case_insensitive_input() {
        // Mixed-case unknown input should produce the same ranking as the
        // lowercase form — the engine passes the token's case-preserved
        // core through, the candidates module shouldn't care.
        let upper = ranked_known_candidates("TEH", lex(), 5);
        let lower = ranked_known_candidates("teh", lex(), 5);
        assert_eq!(upper, lower);
    }
}
