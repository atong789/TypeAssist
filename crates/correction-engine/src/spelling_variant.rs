//! Spelling variant map (UK English, v0.3.0) — Component of the suggestion path.
//!
//! **Membership is dialect-blind; only *suggestion* is locale-aware.** The
//! lexicon ([`crate::lexicon`]) accepts both US and UK spellings as valid words
//! (the v3 merged tolerant list), so a spelling variant is *never* flagged as a
//! slip. This module does the opposite-direction job: once the engine has
//! *already* decided to suggest a fix for a genuine motor slip, it maps the
//! target word to the spelling that matches the user's system locale — a
//! UK-locale Mac is offered `colour`, a US-locale Mac `color`.
//!
//! ## Where locale comes from (Principle #8 / architecture)
//!
//! This module is pure data — it takes the resolved [`SpellingVariant`] as a
//! parameter and **never reads the OS**. Locale *detection* happens in the L5
//! shell (the webview's `navigator.language`, already used by Practice) and is
//! pushed to the engine via `EngineControl::SetSpellingVariant`. Keeping the
//! table in L4 and the OS read in L5 respects the "L2–L4 call no OS APIs" rule.
//!
//! ## Precision over recall (Principle #9 — never a "worse autocorrect")
//!
//! The pairs are derived from VarCon (Kevin Atkinson's variant-conversion
//! dataset, the SCOWL sibling) with a strict filter: both sides must be real
//! dictionary words, and **homographs / context-dependent / register pairs are
//! excluded** — `meter/metre`, `program/programme`, `check/cheque`,
//! `story/storey`, `practice/practise`, `license/licence`, `among/amongst`,
//! `blond/blonde`, … A blind swap there would change the user's *word* or be
//! wrong for the common sense, so those words are left exactly as typed. What
//! remains is the unambiguous `-our`/`-re`/`-ise`/`-ce`/doubled-l/medical
//! families (~1.9k pairs).

use std::collections::HashMap;
use std::sync::OnceLock;

/// The spelling a suggestion should use, resolved from system locale in L5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpellingVariant {
    /// US spelling (`color`, `organize`). The engine default until L5 reports
    /// the locale — safe because it only normalises *British-spelled* targets
    /// and leaves everything else untouched.
    American,
    /// UK spelling (`colour`, `organise`).
    British,
}

impl Default for SpellingVariant {
    fn default() -> Self {
        SpellingVariant::American
    }
}

/// `american<TAB>british`, one pair per line. VarCon-derived, filtered to
/// unambiguous 1:1 spelling variants (see module docs). Bundled at build.
const VARIANTS: &str = include_str!("../lexicon/variants_us_uk.tsv");

struct Maps {
    /// american spelling → british spelling
    to_british: HashMap<&'static str, &'static str>,
    /// british spelling → american spelling
    to_american: HashMap<&'static str, &'static str>,
}

fn maps() -> &'static Maps {
    static M: OnceLock<Maps> = OnceLock::new();
    M.get_or_init(|| {
        let mut to_british = HashMap::new();
        let mut to_american = HashMap::new();
        for line in VARIANTS.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split('\t');
            if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
                to_british.insert(a, b);
                to_american.insert(b, a);
            }
        }
        Maps {
            to_british,
            to_american,
        }
    })
}

/// Re-apply `sample`'s case shape (all-lower / Titlecase / ALL-UPPER) to
/// `mapped`, which is stored lowercase. Anything more exotic falls back to the
/// stored lowercase form — motor-slip targets are overwhelmingly lowercase.
fn match_case(sample: &str, mapped: &'static str) -> String {
    let has_upper = sample.chars().any(|c| c.is_uppercase());
    if !has_upper {
        return mapped.to_string();
    }
    if sample.chars().all(|c| !c.is_lowercase()) {
        return mapped.to_uppercase();
    }
    // Titlecase: first char upper, rest lower — the common `Colour` case.
    let mut out = String::with_capacity(mapped.len());
    for (i, c) in mapped.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The locale-appropriate spelling of `word` for `variant`, or `None` if `word`
/// is not a mapped variant (leave it exactly as typed). Case-insensitive match,
/// case-preserving result. Returns `None` when the word is already in the
/// target variant, so a caller can treat `None` as "no change needed".
pub fn localize(word: &str, variant: SpellingVariant) -> Option<String> {
    if word.is_empty() {
        return None;
    }
    let key = word.to_ascii_lowercase();
    let table = match variant {
        SpellingVariant::British => &maps().to_british,
        SpellingVariant::American => &maps().to_american,
    };
    table.get(key.as_str()).map(|&m| match_case(word, m))
}

/// Non-allocating membership probe against `map`, case-insensitive. The hot
/// path (`Lexicon::is_in_clean`) already hands us a lowercased key, so avoid a
/// per-probe `String` allocation whenever the input is already lowercase.
fn key_in(map: &HashMap<&'static str, &'static str>, word: &str) -> bool {
    if word.bytes().all(|b| !b.is_ascii_uppercase()) {
        map.contains_key(word)
    } else {
        map.contains_key(word.to_ascii_lowercase().as_str())
    }
}

/// True iff `word` is the spelling that does **not** match `variant` — i.e. it
/// is a VarCon variant whose locale-correct counterpart is spelled differently
/// (on a British system, `color`; on an American system, `colour`).
///
/// **Membership excludes it (locale-gated `Lexicon::is_known`, v0.3.0):** the
/// opposite-locale variant is treated as a slip *against* the locale spelling
/// and routed through the slip engine toward it, rather than being accepted as
/// authorship. Locale defines the correct spelling; the other variant is a slip
/// against it (the user still confirms via Shift / dismisses — nothing is
/// auto-applied). Non-variant words and the locale-correct spelling → `false`.
/// Case-insensitive; non-allocating for lowercase input.
pub fn is_opposite_locale(word: &str, variant: SpellingVariant) -> bool {
    if word.is_empty() {
        return false;
    }
    match variant {
        // British system → the *American* spellings (keys of `to_british`) are
        // the opposite-locale variant.
        SpellingVariant::British => key_in(&maps().to_british, word),
        // American system → the *British* spellings (keys of `to_american`).
        SpellingVariant::American => key_in(&maps().to_american, word),
    }
}

/// True iff `word` is either side of any VarCon variant pair (locale-blind).
/// Used to **guard the learner** (v0.3.0): a spelling variant must never be
/// confirmed into the runtime-learned vocabulary, or a learned opposite-locale
/// spelling would re-enter the "known" set and suppress the locale correction
/// again. Novel words (names, jargon) are not in the map, so they learn
/// normally. Case-insensitive.
pub fn is_variant_word(word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    key_in(&maps().to_british, word) || key_in(&maps().to_american, word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposite_locale_is_the_other_spelling() {
        // British system: the American spelling is the opposite-locale variant.
        assert!(is_opposite_locale("color", SpellingVariant::British));
        assert!(is_opposite_locale("colors", SpellingVariant::British));
        assert!(is_opposite_locale("Color", SpellingVariant::British)); // case-insensitive
        // …and the British spelling is NOT (it's locale-correct).
        assert!(!is_opposite_locale("colour", SpellingVariant::British));

        // American system: mirror image.
        assert!(is_opposite_locale("colour", SpellingVariant::American));
        assert!(!is_opposite_locale("color", SpellingVariant::American));

        // Non-variant words are never opposite-locale, either way.
        assert!(!is_opposite_locale("keyboard", SpellingVariant::British));
        assert!(!is_opposite_locale("", SpellingVariant::American));
    }

    #[test]
    fn variant_word_is_locale_blind() {
        // Both sides of a pair are variant words regardless of locale.
        assert!(is_variant_word("color"));
        assert!(is_variant_word("colour"));
        assert!(is_variant_word("ORGANISE")); // case-insensitive
        // Homographs excluded from the map are NOT variant words.
        assert!(!is_variant_word("meter"));
        assert!(!is_variant_word("program"));
        // Ordinary + novel words are never variant words (learner guard is a no-op).
        assert!(!is_variant_word("keyboard"));
        assert!(!is_variant_word("krutrim"));
        assert!(!is_variant_word(""));
    }

    #[test]
    fn maps_both_directions() {
        assert_eq!(
            localize("color", SpellingVariant::British).as_deref(),
            Some("colour")
        );
        assert_eq!(
            localize("colour", SpellingVariant::American).as_deref(),
            Some("color")
        );
        assert_eq!(
            localize("organize", SpellingVariant::British).as_deref(),
            Some("organise")
        );
        assert_eq!(
            localize("centre", SpellingVariant::American).as_deref(),
            Some("center")
        );
    }

    #[test]
    fn already_in_target_variant_is_none() {
        // `colour` is already British → nothing to change for British locale.
        assert_eq!(localize("colour", SpellingVariant::British), None);
        assert_eq!(localize("color", SpellingVariant::American), None);
    }

    #[test]
    fn non_variant_words_untouched() {
        assert_eq!(localize("keyboard", SpellingVariant::British), None);
        assert_eq!(localize("the", SpellingVariant::American), None);
        assert_eq!(localize("", SpellingVariant::British), None);
    }

    #[test]
    fn preserves_case() {
        assert_eq!(
            localize("Color", SpellingVariant::British).as_deref(),
            Some("Colour")
        );
        assert_eq!(
            localize("COLOR", SpellingVariant::British).as_deref(),
            Some("COLOUR")
        );
        assert_eq!(
            localize("color", SpellingVariant::British).as_deref(),
            Some("colour")
        );
    }

    #[test]
    fn homographs_and_context_pairs_excluded() {
        // These must NOT map in either direction — a blind swap would be wrong.
        for w in [
            "meter", "program", "check", "story", "tire", "practice", "license", "among", "blond",
        ] {
            assert_eq!(
                localize(w, SpellingVariant::British),
                None,
                "{w} should be excluded"
            );
        }
    }
}
