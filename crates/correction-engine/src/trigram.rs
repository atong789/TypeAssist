//! Letter-trigram plausibility — a cheap "does this look like a real word"
//! gate for the pattern-recording guard (Guard 1).
//!
//! Built once from the bundled clean lexicon (~81k words): a trigram is
//! *attested* if it occurs in at least one dictionary word. A target is
//! **trigram-plausible** when *every* one of its letter-trigrams is attested.
//!
//! ## Why "all attested", not a fraction
//!
//! The artifacts this guards against are a real word with a junk tail glued
//! on — `obstacle` + `edndd` → `obstacleedndd`. Measured against the real
//! dictionary, those score a *high* trigram-present fraction (0.85–0.92): the
//! junk is only a couple of bad trigrams diluted by a real stem, so a
//! fraction floor is fooled (a junk target like `predndd` scores 0.80, higher
//! than the real name `soumyo` at 0.75 — they are not fraction-separable).
//!
//! The one clean signal is the **truly-impossible trigram**. Every member of
//! the `edndd`/`dndd` family contains `dnd` (or `fae`), which appears in
//! **zero** of the 81,478 dictionary words. Requiring *all* trigrams to be
//! attested rejects the whole family.
//!
//! ## The trade-off (documented, intentional)
//!
//! A genuinely real word that happens to contain a trigram absent from the
//! bundled dictionary (e.g. `soumyo`, `kubernetes`) is **not** trigram-
//! plausible, so its `typed → target` correction pairs aren't recorded *until
//! the word is `is_known`*. That does NOT block learning the word: the
//! lexicon proposer learns vocabulary from the user typing the word
//! *correctly* (a separate path from this correction-pair store), and once it
//! is learned `is_known` short-circuits the guard. So "it learns your words"
//! holds — the guard only defers learning *corrections toward* an unattested,
//! not-yet-known target, which is the conservative and correct stance.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::Lexicon;

/// Set of letter trigrams attested in the bundled clean dictionary.
pub struct TrigramModel {
    seen: HashSet<[char; 3]>,
}

impl TrigramModel {
    fn build() -> Self {
        let mut seen = HashSet::new();
        for word in Lexicon::shared().iter_clean() {
            for tri in trigrams(word) {
                seen.insert(tri);
            }
        }
        Self { seen }
    }

    /// Process-wide model, built once (lazily) from the bundled lexicon.
    pub fn shared() -> &'static TrigramModel {
        static MODEL: OnceLock<TrigramModel> = OnceLock::new();
        MODEL.get_or_init(TrigramModel::build)
    }

    /// True iff `word` looks phonotactically like English: a word shorter
    /// than 3 letters is too short to judge and always passes (`is_known`
    /// carries the load there); otherwise every one of its letter-trigrams
    /// must be attested in the bundled dictionary.
    pub fn plausible(&self, word: &str) -> bool {
        let tris = trigrams(word);
        if tris.is_empty() {
            return true; // < 3 letters: undecidable, defer to is_known
        }
        tris.iter().all(|t| self.seen.contains(t))
    }
}

/// Lowercased ASCII-letter trigrams of `word` (apostrophes/hyphens/digits
/// dropped, so `don't` and `well-known` reduce to their letter runs).
fn trigrams(word: &str) -> Vec<[char; 3]> {
    let letters: Vec<char> = word
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    letters.windows(3).map(|w| [w[0], w[1], w[2]]).collect()
}

/// Guard 1 (soft): a `typed → target` pair is recordable only if the
/// intended `target` is a word the engine knows (bundled dict OR the user's
/// learned set) OR it is letter-trigram plausible. Rejects the `"edndd"`-tail
/// re-tokenization artifacts (each carries a dictionary-absent trigram)
/// without blocking genuine vocabulary — a learned word is `is_known`, and a
/// fully-attested real word clears the trigram floor.
pub fn target_is_recordable(target: &str, lexicon: &Lexicon) -> bool {
    lexicon.is_known(target) || TrigramModel::shared().plausible(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex() -> &'static Lexicon {
        Lexicon::shared()
    }

    #[test]
    fn edndd_artifact_family_is_rejected() {
        // The whole artifact family — bare tails, real-stem-plus-tail, and
        // the merge form. Each carries the dictionary-absent trigram `dnd`
        // (or `fae`), so none is trigram-plausible; none is is_known either.
        for w in &[
            "edndd",
            "faedndd",
            "predndd",
            "obstacledndd",
            "obstacleedndd",
            "obstaclesedndd",
            "wasedndd",
            "liedndd",
            "throughedndd",
            "monthedndd",
        ] {
            assert!(!lex().is_known(w), "test premise: {w:?} must be unknown");
            assert!(
                !target_is_recordable(w, lex()),
                "{w:?} is an artifact and must be rejected"
            );
        }
    }

    #[test]
    fn known_and_learned_words_pass_via_is_known() {
        for w in &["word", "obstacles", "the", "because"] {
            assert!(
                target_is_recordable(w, lex()),
                "{w:?} is a real word and must be recordable"
            );
        }
        // A learned (user-taught) word passes even though it's not in the
        // bundled dict — "it learns your words".
        let made_up = "zorptesting";
        lex().learn(made_up);
        assert!(lex().is_known(made_up));
        assert!(target_is_recordable(made_up, lex()));
        lex().unlearn(made_up);
    }

    #[test]
    fn attested_real_names_and_jargon_pass() {
        // Not-yet-learned, not in the bundled dict, but every trigram is
        // attested — must clear the floor so the user can build patterns
        // toward learning them. (If any happen to be in the dict, the
        // assertion still holds via is_known.)
        for w in &[
            "sinha",
            "anthropic",
            "mbappe",
            "blorp",
            "namaste",
            "krutrim",
            "svelte",
        ] {
            assert!(
                target_is_recordable(w, lex()),
                "{w:?} should be recordable (fully-attested phonotactics)"
            );
        }
    }

    #[test]
    fn unattested_real_word_is_gated_until_learned() {
        // The documented trade-off: a real word with a dictionary-absent
        // trigram (`oum` in "soumyo") is NOT trigram-plausible, so its
        // correction pairs are deferred — UNTIL it's learned as vocabulary
        // (the proposer's job), after which is_known admits it. This never
        // blocks learning the word itself.
        let name = "soumyo";
        assert!(!lex().is_known(name));
        assert!(
            !TrigramModel::shared().plausible(name),
            "test premise: has an unattested trigram"
        );
        assert!(!target_is_recordable(name, lex()), "deferred while unknown");
        lex().learn(name);
        assert!(target_is_recordable(name, lex()), "admitted once learned");
        lex().unlearn(name);
    }

    #[test]
    fn short_words_defer_to_is_known() {
        // < 3 letters can't be judged by trigrams; plausible() passes them
        // and the recordable gate leans on is_known.
        assert!(TrigramModel::shared().plausible("a"));
        assert!(TrigramModel::shared().plausible("of"));
    }
}
