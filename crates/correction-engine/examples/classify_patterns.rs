//! Observe-only report: run the risk-tiered, suggest-only kill-switch classifier
//! over the real `word_patterns.json` and show what it would (and would NOT)
//! suggest, why, and what changed vs the old flat-12 model.
//!
//! NOTHING is corrected — this only reads the store and classifies. Run with:
//!   cargo run -p correction-engine --example classify_patterns -- [path-to-word_patterns.json]
//! Default path: $TYPEASSIST_DATA_DIR/word_patterns.json, else ~/.typeassist/word_patterns.json

use std::path::PathBuf;

use correction_engine::{
    classify, classify_explained, single_motor_edit, Lexicon, MotorMap, PatternReadiness,
    SuggestTier, WordPatternStore, NONWORD_SOURCE_EVIDENCE_BAR, REALWORD_SOURCE_EVIDENCE_BAR,
};

fn data_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    let base = std::env::var("TYPEASSIST_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").expect("HOME unset");
            PathBuf::from(home).join(".typeassist")
        });
    base.join("word_patterns.json")
}

fn main() {
    let path = data_path();
    eprintln!("reading {}", path.display());
    let store = WordPatternStore::load_from(&path).expect("load word_patterns.json");
    let lex = Lexicon::shared();
    // Load the motor map beside word_patterns.json so the affected-key bonus is
    // in effect; empty map → no easing.
    let mm = path
        .parent()
        .map(|d| d.join("motor_map.json"))
        .filter(|p| p.exists())
        .and_then(|p| MotorMap::load_from(&p).ok())
        .unwrap_or_default();

    println!("== risk-tiered, suggest-only classifier over real data ==");
    println!(
        "bars: non-word source ≥ {NONWORD_SOURCE_EVIDENCE_BAR}, real-word source ≥ {REALWORD_SOURCE_EVIDENCE_BAR}"
    );
    println!("patterns in store: {}\n", store.len());

    let snaps = store.snapshots();

    // --- new model: who would SUGGEST? -------------------------------------
    let mut suggest_nonword: Vec<(String, String, f32)> = Vec::new();
    let mut suggest_realword: Vec<(String, String, f32)> = Vec::new();
    let mut observe_counts: std::collections::BTreeMap<String, u32> = Default::default();

    for s in &snaps {
        match classify(&s.typed, &s.target, &store, lex, &mm) {
            PatternReadiness::Suggest { tier } => match tier {
                SuggestTier::NonWordSource => {
                    suggest_nonword.push((s.typed.clone(), s.target.clone(), s.weight))
                }
                SuggestTier::RealWordSource => {
                    suggest_realword.push((s.typed.clone(), s.target.clone(), s.weight))
                }
            },
            PatternReadiness::Observe { reason } => {
                *observe_counts.entry(format!("{reason:?}")).or_default() += 1;
            }
        }
    }

    println!("--- WOULD SUGGEST: non-word source (low bar) ---");
    for (t, g, w) in &suggest_nonword {
        println!("  {t:>14} -> {g:<14} w={w:.1}");
    }
    println!("--- WOULD SUGGEST: real-word source (high bar) ---");
    if suggest_realword.is_empty() {
        println!("  (none)");
    }
    for (t, g, w) in &suggest_realword {
        println!("  {t:>14} -> {g:<14} w={w:.1}");
    }
    println!(
        "\nsuggest total: {} (non-word {}, real-word {})",
        suggest_nonword.len() + suggest_realword.len(),
        suggest_nonword.len(),
        suggest_realword.len()
    );
    println!("observe breakdown:");
    for (reason, n) in &observe_counts {
        println!("  {reason:<22} {n}");
    }

    // --- what the OLD flat-12 model would have called actionable ------------
    // Old "actionable" ≈ a motor pair (lenient classify_slip), target known,
    // weight ≥ 12, typed not a real word (Tier-1) OR typed real word (Tier-2).
    // Show which of those now DROP, and why.
    println!("\n--- entries the OLD flat-12 model would have surfaced, now reclassified ---");
    let mut dropped = 0;
    for s in &snaps {
        let old_motor = correction_engine::classify_slip(&s.typed, &s.target).is_some();
        let old_actionable = old_motor && lex.is_known(&s.target) && s.weight >= 12.0;
        if !old_actionable {
            continue;
        }
        let now = classify(&s.typed, &s.target, &store, lex, &mm);
        let verdict = match now {
            PatternReadiness::Suggest { tier } => format!("still SUGGEST({tier:?})"),
            PatternReadiness::Observe { reason } => {
                dropped += 1;
                format!("now DROPPED: {reason:?}")
            }
        };
        println!(
            "  {:>14} -> {:<14} w={:.1}  {verdict}",
            s.typed, s.target, s.weight
        );
    }
    println!("(dropped from old-actionable: {dropped})");

    // --- single-letter handling --------------------------------------------
    println!("\n--- single-letter sources/targets (never nominated) ---");
    let mut sl = 0;
    for s in &snaps {
        let tl = correction_engine::normalize_word(&s.typed).chars().count();
        let gl = correction_engine::normalize_word(&s.target).chars().count();
        if (tl == 1 || gl == 1) && s.weight >= 1.0 {
            sl += 1;
            let why = if tl == 1 { "source" } else { "target" };
            println!(
                "  {:>6} -> {:<6} w={:.1}  ({why})",
                s.typed, s.target, s.weight
            );
        }
    }
    println!("(single-letter src/tgt with w≥1: {sl})");

    // --- data-hygiene: cross-token merge suspects --------------------------
    // A merge artifact (e.g. `stroke -> strokof`, `have -> havif`) is NOT a
    // single motor error, so the new filter drops it. List high-ish-weight
    // pairs that fail the single-motor filter but DO pass the lenient
    // classify_slip guard — these are the rewrites/merges the old model leaked.
    println!("\n--- non-single-motor pairs that the lenient filter still admitted (merge/rewrite suspects) ---");
    let mut shown = 0;
    for s in &snaps {
        if s.weight < 0.8 {
            continue;
        }
        let lenient = correction_engine::classify_slip(&s.typed, &s.target).is_some();
        let strict = single_motor_edit(&s.typed, &s.target).is_some();
        if lenient && !strict {
            shown += 1;
            if shown <= 30 {
                println!("  {:>16} -> {:<16} w={:.1}", s.typed, s.target, s.weight);
            }
        }
    }
    println!("(total lenient-but-not-single-motor with w≥0.8: {shown})");

    // --- refreshed Impact ledger (what read_word_patterns now returns) -----
    // Mirrors the reconciled UI filter: include a row iff it's a genuine motor
    // slip (Suggest, or Observe{Insufficient|Stale|Brake}); drop everything
    // structural. Group coord/precis from the strict motor shape; `ready` from
    // the classifier.
    println!("\n--- REFRESHED Impact ledger (genuine motor slips only) ---");
    let mut coord: Vec<String> = Vec::new();
    let mut precis: Vec<String> = Vec::new();
    let mut old_rows = 0usize;
    for s in &snaps {
        // OLD ledger count: anything classify_slip admitted with a known target.
        if correction_engine::classify_slip(&s.typed, &s.target).is_some()
            && lex.is_known(&s.target)
        {
            old_rows += 1;
        }
        let ready = match classify(&s.typed, &s.target, &store, lex, &mm) {
            PatternReadiness::Suggest { .. } => true,
            PatternReadiness::Observe {
                reason:
                    correction_engine::ObserveReason::InsufficientEvidence
                    | correction_engine::ObserveReason::Stale
                    | correction_engine::ObserveReason::BrakeTripped,
            } => false,
            _ => continue, // dropped: swap / single-letter / non-word target / merge
        };
        let Some(edit) = single_motor_edit(&s.typed, &s.target) else {
            continue;
        };
        let mark = if ready { "READY  " } else { "observe" };
        let row = format!(
            "  [{mark}] {:>12} -> {:<12} w={:.1}",
            s.typed, s.target, s.weight
        );
        match edit {
            correction_engine::MotorEdit::Transposition => coord.push(row),
            _ => precis.push(row),
        }
    }
    println!("Coordination ({}):", coord.len());
    for r in coord.iter().take(12) {
        println!("{r}");
    }
    println!("Precision ({}):", precis.len());
    for r in precis.iter().take(12) {
        println!("{r}");
    }
    println!(
        "\nIMPACT ROWS: old lenient ledger ≈ {old_rows}  →  reconciled = {} (dropped {})",
        coord.len() + precis.len(),
        old_rows.saturating_sub(coord.len() + precis.len())
    );

    // --- Motor-Map-aware bar: who surfaces EARLY due to the affected-key bonus -
    // For every Suggest, show the affectedness score and the eased bar, and flag
    // the ones that surfaced ONLY because the bonus lowered the bar below the
    // base. These are the rows to eyeball for sharp-vs-noisy.
    println!("\n--- Motor-Map-aware bar: suggestions & affected-key easing ---");
    let mut early = 0;
    let mut total_suggest = 0;
    for s in &snaps {
        let ex = classify_explained(&s.typed, &s.target, &store, lex, &mm);
        if !matches!(ex.readiness, PatternReadiness::Suggest { .. }) {
            continue;
        }
        total_suggest += 1;
        let mark = if ex.surfaced_early {
            early += 1;
            "  <-- EARLY (affected-key bonus)"
        } else {
            ""
        };
        println!(
            "  {:>12} -> {:<12} w={:.1}  affected={:.2}  bar={:.1}/{:.1}{}",
            s.typed, s.target, s.weight, ex.affectedness, ex.bar_used, ex.base_bar, mark
        );
    }
    println!("\nSUGGEST total {total_suggest}; surfaced EARLY via affected-key bonus: {early}");
    println!(
        "(bars: non-word base {NONWORD_SOURCE_EVIDENCE_BAR} → floor {}, real-word base {REALWORD_SOURCE_EVIDENCE_BAR} → floor {NONWORD_SOURCE_EVIDENCE_BAR}; saturates at affected {})",
        correction_engine::AFFECTED_BAR_FLOOR,
        correction_engine::STRONG_AFFECTEDNESS,
    );
}
