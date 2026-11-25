//! Teach-back (engine only — no UI yet): the **Keep** and **Remove** actions on the
//! user's learned words, and the OS-dictionary teaching that goes with them.
//!
//! * **Keep(word)** — "this word is mine":
//!   1. adds it to the learned lexicon as a *pinned* known word (no gate, re-evaluation
//!      or retraction can demote it) and persists `learned_lexicon.json`;
//!   2. **Jordan stops offering a fix for it.** Lexicon membership alone already stops
//!      the dictionary ("bold") lane (a known word has no candidates) but NOT the
//!      learned lane — a known word with ≥12 historical corrections still cleared the
//!      real-word bar. So the classifier now treats a *learned* typed word as never
//!      suggestible (`ObserveReason::LearnedWord`) — no separate suppression store needed;
//!   3. teaches the macOS spellchecker (`learnWord:`) so the red squiggle goes, and
//!      remembers it did so (`os_taught`) so it can be undone.
//! * **Remove(word)** — the undo: drops it from the lexicon (+ pinned), tombstones it so
//!   the silent auto-grow lane doesn't quietly re-learn it, persists, and un-teaches the
//!   OS dictionary so the squiggle returns.
//! * **Keep / Remove are teach actions, not typing outcomes — they write no diary row.**
//! * **Reset / Delete-everything / Restore** un-teach every word Jordan taught the OS
//!   ([`unteach_all`]) so macOS isn't left holding words Jordan has forgotten.
//!
//! Only words Jordan itself taught are ever un-taught: a word the user added to the
//! system dictionary on their own is never touched by Reset/Delete (Remove un-teaches
//! the word the user explicitly removed, whoever taught it).
//!
//! The policy is a pure function over a [`SpellBackend`], so the joins are unit-tested
//! with a fake dictionary; the engine handler calls these with the real one.

use crate::os_dictionary::SpellBackend;
use correction_engine::{lexicon_persist_if_dirty, LexiconProposer, TeachError};
use serde::Serialize;
use std::path::Path;

/// What a Keep/Remove did, for the UI and the log. Serialised on `lexicon://changed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TeachReport {
    /// `"keep"` or `"remove"`.
    pub action: &'static str,
    /// The clean word acted on (empty if refused).
    pub word: String,
    /// The learned lexicon was changed (and is what `Jordan` now uses).
    pub ok: bool,
    /// `learned_lexicon.json` was written (word count), `None` if the write failed
    /// or there is no data dir (the change is in memory and will retry).
    pub saved_words: Option<usize>,
    /// Keep: did macOS accept the word (squiggle gone)? Remove: did macOS give it back
    /// (squiggle returns)? `None` if there was nothing to do on the OS side.
    pub os_ok: Option<bool>,
    /// Plain-language note when something didn't fully land.
    pub message: String,
}

fn refused(action: &'static str, e: TeachError) -> TeachReport {
    TeachReport {
        action,
        word: String::new(),
        ok: false,
        saved_words: None,
        os_ok: None,
        message: match e {
            TeachError::Empty => "There’s no word to use.".into(),
            TeachError::VariantWord => {
                "This spelling follows your system language, so it can’t be kept.".into()
            }
        },
    }
}

fn persist(proposer: &mut LexiconProposer, path: Option<&Path>) -> Option<usize> {
    let path = path?;
    lexicon_persist_if_dirty(proposer, path).ok().flatten()
}

/// **Keep** `raw`: lexicon (pinned) → persist → macOS learnWord. See the module docs.
pub fn keep_word(
    proposer: &mut LexiconProposer,
    os: &mut dyn SpellBackend,
    lexicon_path: Option<&Path>,
    raw: &str,
    now: u64,
) -> TeachReport {
    let word = match proposer.keep_word(raw, now) {
        Ok(w) => w,
        Err(e) => return refused("keep", e),
    };
    // OS side first so `os_taught` lands in the same write as the word.
    let (os_ok, message) = match os.learn(&word) {
        Ok(()) => {
            proposer.mark_os_taught(&word);
            (Some(true), String::new())
        }
        Err(e) => (
            Some(false),
            format!("Kept in Jordan, but macOS didn’t take it: {e}"),
        ),
    };
    let saved_words = persist(proposer, lexicon_path);
    TeachReport {
        action: "keep",
        word,
        ok: true,
        saved_words,
        os_ok,
        message,
    }
}

/// **Remove** `raw`: lexicon (+pin, +tombstone) → persist → macOS unlearnWord.
pub fn remove_word(
    proposer: &mut LexiconProposer,
    os: &mut dyn SpellBackend,
    lexicon_path: Option<&Path>,
    raw: &str,
) -> TeachReport {
    let word = match proposer.forget_word(raw) {
        Ok(w) => w,
        Err(e) => return refused("remove", e),
    };
    // Un-teach only if macOS actually holds it (a no-op otherwise).
    let (os_ok, message) = if os.has_learned(&word) {
        match os.unlearn(&word) {
            Ok(()) => (Some(true), String::new()),
            Err(e) => (
                Some(false),
                format!("Removed from Jordan, but macOS still holds it: {e}"),
            ),
        }
    } else {
        (None, String::new())
    };
    proposer.clear_os_taught(&word);
    let saved_words = persist(proposer, lexicon_path);
    TeachReport {
        action: "remove",
        word,
        ok: true,
        saved_words,
        os_ok,
        message,
    }
}

/// Un-teach every word **Jordan** taught the OS (Reset / Delete-everything / Restore),
/// best-effort, and forget the record. Returns `(un-taught, failed)`.
pub fn unteach_all(proposer: &mut LexiconProposer, os: &mut dyn SpellBackend) -> (usize, usize) {
    let (mut done, mut failed) = (0, 0);
    for w in proposer.os_taught_words() {
        if os.has_learned(&w) {
            match os.unlearn(&w) {
                Ok(()) => done += 1,
                Err(_) => failed += 1,
            }
        }
        proposer.clear_os_taught(&w);
    }
    (done, failed)
}

/// The learned set is process-global; tests that touch it share this lock.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os_dictionary::fake::FakeSpell;
    use correction_engine::{load_learned_state, read_learned_words, Lexicon};

    fn setup(
        tag: &str,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        std::path::PathBuf,
        LexiconProposer,
    ) {
        let g = test_lock();
        Lexicon::shared().clear_learned();
        let dir =
            std::env::temp_dir().join(format!("tcd-teach-{tag}-{:?}", std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (g, dir, LexiconProposer::new())
    }

    const NOW: u64 = 1_800_000_000_000;

    #[test]
    fn keep_does_all_three_lexicon_persisted_and_the_macos_squiggle_is_gone() {
        let (_g, dir, mut p) = setup("keep3");
        let path = dir.join("learned_lexicon.json");
        let mut os = FakeSpell::default();
        assert!(os.squiggle("Krutrimish"), "precondition: squiggle present");

        let r = keep_word(&mut p, &mut os, Some(&path), "'Krutrimish'", NOW);

        assert!(r.ok && r.word == "Krutrimish", "{r:?}");
        // a) the lexicon has it (memory AND disk)
        assert!(Lexicon::shared().is_learned("Krutrimish"));
        assert_eq!(read_learned_words(&path), vec!["Krutrimish".to_string()]);
        assert_eq!(r.saved_words, Some(1));
        // c) macOS taught — squiggle gone — and remembered so it can be undone
        assert_eq!(os.learn_calls, vec!["Krutrimish".to_string()]);
        assert!(!os.squiggle("Krutrimish"), "c) squiggle gone");
        assert_eq!(r.os_ok, Some(true));
        let st = load_learned_state(&path);
        assert_eq!(st.os_taught, vec!["Krutrimish".to_string()]);
        assert_eq!(st.pinned, vec!["krutrimish".to_string()]);
    }

    #[test]
    fn keep_still_keeps_the_word_when_macos_refuses_and_says_so() {
        let (_g, dir, mut p) = setup("keepfail");
        let path = dir.join("learned_lexicon.json");
        let mut os = FakeSpell {
            fail_learn: true,
            ..Default::default()
        };
        let r = keep_word(&mut p, &mut os, Some(&path), "Krutrimish", NOW);
        assert!(r.ok, "the Jordan side still lands");
        assert_eq!(r.os_ok, Some(false));
        assert!(
            !r.message.is_empty(),
            "an honest note, not a silent half-success"
        );
        assert!(Lexicon::shared().is_learned("Krutrimish"));
        assert!(
            load_learned_state(&path).os_taught.is_empty(),
            "never claims it taught the OS when it didn't"
        );
    }

    #[test]
    fn keep_writes_no_diary_row() {
        let (_g, dir, mut p) = setup("keepdiary");
        // Diary is ON in this dir; a Keep is a teach action, not a typing outcome.
        std::fs::write(dir.join(crate::resolution_diary::DIARY_FLAG), "1").unwrap();
        let mut os = FakeSpell::default();
        keep_word(
            &mut p,
            &mut os,
            Some(&dir.join("learned_lexicon.json")),
            "Krutrimish",
            NOW,
        );
        remove_word(
            &mut p,
            &mut os,
            Some(&dir.join("learned_lexicon.json")),
            "Krutrimish",
        );
        assert!(
            !dir.join(crate::resolution_diary::DIARY_FILE).exists(),
            "no diary file created"
        );
        assert_eq!(
            crate::resolution_diary::read_summary(&dir, NOW).total_events,
            0
        );
    }

    #[test]
    fn remove_drops_the_word_unteaches_macos_and_the_squiggle_returns() {
        let (_g, dir, mut p) = setup("remove");
        let path = dir.join("learned_lexicon.json");
        let mut os = FakeSpell::default();
        keep_word(&mut p, &mut os, Some(&path), "Krutrimish", NOW);
        assert!(!os.squiggle("Krutrimish"));

        let r = remove_word(&mut p, &mut os, Some(&path), "Krutrimish");

        assert!(r.ok && r.word == "Krutrimish");
        assert!(
            !Lexicon::shared().is_learned("Krutrimish"),
            "a) gone from the lexicon"
        );
        assert!(read_learned_words(&path).is_empty(), "a) gone from disk");
        assert_eq!(os.unlearn_calls, vec!["Krutrimish".to_string()]);
        assert!(os.squiggle("Krutrimish"), "b) the squiggle returns");
        assert_eq!(r.os_ok, Some(true));
        let st = load_learned_state(&path);
        assert!(
            st.os_taught.is_empty() && st.pinned.is_empty(),
            "no stale Keep records"
        );
        assert_eq!(
            st.removed,
            vec!["krutrimish".to_string()],
            "tombstoned against silent re-learning"
        );
    }

    #[test]
    fn remove_of_a_word_macos_does_not_hold_does_not_call_unlearn() {
        let (_g, dir, mut p) = setup("remove-noos");
        let mut os = FakeSpell::default();
        let r = remove_word(
            &mut p,
            &mut os,
            Some(&dir.join("learned_lexicon.json")),
            "Krutrimish",
        );
        assert!(r.ok);
        assert!(os.unlearn_calls.is_empty());
        assert_eq!(r.os_ok, None);
    }

    #[test]
    fn unteach_all_only_touches_words_jordan_taught() {
        let (_g, dir, mut p) = setup("unteach");
        let mut os = FakeSpell::default();
        os.held.insert("mineownword".into()); // the user added this to macOS themselves
        keep_word(
            &mut p,
            &mut os,
            Some(&dir.join("learned_lexicon.json")),
            "Krutrimish",
            NOW,
        );
        let (done, failed) = unteach_all(&mut p, &mut os);
        assert_eq!((done, failed), (1, 0));
        assert!(os.squiggle("Krutrimish"), "Jordan's teaching is undone");
        assert!(
            !os.squiggle("mineownword"),
            "the user's own dictionary word is untouched"
        );
        assert!(p.os_taught_words().is_empty());
    }

    #[test]
    fn teach_state_rides_in_the_backup_bundle() {
        let (_g, dir, mut p) = setup("backup");
        let path = dir.join("learned_lexicon.json");
        let mut os = FakeSpell::default();
        keep_word(&mut p, &mut os, Some(&path), "Krutrimish", NOW);
        remove_word(&mut p, &mut os, Some(&path), "Zorbo");
        let want = load_learned_state(&path);
        assert!(!want.pinned.is_empty() && !want.removed.is_empty() && !want.os_taught.is_empty());

        let bytes = crate::backup::build_bundle(&dir, "t", NOW).unwrap();
        let bundle = crate::backup::parse_bundle(&bytes).unwrap();
        let dst = dir.join("restored");
        crate::backup::apply_bundle(&bundle, &dst).unwrap();
        assert_eq!(
            load_learned_state(&dst.join("learned_lexicon.json")),
            want,
            "pinned / removed / os_taught survive a backup→restore"
        );
    }

    #[test]
    fn variant_spelling_is_refused_not_silently_kept() {
        let (_g, dir, mut p) = setup("variant");
        let mut os = FakeSpell::default();
        let r = keep_word(
            &mut p,
            &mut os,
            Some(&dir.join("learned_lexicon.json")),
            "colour",
            NOW,
        );
        assert!(!r.ok && !r.message.is_empty());
        assert!(
            os.learn_calls.is_empty(),
            "nothing is sent to macOS for a refused word"
        );
    }

    /// **Real-state migration (read-only, opt-in):** copies the real (or
    /// `TYPEASSIST_DATA_DIR`) `learned_lexicon.json` to a scratch dir, runs the same
    /// restore the engine runs at startup, and prints before/after — the live
    /// `'LooopClothing'` entry must come out clean. Never writes the real file.
    #[test]
    #[ignore]
    fn real_state_quote_migration() {
        let (_g, dir, mut p) = setup("realmig");
        let src = std::env::var_os("TYPEASSIST_DATA_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".typeassist"))
            })
            .unwrap()
            .join("learned_lexicon.json");
        let copy = dir.join("learned_lexicon.json");
        std::fs::copy(&src, &copy).expect("a learned_lexicon.json to copy");
        let before: Vec<String> = load_learned_state(&copy)
            .words
            .into_iter()
            .map(|w| w.word)
            .collect();
        p.restore_state(load_learned_state(&copy));
        lexicon_persist_if_dirty(&mut p, &copy).unwrap();
        let after: Vec<String> = load_learned_state(&copy)
            .words
            .into_iter()
            .map(|w| w.word)
            .collect();
        println!("BEFORE {before:?}\nAFTER  {after:?}");
        assert!(
            after
                .iter()
                .all(|w| !w.starts_with('\'') && !w.ends_with('\'') && !w.starts_with('"')),
            "no quoted entries remain"
        );
    }
}
