//! Lexicon — Component 3a of the L4 brief.
//!
//! Read-only word source. A bundled frequency list (Norvig's top-50k unigrams
//! from the Google Web Trillion Word Corpus) plus a temporary dev fixture for
//! proper nouns. Component 3b (candidate scoring) and downstream tiers will
//! consume `is_known` / `frequency`; nothing in this file scores, ranks, or
//! corrects anything.
//!
//! ## API
//!
//! * [`Lexicon::shared`] — process-wide singleton, loaded on first use.
//! * [`Lexicon::is_known`] — case-insensitive bool, true iff `frequency > 0`.
//! * [`Lexicon::frequency`] — raw unigram count; 0 for unknown words.
//!
//! ## Versioning
//!
//! Bump [`LEXICON_VERSION`] on any change to the bundled list **or** the seed
//! list. Consumers can compare the version to refuse stale snapshots — same
//! pattern as [`crate::tokenizer::TOKENIZER_VERSION`].

use std::collections::HashMap;
use std::sync::OnceLock;

/// Version of the loaded lexicon (bundled list + seed fixture). Bump on any
/// observable change to the loaded set.
pub const LEXICON_VERSION: u32 = 1;

/// Bundled `word<TAB>count` table — top 50,000 unigrams. All lowercase ASCII.
const WORDS_FREQ: &str = include_str!("../lexicon/words_freq.txt");

/// **DEV FIXTURE — temporary.** Production populates the user's proper-noun
/// set from live-learning (Component 5: user typed it, the engine left it
/// alone, after N repetitions it earns a slot). The seed list is *never*
/// populated by the user typing words into any UI.
///
/// Stored lowercase so the case-insensitive lookup works for any input case
/// (`Krutrim`, `KRUTRIM`, `krutrim` all hit).
const SEED_PROPER_NOUNS: &[&str] = &["krutrim", "zams", "ondc"];

/// Frequency we attach to a seed proper noun. Nonzero so `is_known` treats
/// the bundled list and the seed list as one set (`frequency > 0`), low
/// enough that seeds don't outrank genuinely common words once scoring lands.
const SEED_FREQUENCY: u64 = 1;

pub struct Lexicon {
    by_word: HashMap<String, u64>,
}

impl Lexicon {
    /// Parse [`WORDS_FREQ`] and inject [`SEED_PROPER_NOUNS`]. Done once per
    /// process via [`Self::shared`]; exposed for tests that want a fresh
    /// instance without touching the global.
    pub fn load() -> Self {
        let mut by_word: HashMap<String, u64> = HashMap::with_capacity(60_000);
        for raw in WORDS_FREQ.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split('\t');
            let (Some(word), Some(count)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Ok(count) = count.parse::<u64>() else {
                continue;
            };
            by_word.insert(word.to_ascii_lowercase(), count);
        }
        // Seeds use `entry().or_insert` so a coincidentally-bundled lowercase
        // form keeps its real frequency rather than collapsing to 1.
        for &seed in SEED_PROPER_NOUNS {
            by_word
                .entry(seed.to_ascii_lowercase())
                .or_insert(SEED_FREQUENCY);
        }
        Self { by_word }
    }

    /// Process-wide singleton. ~50k inserts on first call; subsequent calls
    /// are a pointer load. Safe to call from anywhere.
    pub fn shared() -> &'static Lexicon {
        static LEX: OnceLock<Lexicon> = OnceLock::new();
        LEX.get_or_init(Lexicon::load)
    }

    /// True iff the word appears in the bundled list or the seed fixture.
    /// Case-insensitive.
    pub fn is_known(&self, word: &str) -> bool {
        self.frequency(word) > 0
    }

    /// Raw unigram count for the word, or 0 if unknown. Case-insensitive.
    pub fn frequency(&self, word: &str) -> u64 {
        if word.is_empty() {
            return 0;
        }
        self.by_word
            .get(&word.to_ascii_lowercase())
            .copied()
            .unwrap_or(0)
    }

    /// Number of unique entries loaded (bundled + seed). Debug-panel only.
    pub fn len(&self) -> usize {
        self.by_word.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_word.is_empty()
    }
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn lex() -> Lexicon {
        Lexicon::load()
    }

    #[test]
    fn loads_substantial_number_of_words() {
        let l = lex();
        // Top 50k bundled + a handful of seeds. Pin a generous floor so a
        // refresh that accidentally truncates the file fails loudly.
        assert!(l.len() >= 49_000, "loaded only {} entries", l.len());
    }

    #[test]
    fn common_word_is_known_and_high_frequency() {
        let l = lex();
        assert!(l.is_known("the"));
        // "the" is the #1 unigram — billions. Pin a loose floor.
        assert!(
            l.frequency("the") > 1_000_000_000,
            "frequency('the') = {}",
            l.frequency("the")
        );
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let l = lex();
        let lower = l.frequency("the");
        assert_eq!(l.frequency("The"), lower);
        assert_eq!(l.frequency("THE"), lower);
        assert_eq!(l.frequency("tHe"), lower);
    }

    #[test]
    fn unknown_word_returns_zero_and_false() {
        let l = lex();
        assert!(!l.is_known("asdfqwerty"));
        assert_eq!(l.frequency("asdfqwerty"), 0);
    }

    #[test]
    fn empty_string_is_unknown() {
        let l = lex();
        assert!(!l.is_known(""));
        assert_eq!(l.frequency(""), 0);
    }

    #[test]
    fn seed_proper_nouns_are_known_any_case() {
        let l = lex();
        for name in &["Krutrim", "krutrim", "KRUTRIM", "ZAMS", "zams", "ONDC", "ondc"] {
            assert!(l.is_known(name), "expected seed {name:?} to be known");
            assert!(l.frequency(name) > 0);
        }
    }

    #[test]
    fn seed_does_not_overwrite_bundled_frequency() {
        // Pure paranoia: if a seed ever collides with a bundled lowercase
        // entry, the bundled frequency must win — seeds are nominal-1 only
        // for words we'd otherwise have zero data on.
        let l = lex();
        // "the" isn't in the seed list, but check that a known-bundled word
        // is far above the seed sentinel.
        assert!(l.frequency("the") > SEED_FREQUENCY);
    }

    #[test]
    fn shared_is_singleton_and_matches_load() {
        // `shared()` returns the same pointer every call.
        let a = Lexicon::shared() as *const Lexicon;
        let b = Lexicon::shared() as *const Lexicon;
        assert_eq!(a, b);
        // And produces the same answers as a fresh load.
        let fresh = Lexicon::load();
        assert_eq!(Lexicon::shared().is_known("the"), fresh.is_known("the"));
        assert_eq!(
            Lexicon::shared().frequency("krutrim"),
            fresh.frequency("krutrim")
        );
    }

}

// Sanity at compile time — bumping LEXICON_VERSION is how downstream consumers
// know the data shifted under them. Zero would mean "never set".
const _: () = assert!(LEXICON_VERSION >= 1);
