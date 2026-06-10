//! Read-only diagnostic — inspect the learned word-pattern store against the
//! Step-2 auto-fire classifier. **Observe-only; nothing fires, nothing is
//! written.** A throwaway tuning tool for judging whether the personal long
//! tail clusters near the readiness threshold or is stuck at 1–2 observations,
//! and whether the provisional common-vs-personal thresholds split sensibly.
//!
//! Run locally so pattern content never leaves the device:
//!   cargo run -p correction-engine --example inspect_patterns -- [path]
//! Defaults to ~/.typeassist/word_patterns.json.

use std::path::PathBuf;

use correction_engine::{
    classify_auto_fire_for, edit_distance, AutoFireClass, Lexicon, WordPatternStore,
    TIER1_MIN_OBSERVATIONS,
};
use serde::Deserialize;

// Minimal mirror of the on-disk shape — we only need the raw lifetime `count`
// (the public snapshot exposes the decayed weight, not the raw count).
#[derive(Deserialize)]
struct RawStore {
    patterns: Vec<RawStat>,
}
#[derive(Deserialize)]
struct RawStat {
    typed: String,
    target: String,
    count: f32,
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").expect("HOME unset");
            PathBuf::from(home).join(".typeassist/word_patterns.json")
        });

    let bytes = std::fs::read(&path).expect("read word_patterns.json");
    let raw: RawStore = serde_json::from_slice(&bytes).expect("parse word_patterns.json");
    let store = WordPatternStore::load_from(&path).expect("load store");
    let lex = Lexicon::shared();

    println!(
        "# inspect_patterns — {} patterns (read-only)\n",
        raw.patterns.len()
    );

    // ---- distribution by DECAYED weight (what the readiness gate sees) ----
    // The gate is weight >= TIER1_MIN_OBSERVATIONS (12). Buckets show whether
    // the tail sits just under it (will mature) or at the floor (won't).
    let buckets = [
        (">=12 (ready)", f32::MAX, TIER1_MIN_OBSERVATIONS),
        ("8..12", TIER1_MIN_OBSERVATIONS, 8.0),
        ("4..8", 8.0, 4.0),
        ("2..4", 4.0, 2.0),
        ("<2", 2.0, 0.0),
    ];
    let weights: Vec<f32> = raw
        .patterns
        .iter()
        .filter_map(|p| store.snapshot(&p.typed, &p.target).map(|s| s.weight))
        .collect();
    println!("## decayed-weight distribution (gate = >= {TIER1_MIN_OBSERVATIONS})");
    for (label, hi, lo) in buckets {
        let n = weights.iter().filter(|&&w| w < hi && w >= lo).count();
        println!("  {label:>14}: {n}");
    }

    // ---- classifier breakdown ----
    let mut personal = 0u32;
    let mut defer = 0u32;
    let mut reasons: std::collections::BTreeMap<String, u32> = Default::default();
    for p in &raw.patterns {
        match classify_auto_fire_for(&p.typed, &p.target, &store, lex) {
            AutoFireClass::Personal => personal += 1,
            AutoFireClass::DeferToAutocorrect => defer += 1,
            AutoFireClass::NotActionable { reason } => {
                *reasons.entry(format!("{reason:?}")).or_default() += 1;
            }
        }
    }
    println!("\n## auto-fire classes");
    println!("  Personal: {personal}   DeferToAutocorrect: {defer}");
    for (r, n) in &reasons {
        println!("  NotActionable/{r}: {n}");
    }

    // ---- top patterns by raw lifetime count ----
    let mut rows: Vec<(&RawStat, f32, AutoFireClass)> = raw
        .patterns
        .iter()
        .map(|p| {
            let w = store
                .snapshot(&p.typed, &p.target)
                .map(|s| s.weight)
                .unwrap_or(0.0);
            let class = classify_auto_fire_for(&p.typed, &p.target, &store, lex);
            (p, w, class)
        })
        .collect();
    rows.sort_by(|a, b| b.0.count.partial_cmp(&a.0.count).unwrap());

    println!("\n## top 15 by raw count");
    println!(
        "  {:<22} {:>5} {:>6} {:>12} {:>4}  {}",
        "typed->target", "count", "weight", "tgt_freq", "dist", "class / reason"
    );
    for (p, w, class) in rows.iter().take(15) {
        let pair = format!("{}->{}", p.typed, p.target);
        let freq = lex.frequency(&p.target);
        let dist = edit_distance(&p.typed, &p.target);
        let verdict = match class {
            AutoFireClass::Personal => "PERSONAL".to_string(),
            AutoFireClass::DeferToAutocorrect => "defer".to_string(),
            AutoFireClass::NotActionable { reason } => format!("{reason:?}"),
        };
        println!(
            "  {pair:<22} {:>5.0} {w:>6.1} {freq:>12} {dist:>4}  {verdict}",
            p.count
        );
    }
}
