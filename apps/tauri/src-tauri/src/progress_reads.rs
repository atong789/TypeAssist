//! Read-only data for the Progress UI that doesn't belong to the diary:
//!
//! * [`learned_words`] — the **learned set** (`learned_lexicon.json`): the words Jordan
//!   treats as the user's own ("Your words"). Deliberately NOT `word_freq.json` (that
//!   is the private kept-word tally, a different store).
//! * [`pattern_outcomes`] — the all-time per-pattern bubble tallies already stored by
//!   the pattern store (`shown` / `accepted` / `self_matched` / `undone`, plus `typed`
//!   and `target`), so the UI can rank "most offered, least taken"
//!   (`shown − accepted − self_matched`). Includes outcome-only pairs (a bubble shown,
//!   never observed as a correction) that the Impact ledger hides.
//!
//! Both are **pure reads**: no engine round-trip, no writes, and — unlike the loaders the
//! engine uses — a bad file is never quarantined/moved from here (the engine is the sole
//! writer of `~/.typeassist`). Anything unreadable reads as empty / untracked, never as a
//! made-up zero.

use correction_engine::{OutcomeLedger, WordPatternStore};
use serde::Serialize;
use std::path::Path;

/// One pattern's all-time tallies (decayed with the store's 30-day half-life, so they
/// are weights, not integers).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutcomeRow {
    pub typed: String,
    pub target: String,
    pub shown: f32,
    pub accepted: f32,
    pub self_matched: f32,
    pub undone: f32,
}

impl OutcomeRow {
    /// `shown − accepted − self_matched` — the "ignored" weight the UI ranks on.
    fn ignored(&self) -> f32 {
        self.shown - self.accepted - self.self_matched
    }
}

/// What `read_pattern_outcomes` returns.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PatternOutcomesRead {
    /// `"tracked"` or `"untracked"`. Untracked (tracking never started, the integrity
    /// reconcile flagged the store, or the file is unreadable) means **no number may be
    /// inferred** — `patterns` is empty and must not be shown as zeros.
    pub state: &'static str,
    /// When outcome tracking first ran on this store (epoch ms). `None` when untracked.
    pub tracking_since: Option<u64>,
    /// Most-ignored first (`shown − accepted − self_matched` descending), then
    /// `typed`, `target` for a stable order. Not truncated — the UI takes its top N.
    pub patterns: Vec<OutcomeRow>,
}

const UNTRACKED: PatternOutcomesRead = PatternOutcomesRead {
    state: "untracked",
    tracking_since: None,
    patterns: Vec::new(),
};

/// Build the read from a loaded store (the pure core; see [`pattern_outcomes`]).
pub fn outcomes_from_store(store: &WordPatternStore) -> PatternOutcomesRead {
    let OutcomeLedger::Tracked { since, rows } = store.outcome_ledger() else {
        return UNTRACKED;
    };
    let mut patterns: Vec<OutcomeRow> = rows
        .into_iter()
        .map(|r| OutcomeRow {
            typed: r.typed,
            target: r.target,
            shown: r.outcomes.shown,
            accepted: r.outcomes.accepted,
            self_matched: r.outcomes.self_matched,
            undone: r.outcomes.undone,
        })
        .collect();
    patterns.sort_by(|a, b| {
        b.ignored()
            .partial_cmp(&a.ignored())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.typed.cmp(&b.typed))
            .then_with(|| a.target.cmp(&b.target))
    });
    PatternOutcomesRead {
        state: "tracked",
        tracking_since: Some(since),
        patterns,
    }
}

/// Read `word_patterns.json` from `dir` and return its all-time outcome tallies.
/// Missing / unparseable file ⇒ untracked (never an error, never quarantined here).
pub fn pattern_outcomes(dir: &Path) -> PatternOutcomesRead {
    let Ok(bytes) = std::fs::read(dir.join("word_patterns.json")) else {
        return UNTRACKED;
    };
    match serde_json::from_slice::<WordPatternStore>(&bytes) {
        Ok(store) => outcomes_from_store(&store),
        Err(_) => UNTRACKED,
    }
}

/// The learned words from `dir/learned_lexicon.json` (case preserved, sorted
/// case-insensitively). Missing / corrupt ⇒ empty.
pub fn learned_words(dir: &Path) -> Vec<String> {
    correction_engine::read_learned_words(&dir.join(crate::backup::LEARNED_LEXICON_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use correction_engine::{
        save_learned_lexicon, Lane, LexiconProposal, MotorVerdict, OutcomeEvent, ProposalTier,
    };

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("tcd-reads-{tag}-{:?}", std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const T0: u64 = 1_800_000_000_000;

    fn proposal(word: &str) -> LexiconProposal {
        LexiconProposal {
            word: word.to_string(),
            lane: Lane::Fast,
            motor_verdict: MotorVerdict::Clean,
            tier: ProposalTier::Confirmed,
            occasions: 3,
            last_motor_evidence: None,
            last_record_id: 1,
            last_seen_ms: T0,
            plausibility: -2.0,
            proximity: correction_engine::ProximityVerdict::FarFromKnown,
            norvig_freq: 0,
            version: correction_engine::LEXICON_PROPOSAL_VERSION,
        }
    }

    // ---- 2. learned lexicon read ----

    #[test]
    fn learned_words_returns_the_persisted_learned_set() {
        let dir = tmp("lex");
        save_learned_lexicon(
            &dir.join(crate::backup::LEARNED_LEXICON_FILE),
            &[
                proposal("Soumyo"),
                proposal("kubernetis"),
                proposal("Alice"),
            ],
        )
        .unwrap();
        assert_eq!(learned_words(&dir), vec!["Alice", "kubernetis", "Soumyo"]);
    }

    #[test]
    fn learned_words_is_the_learned_set_not_the_vocabulary_tally() {
        // word_freq.json (kept-word counts) is a different store and must not leak in.
        let dir = tmp("lex-notfreq");
        std::fs::write(
            dir.join("word_freq.json"),
            br#"{"version":1,"counts":{"hello":{"count":5.0}},"last_update":1}"#,
        )
        .unwrap();
        assert!(
            learned_words(&dir).is_empty(),
            "no learned_lexicon.json ⇒ empty, regardless of word_freq"
        );
    }

    #[test]
    fn learned_words_never_mutates_the_data_dir() {
        let dir = tmp("lex-readonly");
        let f = dir.join(crate::backup::LEARNED_LEXICON_FILE);
        std::fs::write(&f, b"{ corrupt").unwrap();
        assert!(learned_words(&dir).is_empty());
        assert_eq!(
            std::fs::read(&f).unwrap(),
            b"{ corrupt",
            "a read must not quarantine or rewrite"
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names.len(), 1, "nothing was moved aside: {names:?}");
    }

    // ---- 3. all-time outcome tallies ----

    fn store_with_outcomes() -> WordPatternStore {
        use correction_engine::Outcome;
        let mut s = WordPatternStore::new();
        s.start_outcome_tracking(T0);
        // teh→the: learned, shown 4, accepted 1, self-matched 1 ⇒ ignored 2
        for _ in 0..3 {
            s.observe_correction(Outcome::CorrectedToOther, "teh", "the", T0);
        }
        for ev in [
            OutcomeEvent::Shown,
            OutcomeEvent::Shown,
            OutcomeEvent::Shown,
            OutcomeEvent::Shown,
            OutcomeEvent::Accepted,
            OutcomeEvent::SelfMatched,
            OutcomeEvent::Undone,
        ] {
            s.record_outcome(ev, "teh", "the", T0);
        }
        // ahve→have: shown 5, never taken ⇒ ignored 5 (the top repeat offender)
        for _ in 0..5 {
            s.record_outcome(OutcomeEvent::Shown, "ahve", "have", T0);
        }
        // detoure→detour: OUTCOME-ONLY (dictionary lane — never a learned correction), ignored 1
        s.record_outcome(OutcomeEvent::Shown, "detoure", "detour", T0);
        s
    }

    #[test]
    fn pattern_outcomes_returns_every_stored_tally_ranked_most_ignored_first() {
        let dir = tmp("outcomes");
        let mut store = store_with_outcomes();
        store.save_to(&dir.join("word_patterns.json")).unwrap();

        let r = pattern_outcomes(&dir);
        assert_eq!(r.state, "tracked");
        assert_eq!(r.tracking_since, Some(T0));
        let order: Vec<&str> = r.patterns.iter().map(|p| p.typed.as_str()).collect();
        assert_eq!(order, vec!["ahve", "teh", "detoure"], "5 > 2 > 1 ignored");

        let teh = r.patterns.iter().find(|p| p.typed == "teh").unwrap();
        assert_eq!(
            (
                teh.target.as_str(),
                teh.shown,
                teh.accepted,
                teh.self_matched,
                teh.undone
            ),
            ("the", 4.0, 1.0, 1.0, 1.0)
        );
        let ahve = &r.patterns[0];
        assert_eq!(
            (
                ahve.target.as_str(),
                ahve.shown,
                ahve.accepted,
                ahve.self_matched,
                ahve.undone
            ),
            ("have", 5.0, 0.0, 0.0, 0.0)
        );
        let det = r.patterns.iter().find(|p| p.typed == "detoure").unwrap();
        assert_eq!(
            (det.target.as_str(), det.shown),
            ("detour", 1.0),
            "outcome-only pairs are included"
        );
    }

    #[test]
    fn pattern_outcomes_is_untracked_never_zeros() {
        let dir = tmp("outcomes-untracked");
        // No file at all.
        assert_eq!(pattern_outcomes(&dir), UNTRACKED);
        // A store that never started outcome tracking.
        let mut s = WordPatternStore::new();
        s.observe_correction(
            correction_engine::Outcome::CorrectedToOther,
            "teh",
            "the",
            T0,
        );
        s.save_to(&dir.join("word_patterns.json")).unwrap();
        let r = pattern_outcomes(&dir);
        assert_eq!(
            (r.state, r.tracking_since, r.patterns.len()),
            ("untracked", None, 0)
        );
        // A store the integrity reconcile flagged broken.
        let mut b = store_with_outcomes();
        b.flag_outcomes_broken(T0 + 1);
        b.save_to(&dir.join("word_patterns.json")).unwrap();
        assert_eq!(pattern_outcomes(&dir).state, "untracked");
        // Unparseable file: untracked, and left exactly as found (no quarantine from a read).
        std::fs::write(dir.join("word_patterns.json"), b"nope").unwrap();
        assert_eq!(pattern_outcomes(&dir).state, "untracked");
        assert_eq!(
            std::fs::read(dir.join("word_patterns.json")).unwrap(),
            b"nope"
        );
    }

    #[test]
    fn the_three_reads_are_registered_commands_the_ui_can_invoke() {
        let lib = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
            .unwrap();
        let handler = lib
            .split("generate_handler![")
            .nth(1)
            .expect("handler list")
            .split("])")
            .next()
            .unwrap();
        let handler: String = handler.split_whitespace().collect();
        for cmd in [
            "read_resolution_diary",
            "read_learned_lexicon",
            "read_pattern_outcomes",
        ] {
            assert!(
                handler.contains(&format!("{cmd},")) || handler.ends_with(cmd),
                "{cmd} must be registered"
            );
        }
    }

    /// **Real-state smoke (read-only, opt-in):** reads the actual data dir
    /// (`TYPEASSIST_DATA_DIR`, else `~/.typeassist`) through the same three reads the UI
    /// uses and prints what they return. Run with
    /// `cargo test -p typeassist-app real_state_smoke -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_state_smoke() {
        let dir = std::env::var_os("TYPEASSIST_DATA_DIR")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".typeassist"))
            })
            .expect("a data dir");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let diary = crate::resolution_diary::read_summary(&dir, now);
        println!(
            "DIARY enabled={} tracking_since={:?} oldest_ts={:?} total_events={}",
            diary.enabled, diary.tracking_since, diary.oldest_ts, diary.total_events
        );
        let words = learned_words(&dir);
        println!(
            "LEXICON learned words = {} {:?}",
            words.len(),
            &words[..words.len().min(10)]
        );
        let o = pattern_outcomes(&dir);
        println!(
            "OUTCOMES state={} tracking_since={:?} patterns={}",
            o.state,
            o.tracking_since,
            o.patterns.len()
        );
        for p in o.patterns.iter().take(5) {
            println!(
                "  {} -> {}  shown={:.2} accepted={:.2} self_matched={:.2} undone={:.2}",
                p.typed, p.target, p.shown, p.accepted, p.self_matched, p.undone
            );
        }
        assert!(
            o.state == "tracked" || o.patterns.is_empty(),
            "untracked must carry no rows"
        );
        if let Some(t) = diary.tracking_since {
            assert!(diary.enabled, "a 'since' only exists while collecting");
            assert!(t <= now);
        }
    }

    #[test]
    fn pattern_outcomes_serializes_the_exact_fields_the_ui_reads() {
        let r = outcomes_from_store(&store_with_outcomes());
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["state"], "tracked");
        assert!(v["tracking_since"].is_u64());
        let mut keys: Vec<&str> = v["patterns"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(|k| k.as_str())
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "accepted",
                "self_matched",
                "shown",
                "target",
                "typed",
                "undone"
            ]
        );
    }
}
