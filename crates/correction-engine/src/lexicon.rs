//! Lexicon — Component 3a of the L4 brief.
//!
//! Read-only word source for the correction engine. The lexicon does **two
//! different jobs** and uses **two different data sources**:
//!
//! 1. **Membership** — "is this a correctly-spelled word?" Answered by a
//!    clean spelling dictionary (SCOWL en_US + contractions + VarCon British
//!    spellings, ~84k entries — the v3 **merged tolerant list**: both US and UK
//!    spellings are always valid, so a spelling variant is never flagged as a
//!    slip). Common misspellings like `teh`, `recieve`, `didnt` are *not*
//!    members.
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
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{OnceLock, RwLock};

use crate::spelling_variant::SpellingVariant;

/// Version of the loaded lexicon (clean dict + freq table + seed fixture).
///
/// v1 — single-source: Norvig top-50k served both membership and frequency.
///       Bug: web typos like `teh` / `recieve` passed `is_known`, protecting
///       the very misspellings the engine should catch. Apostrophe-stripped
///       contractions in Norvig also caused `didn't` → suggested as `didnt`.
/// v2 — split sources: SCOWL en_US + contractions is the membership test
///       (rejects `teh` / `recieve`, includes `didn't`); Norvig stays as the
///       ranking-only frequency table.
/// v3 — **merged list** (UK English): British spellings (VarCon British forms,
///       level ≤ 50, ~3.1k words) are unioned into the membership set alongside
///       en_US so both variants are bundled.
/// v4 — **locale-gated membership** (UK English, v0.3.0 refinement): once L5
///       reports the system locale ([`Self::set_spelling_variant`]), the
///       **opposite-locale** spelling of a VarCon variant pair is excluded from
///       `is_known` — on a British system `color` is unknown, on an American
///       system `colour` is. **Locale defines the correct spelling; the other
///       variant is a slip against it**, routed through the slip engine toward
///       the locale spelling (the user still confirms via Shift / dismisses —
///       nothing is auto-applied). This *supersedes* the earlier v3 "spelling
///       variant is authorship, never flagged" stance. Before a locale is set
///       the gate is off (both variants known — the pre-locale tolerant
///       default, so startup and tests are unaffected). The runtime learner is
///       guarded so a variant word can never re-enter "known" (see
///       [`crate::spelling_variant::is_variant_word`] /
///       [`crate::LexiconProposer::note_record`]); novel words learn normally.
pub const LEXICON_VERSION: u32 = 4;

/// Bundled clean spelling dictionary — SCOWL cumulative size-50, en_US
/// flavour (english-words + american-words + variant_1-words +
/// english-contractions + variant_1-contractions, all sizes ≤ 50). One word
/// per line, lowercase, sorted, deduplicated. ~81k entries. License: SCOWL
/// composite — see `lexicon/SCOWL_COPYRIGHT.txt`.
const WORDS_CLEAN: &str = include_str!("../lexicon/words_clean.txt");

/// Bundled **British** spellings — VarCon British forms (level ≤ 50, en_US
/// removed so it's add-only), lowercase, one per line incl. possessives to
/// match `words_clean`. Unioned into the membership set so both US and UK
/// spellings are always valid (v3 merged tolerant list — see
/// [`LEXICON_VERSION`]). License: VarCon (Kevin Atkinson, SCOWL sibling) — see
/// `lexicon/SCOWL_COPYRIGHT.txt`.
const WORDS_UK: &str = include_str!("../lexicon/words_uk.txt");

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
    /// Bundled; immutable after `load`.
    clean: HashSet<String>,
    /// Frequency table — Norvig web unigrams, all lowercase. May contain
    /// entries that are *not* in `clean` (typos, noise); those still
    /// expose their count via `frequency`, but `is_known` returns false
    /// for them unless they're in `clean` or `learned`.
    freq: HashMap<String, u64>,
    /// **C5b Phase 2** — runtime-learned vocabulary. The
    /// [`crate::LexiconProposer`] adds a word here when its proposal
    /// reaches `Confirmed`, removes it on demotion. Interior mutability
    /// (`RwLock`) so the `'static` shared singleton can host live
    /// learning without changing every caller's signature. Reads are
    /// fast (RwLock favours readers); writes happen at most once per
    /// proposal-tier transition and are followed by a single re-eval
    /// pass.
    ///
    /// `is_known` returns true for membership in EITHER set — that's
    /// the "one wiring, three effects" the brief asked for: every
    /// caller (linguistic gate, candidate gen, decision pipeline) sees
    /// the live lexicon without a separate code path.
    learned: RwLock<HashSet<String>>,
    /// **v4 locale gate** — the active spelling variant reported by L5,
    /// encoded `0 = unset` (both variants known — pre-locale tolerant
    /// default), `1 = American`, `2 = British`. When set, the
    /// opposite-locale spelling of a VarCon pair is excluded from
    /// membership (see [`Self::is_known`]). Atomic so the `'static`
    /// singleton can be re-pointed on a `SetSpellingVariant` control
    /// without a lock on the hot membership path.
    active_variant: AtomicU8,
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
        for raw in WORDS_UK.lines() {
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

        Self {
            clean,
            freq,
            learned: RwLock::new(HashSet::new()),
            active_variant: AtomicU8::new(0), // unset → gate off until L5 reports
        }
    }

    /// Process-wide singleton. Loading is ~140k inserts on first call;
    /// subsequent calls are a pointer load. Safe to call from anywhere.
    pub fn shared() -> &'static Lexicon {
        static LEX: OnceLock<Lexicon> = OnceLock::new();
        LEX.get_or_init(Lexicon::load)
    }

    /// Iterate every word in the clean membership set — lowercase, no
    /// guaranteed order. Used by C5b's bigram plausibility model to
    /// build its frequency table at module load. Not for hot-path use.
    pub fn iter_clean(&self) -> impl Iterator<Item = &str> {
        self.clean.iter().map(|s| s.as_str())
    }

    /// True iff the word is a member of the clean spelling dictionary,
    /// the seed fixture, OR the runtime-learned set (see
    /// [`Self::learn`]). Frequency is *not* consulted — a corpus typo
    /// like `teh` with millions of web occurrences is still unknown.
    /// Case-insensitive.
    ///
    /// **C5b Phase 2:** the union with `learned` is the single wiring
    /// that gives every consumer (linguistic gate, candidate
    /// generation, decision pipeline) live access to the user's
    /// learned vocabulary without a separate code path.
    ///
    /// **v4 locale gate:** once a spelling variant is set, the
    /// opposite-locale spelling of a VarCon pair is *not* known — so a
    /// garble converges only on the locale spelling and a cleanly-typed
    /// opposite variant routes through the slip engine toward it.
    pub fn is_known(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        let key = word.to_ascii_lowercase();
        if self.is_opposite_locale_variant(&key) {
            return false;
        }
        self.clean.contains(&key) || self.learned.read().unwrap().contains(&key)
    }

    /// Adopt the system spelling variant reported by L5 (v0.3.0). Flips
    /// the locale gate (see [`Self::is_known`]): the opposite-locale
    /// spelling of every VarCon pair becomes unknown. Content-free (a
    /// two-value enum), never persisted. Cheap — a single atomic store.
    pub fn set_spelling_variant(&self, variant: SpellingVariant) {
        let code = match variant {
            SpellingVariant::American => 1,
            SpellingVariant::British => 2,
        };
        self.active_variant.store(code, Ordering::Relaxed);
    }

    /// True iff `key` (already lowercase) is the opposite-locale spelling
    /// of a VarCon pair under the active variant. `false` while the
    /// variant is unset (pre-locale tolerant default) or for non-variant
    /// words. Hot path: one relaxed atomic load + one hashmap probe, no
    /// allocation.
    fn is_opposite_locale_variant(&self, key: &str) -> bool {
        let variant = match self.active_variant.load(Ordering::Relaxed) {
            1 => SpellingVariant::American,
            2 => SpellingVariant::British,
            _ => return false, // unset → gate off
        };
        crate::spelling_variant::is_opposite_locale(key, variant)
    }

    /// True iff the word is in the **runtime-learned** set
    /// specifically (NOT in the bundled clean dict). Used by
    /// callers that need to distinguish learned from bundled
    /// vocabulary — e.g. the linguistic gate's proximity check
    /// treats learned words as anchors regardless of Norvig
    /// frequency (they're user-specific by construction). Case-
    /// insensitive.
    pub fn is_learned(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        self.learned.read().unwrap().contains(&word.to_ascii_lowercase())
    }

    /// True iff the word is in the **bundled clean dict** ONLY (SCOWL
    /// plus seed). Does NOT consult the runtime-learned set, and so does
    /// NOT acquire the RwLock. Used by hot inner loops (the linguistic
    /// gate's edit-2 proximity check runs ~120k membership probes per
    /// call) that already check the learned side via a passed-in
    /// snapshot. Case-insensitive. **v4 locale gate applies** — the
    /// opposite-locale variant is excluded here too, so convergence
    /// candidate generation ([`crate::shadow_convergence_scan`]) never
    /// offers it as a rival to the locale spelling.
    pub fn is_in_clean(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        let key = word.to_ascii_lowercase();
        if self.is_opposite_locale_variant(&key) {
            return false;
        }
        self.clean.contains(&key)
    }

    /// Add a word to the runtime-learned set. Case-insensitive
    /// (lowercased on insert). Idempotent — re-adding a learned
    /// word is a no-op. Called by [`crate::LexiconProposer`] on a
    /// `Confirmed` tier transition. Returns true iff this call
    /// changed the set (the word wasn't there before).
    pub fn learn(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        self.learned
            .write()
            .unwrap()
            .insert(word.to_ascii_lowercase())
    }

    /// Remove a word from the runtime-learned set. Case-
    /// insensitive. No-op if it wasn't there. Returns true iff
    /// this call changed the set. Called on a tier transition
    /// out of `Confirmed` (demotion).
    pub fn unlearn(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }
        self.learned
            .write()
            .unwrap()
            .remove(&word.to_ascii_lowercase())
    }

    /// Drop every learned word. Used by tests to reset state
    /// between cases; not called in production.
    pub fn clear_learned(&self) {
        self.learned.write().unwrap().clear();
    }

    /// Snapshot of the learned set, lowercase, no guaranteed order.
    /// Surfaced to the debug panel so the builder can see which
    /// words are live in `is_known` and how that's affecting
    /// proximity verdicts elsewhere.
    pub fn learned_snapshot(&self) -> Vec<String> {
        self.learned.read().unwrap().iter().cloned().collect()
    }

    /// Number of words in the learned set. Cheap (single read lock).
    pub fn learned_len(&self) -> usize {
        self.learned.read().unwrap().len()
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

    // ---- v4 locale gate --------------------------------------------------

    #[test]
    fn both_variants_known_until_locale_is_set() {
        // Pre-locale tolerant default (gate off): a fresh lexicon accepts
        // both spellings, so startup and every existing test are unaffected.
        let l = lex();
        assert!(l.is_known("color"));
        assert!(l.is_known("colour"));
        assert!(l.is_in_clean("color"));
        assert!(l.is_in_clean("colour"));
    }

    #[test]
    fn british_locale_gates_the_american_spelling() {
        let l = lex();
        l.set_spelling_variant(SpellingVariant::British);
        // The American spelling is now a slip against the locale → unknown.
        assert!(!l.is_known("color"), "color should be gated on a British system");
        assert!(!l.is_known("colors"));
        assert!(!l.is_in_clean("color"), "candidate gen must not offer `color`");
        // The locale-correct British spelling stays known.
        assert!(l.is_known("colour"));
        assert!(l.is_in_clean("colour"));
        // Non-variant words are untouched by the gate.
        assert!(l.is_known("keyboard"));
        assert!(!l.is_known("teh"));
    }

    #[test]
    fn american_locale_gates_the_british_spelling() {
        let l = lex();
        l.set_spelling_variant(SpellingVariant::American);
        assert!(!l.is_known("colour"), "colour should be gated on an American system");
        assert!(!l.is_in_clean("colour"));
        assert!(l.is_known("color"));
        assert!(l.is_in_clean("color"));
    }

    #[test]
    fn locale_gate_beats_the_learned_set() {
        // Even if a variant word were somehow learned, the gate wins — the
        // opposite-locale spelling can never be "known" (the anti-suppression
        // guarantee; the learner is *also* guarded upstream, belt-and-braces).
        let l = lex();
        l.set_spelling_variant(SpellingVariant::British);
        l.learn("color");
        assert!(!l.is_known("color"), "gate must override a learned variant");
        l.clear_learned();
    }

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
