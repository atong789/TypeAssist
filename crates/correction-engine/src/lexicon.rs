//! Lexicon — Component 3a of the L4 brief.
//!
//! Read-only word source for the correction engine. The lexicon does **two
//! different jobs** and uses **two different data sources**:
//!
//! 1. **Membership** — "is this a correctly-spelled word?" Answered by a
//!    clean spelling dictionary (SCOWL en_US + contractions, ~80k entries).
//!    Common misspellings like `teh`, `recieve`, `didnt` are *not* members.
//!    Plus a temporary seed list of proper nouns (dev fixture — see below).
//! 2. **Frequency** — used by Component 3b *only* to rank candidates of an
//!    unknown word. Sourced from the Norvig web-unigram counts (top 50k).
//!    Frequencies can be huge (`the` ≈ 23B); a real word in the clean dict
//!    that's missing from the freq table still gets a small default count
//!    so it survives ranking — being absent from one corpus must never
//!    flip a real word to unknown.
//!
//! This split exists because the web-frequency corpus (Norvig's
//! `count_1w.txt`) contains common misspellings and apostrophe-stripped
//! contractions as legitimate-looking unigrams — a `teh` with ~1.7M web
//! occurrences would otherwise pass `is_known`, protecting the very typo
//! the engine is supposed to catch. The membership test gates correction;
//! the frequency table only ranks alternatives.
//!
//! ## API
//!
//! * [`Lexicon::shared`] — process-wide singleton, loaded on first use.
//! * [`Lexicon::is_known`] — case-insensitive bool, `true` iff in the clean
//!   dict or the seed list.
//! * [`Lexicon::frequency`] — raw unigram count for ranking; 0 for unknown
//!   words; [`SEED_FREQUENCY`] (a small constant) for words that are known
//!   but absent from the Norvig freq table.
//!
//! ## Versioning
//!
//! Bump [`LEXICON_VERSION`] on any change to either bundled list **or** the
//! seed list. Consumers can compare the version to refuse stale snapshots —
//! same contract as [`crate::tokenizer::TOKENIZER_VERSION`].

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// Version of the loaded lexicon (clean dict + freq table + seed fixture).
///
/// v1 — single-source: Norvig top-50k served both membership and frequency.
///       Bug: web typos like `teh` / `recieve` passed `is_known`, protecting
///       the very misspellings the engine should catch. Apostrophe-stripped
///       contractions in Norvig also caused `didn't` → suggested as `didnt`.
/// v2 — split sources: SCOWL en_US + contractions is the membership test
///       (rejects `teh` / `recieve`, includes `didn't`); Norvig stays as the
///       ranking-only frequency table.
pub const LEXICON_VERSION: u32 = 2;

/// Bundled clean spelling dictionary — SCOWL cumulative size-50, en_US
/// flavour (english-words + american-words + variant_1-words +
/// english-contractions + variant_1-contractions, all sizes ≤ 50). One word
/// per line, lowercase, sorted, deduplicated. ~81k entries. License: SCOWL
/// composite — see `lexicon/SCOWL_COPYRIGHT.txt`.
const WORDS_CLEAN: &str = include_str!("../lexicon/words_clean.txt");

/// Bundled `word<TAB>count` frequency table — top 50k Norvig web unigrams.
/// Lowercase ASCII. Used **only** for ranking candidates of an unknown
/// word; membership is not derived from this file (see module docs).
const WORDS_FREQ: &str = include_str!("../lexicon/words_freq.txt");

/// **DEV FIXTURE — temporary.** Production populates the user's proper-noun
/// set from live-learning (Component 5: user typed it, the engine left it
/// alone, after N repetitions it earns a slot). The seed list is *never*
/// populated by the user typing words into any UI.
///
/// Stored lowercase so the case-insensitive lookup works for any input case
/// (`Krutrim`, `KRUTRIM`, `krutrim` all hit).
const SEED_PROPER_NOUNS: &[&str] = &["krutrim", "zams", "ondc"];

/// Small fallback frequency for words that are known (clean dict or seed)
/// but absent from the Norvig freq table. Nonzero so ranking still
/// considers them, low enough that genuinely common words always lead.
const SEED_FREQUENCY: u64 = 1;

pub struct Lexicon {
    /// Membership set — clean SCOWL dict + seed proper nouns, all lowercase.
    /// `is_known` consults this and nothing else.
    clean: HashSet<String>,
    /// Frequency table — Norvig web unigrams, all lowercase. May contain
    /// entries that are *not* in `clean` (typos, noise); those still
    /// expose their count via `frequency`, but `is_known` returns false.
    freq: HashMap<String, u64>,
}

impl Lexicon {
    /// Parse both bundled files and inject [`SEED_PROPER_NOUNS`] into the
    /// clean set. Done once per process via [`Self::shared`]; exposed for
    /// tests that want a fresh instance without touching the global.
    pub fn load() -> Self {
        let mut clean: HashSet<String> = HashSet::with_capacity(90_000);
        for raw in WORDS_CLEAN.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            clean.insert(line.to_ascii_lowercase());
        }
        for &seed in SEED_PROPER_NOUNS {
            clean.insert(seed.to_ascii_lowercase());
        }

        let mut freq: HashMap<String, u64> = HashMap::with_capacity(60_000);
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
            freq.insert(word.to_ascii_lowercase(), count);
        }

        Self { clean, freq }
    }

    /// Process-wide singleton. Loading is ~140k inserts on first call;
    /// subsequent calls are a pointer load. Safe to call from anywhere.
    pub fn shared() -> &'static Lexicon {
        static LEX: OnceLock<Lexicon> = OnceLock::new();
        LEX.get_or_init(Lexicon::load)
    }

    /// True iff the word is a member of the clean spelling dictionary or
    /// the seed fixture. Frequency is *not* consulted — a corpus typo like
    /// `teh` with millions of web occurrences is still unknown. Case-
    /// insensitive.
    pub fn is_known(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        self.clean.contains(&word.to_ascii_lowercase())
    }

    /// Raw unigram count used for ranking candidates of unknown words.
    ///
    /// * In freq table → returns the bundled count (whether or not the word
    ///   is in the clean dict — typos still have their honest count).
    /// * Not in freq table but in clean dict → returns [`SEED_FREQUENCY`],
    ///   so a real word missing from Norvig still ranks above the floor.
    /// * Neither → returns 0.
    ///
    /// Case-insensitive.
    pub fn frequency(&self, word: &str) -> u64 {
        if word.is_empty() {
            return 0;
        }
        let key = word.to_ascii_lowercase();
        if let Some(&c) = self.freq.get(&key) {
            return c;
        }
        if self.clean.contains(&key) {
            return SEED_FREQUENCY;
        }
        0
    }

    /// Number of unique clean-dict entries (membership set). Debug-panel
    /// only — does not reflect the frequency table size.
    pub fn len(&self) -> usize {
        self.clean.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clean.is_empty()
    }
}

// Sanity at compile time — bumping LEXICON_VERSION is how downstream consumers
// know the data shifted under them. Zero would mean "never set".
const _: () = assert!(LEXICON_VERSION >= 1);

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn lex() -> Lexicon {
        Lexicon::load()
    }

    // ---- Load sanity -----------------------------------------------------

    #[test]
    fn loads_substantial_clean_dict() {
        // SCOWL cumulative size-50 en_US + contractions + seeds. Pin a
        // generous floor so a refresh that truncates the file fails loudly.
        let l = lex();
        assert!(l.len() >= 70_000, "loaded only {} clean entries", l.len());
    }

    // ---- Membership — everyday words must be known ----------------------

    #[test]
    fn common_words_are_known() {
        let l = lex();
        for w in &["the", "because", "it", "should", "have", "would", "people"] {
            assert!(l.is_known(w), "expected {w:?} to be known");
        }
    }

    #[test]
    fn correctly_spelled_apostrophe_words_are_known() {
        // The contractions fix from v2 — SCOWL contractions list pulls
        // these in, where Norvig's apostrophe-stripped corpus did not.
        let l = lex();
        for w in &["didn't", "can't", "won't", "it's", "they're", "we'll", "should've"] {
            assert!(l.is_known(w), "expected contraction {w:?} to be known");
        }
    }

    // ---- Membership — typos must NOT be known ---------------------------

    #[test]
    fn common_typos_are_not_known() {
        // The whole point of v2. These appear in the Norvig web corpus as
        // common typos; if `is_known` accepted them, the engine would
        // protect the very misspellings it should catch.
        let l = lex();
        for typo in &["teh", "recieve", "thier", "definately", "occured", "untill"] {
            assert!(!l.is_known(typo), "typo {typo:?} should NOT be known");
        }
    }

    #[test]
    fn apostrophe_stripped_contractions_are_not_known() {
        // "didnt" / "thats" / "shouldnt" are how the Norvig corpus stores
        // contractions (no apostrophe). They are NOT correctly-spelled
        // English and must not appear in the clean dict.
        //
        // NB: "wont" and "cant" are *also* legitimate English nouns ("the
        // wont of children", "thieves' cant"), so SCOWL keeps them — we
        // don't test those: they're true positives for membership, not
        // false ones. The motor-error story doesn't change: if the user
        // typed "wont" meaning "won't", the engine sees a known word and
        // leaves it alone, which is the conservative outcome.
        let l = lex();
        for stripped in &["didnt", "thats", "shouldnt", "couldnt", "theyre", "youre"] {
            assert!(
                !l.is_known(stripped),
                "apostrophe-stripped {stripped:?} should NOT be known"
            );
        }
    }

    // ---- Membership — case and edge cases --------------------------------

    #[test]
    fn lookup_is_case_insensitive() {
        let l = lex();
        for variant in &["The", "THE", "tHe"] {
            assert!(l.is_known(variant), "case variant {variant:?} should be known");
        }
    }

    #[test]
    fn unknown_word_is_not_known() {
        let l = lex();
        assert!(!l.is_known("asdfqwerty"));
        assert!(!l.is_known("qzxjvk"));
    }

    #[test]
    fn empty_string_is_unknown() {
        let l = lex();
        assert!(!l.is_known(""));
        assert_eq!(l.frequency(""), 0);
    }

    // ---- Seed proper nouns ----------------------------------------------

    #[test]
    fn seed_proper_nouns_are_known_any_case() {
        let l = lex();
        for name in &["Krutrim", "krutrim", "KRUTRIM", "ZAMS", "zams", "ONDC", "ondc"] {
            assert!(l.is_known(name), "expected seed {name:?} to be known");
            // Seed-only words (no Norvig entry) get the SEED_FREQUENCY
            // default so they still sort somewhere when ranking.
            assert!(l.frequency(name) >= SEED_FREQUENCY);
        }
    }

    // ---- Frequency — honest about corpus counts -------------------------

    #[test]
    fn common_word_returns_high_frequency() {
        // "the" tops the Norvig corpus — billions. Pin a loose floor.
        let l = lex();
        assert!(
            l.frequency("the") > 1_000_000_000,
            "frequency('the') = {}",
            l.frequency("the")
        );
    }

    #[test]
    fn typo_in_norvig_keeps_its_count() {
        // "teh" is in the Norvig web corpus (~1.7M occurrences). The fix
        // is to make it `is_known = false`, NOT to hide the frequency.
        // The honest count stays — it's a typo, and nothing in 3b ranks
        // typos against each other, but if a future caller asks for the
        // raw count we hand it over rather than lying.
        let l = lex();
        assert!(!l.is_known("teh"));
        assert!(l.frequency("teh") > 0);
    }

    #[test]
    fn known_word_absent_from_freq_table_gets_default() {
        // The brief: "a real word that's in the clean dict yet absent from
        // the Norvig list must still be is_known=true with a small default
        // frequency." Find such a word programmatically — picking a hard-
        // coded example would brittle across refreshes — then verify both
        // properties hold.
        let l = lex();
        let example = l
            .clean
            .iter()
            .find(|w| !l.freq.contains_key(*w))
            .cloned()
            .expect("expected at least one clean-dict word missing from freq table");
        assert!(l.is_known(&example), "{example:?} should be known");
        assert_eq!(
            l.frequency(&example),
            SEED_FREQUENCY,
            "{example:?} should fall back to SEED_FREQUENCY"
        );
    }

    #[test]
    fn unknown_word_returns_zero_frequency() {
        let l = lex();
        assert_eq!(l.frequency("asdfqwerty"), 0);
        assert_eq!(l.frequency("qzxjvk"), 0);
    }

    // ---- Singleton ------------------------------------------------------

    #[test]
    fn shared_is_singleton_and_matches_load() {
        let a = Lexicon::shared() as *const Lexicon;
        let b = Lexicon::shared() as *const Lexicon;
        assert_eq!(a, b);
        let fresh = Lexicon::load();
        assert_eq!(Lexicon::shared().is_known("the"), fresh.is_known("the"));
        assert_eq!(Lexicon::shared().is_known("teh"), fresh.is_known("teh"));
        assert_eq!(
            Lexicon::shared().frequency("krutrim"),
            fresh.frequency("krutrim")
        );
    }
}
