//! The macOS system spelling dictionary, as a small trait so the Keep/Remove policy in
//! [`crate::teach`] is unit-tested without touching the real dictionary.
//!
//! **What it does.** `NSSpellChecker.learnWord:` adds a word to the user's per-user
//! macOS dictionary — the one every system-spellchecked text field (TextEdit, Mail,
//! Notes, Safari…) consults — so its red squiggle goes away; `unlearnWord:` takes it
//! back out and the squiggle returns. (Apps with their own dictionary — Google Docs,
//! Chrome — are not governed by this and keep their own squiggles.)
//!
//! **Why here and not in the Swift sidecar.** The call itself works from either (proven
//! by a probe before building this), but the sidecar is only alive while capture is —
//! not on first run, while paused, or after Accessibility is revoked — and Keep / Remove
//! are clicked from a Progress window that must work in all of those states. The app
//! process is always up, so it owns the call (hopped onto the main thread, which
//! AppKit expects). Layer rule intact: the correction crates (L2–L4) never call this.
//!
//! **Privacy.** Strictly local: a word goes into the Mac's own dictionary file, nothing
//! else. Only words the user explicitly Kept are ever sent (Principle #8).

/// The OS dictionary operations the teach policy needs.
pub trait SpellBackend {
    /// Does the macOS user dictionary currently hold `word`?
    fn has_learned(&mut self, word: &str) -> bool;
    /// Add `word` (squiggle goes away). `Err` if the OS didn't take it.
    fn learn(&mut self, word: &str) -> Result<(), String>;
    /// Remove `word` (squiggle returns). `Err` if the OS still holds it.
    fn unlearn(&mut self, word: &str) -> Result<(), String>;
}

#[cfg(target_os = "macos")]
pub use mac::MacSpellBackend;

#[cfg(target_os = "macos")]
mod mac {
    use super::SpellBackend;
    use objc2_app_kit::NSSpellChecker;
    use objc2_foundation::NSString;
    use std::sync::mpsc;
    use std::time::Duration;
    use tauri::{AppHandle, Runtime};

    /// Real backend: `NSSpellChecker`, run on the main thread.
    pub struct MacSpellBackend<R: Runtime> {
        app: AppHandle<R>,
    }

    impl<R: Runtime> MacSpellBackend<R> {
        pub fn new(app: AppHandle<R>) -> Self {
            Self { app }
        }

        fn on_main<T: Send + 'static>(
            &self,
            f: impl FnOnce() -> T + Send + 'static,
        ) -> Result<T, String> {
            let (tx, rx) = mpsc::channel();
            self.app
                .run_on_main_thread(move || {
                    let _ = tx.send(f());
                })
                .map_err(|e| format!("could not reach the main thread: {e}"))?;
            rx.recv_timeout(Duration::from_secs(3))
                .map_err(|_| "the system dictionary did not answer".to_string())
        }
    }

    /// The raw calls (main thread only in production). Exposed to the ignored
    /// real-dictionary test, which runs them directly.
    pub(super) fn raw_has_learned(word: &str) -> bool {
        NSSpellChecker::sharedSpellChecker().hasLearnedWord(&NSString::from_str(word))
    }
    pub(super) fn raw_learn(word: &str) -> Result<(), String> {
        let sc = NSSpellChecker::sharedSpellChecker();
        sc.learnWord(&NSString::from_str(word));
        if sc.hasLearnedWord(&NSString::from_str(word)) {
            Ok(())
        } else {
            Err("macOS did not accept the word".into())
        }
    }
    pub(super) fn raw_unlearn(word: &str) -> Result<(), String> {
        let sc = NSSpellChecker::sharedSpellChecker();
        sc.unlearnWord(&NSString::from_str(word));
        if sc.hasLearnedWord(&NSString::from_str(word)) {
            Err("macOS still holds the word".into())
        } else {
            Ok(())
        }
    }
    /// Would the system spellchecker draw a squiggle under `word`?
    #[cfg(test)]
    pub(super) fn raw_is_misspelled(word: &str) -> bool {
        NSSpellChecker::sharedSpellChecker()
            .checkSpellingOfString_startingAt(&NSString::from_str(word), 0)
            .location
            != isize::MAX as usize // NSNotFound == NSIntegerMax
    }

    impl<R: Runtime> SpellBackend for MacSpellBackend<R> {
        fn has_learned(&mut self, word: &str) -> bool {
            let w = word.to_string();
            self.on_main(move || raw_has_learned(&w)).unwrap_or(false)
        }
        fn learn(&mut self, word: &str) -> Result<(), String> {
            let w = word.to_string();
            self.on_main(move || raw_learn(&w))?
        }
        fn unlearn(&mut self, word: &str) -> Result<(), String> {
            let w = word.to_string();
            self.on_main(move || raw_unlearn(&w))?
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// **Real macOS dictionary (opt-in).** Learns a nonsense word through the same
        /// raw calls the app uses, proves the squiggle goes away, un-teaches it, proves
        /// the squiggle returns, and always cleans up. Run with
        /// `cargo test -p typeassist-app real_macos_squiggle -- --ignored --nocapture`.
        #[test]
        #[ignore]
        fn real_macos_squiggle_roundtrip() {
            let w = "zzqxteachbackrealprobe";
            let _ = raw_unlearn(w); // start clean
            assert!(
                raw_is_misspelled(w),
                "precondition: nonsense word is squiggled"
            );
            raw_learn(w).expect("learn");
            let after_learn = raw_is_misspelled(w);
            raw_unlearn(w).expect("unlearn");
            let after_unlearn = raw_is_misspelled(w);
            assert!(!after_learn, "squiggle must be gone after learnWord");
            assert!(after_unlearn, "squiggle must return after unlearnWord");
            assert!(!raw_has_learned(w), "cleaned up");
        }
    }
}

/// In-memory stand-in for the macOS dictionary, for the teach-policy tests.
#[cfg(test)]
pub mod fake {
    use super::SpellBackend;
    use std::collections::BTreeSet;

    #[derive(Default)]
    pub struct FakeSpell {
        /// What the "OS dictionary" holds.
        pub held: BTreeSet<String>,
        pub fail_learn: bool,
        pub fail_unlearn: bool,
        pub learn_calls: Vec<String>,
        pub unlearn_calls: Vec<String>,
    }

    impl SpellBackend for FakeSpell {
        fn has_learned(&mut self, word: &str) -> bool {
            self.held.contains(word)
        }
        fn learn(&mut self, word: &str) -> Result<(), String> {
            self.learn_calls.push(word.to_string());
            if self.fail_learn {
                return Err("refused".into());
            }
            self.held.insert(word.to_string());
            Ok(())
        }
        fn unlearn(&mut self, word: &str) -> Result<(), String> {
            self.unlearn_calls.push(word.to_string());
            if self.fail_unlearn {
                return Err("refused".into());
            }
            self.held.remove(word);
            Ok(())
        }
    }

    impl FakeSpell {
        /// "Is there a red squiggle?" — true unless the OS dictionary holds the word.
        pub fn squiggle(&self, word: &str) -> bool {
            !self.held.contains(word)
        }
    }
}
