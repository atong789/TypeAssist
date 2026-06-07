//! Read-only diagnostic — test the per-user *guesser* that recovers the intended
//! word from a garbled one, instead of memorizing typed→target pairs.
//! **Observe-only; nothing fires, nothing is written; live correction stays
//! OFF.** Runs locally; defaults to ~/.typeassist/word_patterns.json (path arg
//! allowed). Add `--examples` to print a few real-word example recoveries
//! (off by default to keep output word-free).
//!
//! The guessing logic now lives in `correction_engine::guesser` (so the engine
//! can run the same model live and silently — see `guess_ledger`). This example
//! is just the offline evaluation harness over `word_patterns.json`: it does the
//! leave-one-out (LOO) sweep and prints the operating-point table.
//!
//! HOW IT WORKS (noisy-channel): learn this user's per-key error tendencies
//! from the aligned pairs, then for a garbled token generate plausible real-word
//! reversals (single-edit + whole-hand row shifts), and rank by
//!   score = ALPHA·log10(freq) + BETA·channel_loglik(this user's error pattern).
//!
//! CONFIDENCE IS A RELATIVE DOMINANCE SCORE (softmax share of the top
//! candidate), NOT a calibrated probability. The channel is ERRORS-ONLY
//! (relative tendencies from word_patterns.json).
//!
//! TRIED + REJECTED (2026-06-05): calibrating the channel with motor_map.json's
//! per-key slip rate HURT accuracy (top-1 49%→39%; hit% down, false-fix up at
//! every τ). Root cause: the motor map's "slip" is a TIMING flag (graze-dwell),
//! not a correction error — its global rate is ~0.3% and near-uniform, so it
//! adds no per-key discrimination and lets word-frequency dominate. If true
//! per-key error rates are ever wanted, derive them from CORRECTION EVENTS, not
//! the motor map's timing-slip flag. Not now.
//!
//! LOO accuracy here is DIRECTIONAL (~109 pairs, many seen once) — read it
//! promising-or-not, not as a final number. Only the keyboard layout
//! (adjacency / rows) is universal; everything user-specific is learned.

use std::collections::HashSet;
use std::path::PathBuf;

use correction_engine::guesser::{align, build_model, guess, Op, TAU_BUCKETS};
use correction_engine::Lexicon;
use serde::Deserialize;

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

/// Pairs as the model wants them: `(intended = target, actual = typed, weight)`,
/// optionally leaving one index out for an out-of-sample LOO read.
fn pairs(pats: &[RawStat], skip: Option<usize>) -> impl Iterator<Item = (&str, &str, f64)> {
    pats.iter().enumerate().filter_map(move |(idx, p)| {
        if Some(idx) == skip {
            None
        } else {
            Some((p.target.as_str(), p.typed.as_str(), p.count as f64))
        }
    })
}

/// Intended-side chars touched by an error op (sub/drop) — for the high-error
/// key bucketing. Replaces the inline match this example used to carry.
fn intended_error_chars(target: &str, typed: &str) -> Vec<char> {
    align(target, typed)
        .iter()
        .filter_map(|op| match op {
            Op::Sub { intended, .. } | Op::Drop { intended } => Some(*intended),
            _ => None,
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let show_examples = args.iter().any(|a| a == "--examples");
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").expect("HOME unset");
            PathBuf::from(home).join(".typeassist/word_patterns.json")
        });

    let bytes = std::fs::read(&path).expect("read word_patterns.json");
    let raw: RawStore = serde_json::from_slice(&bytes).expect("parse word_patterns.json");
    let lex = Lexicon::shared();
    let n = raw.patterns.len();

    // High-error keys = top quartile of intended keys by error weight (full
    // model) — where the learned channel has the most signal.
    let full = build_model(pairs(&raw.patterns, None));
    let mut keys: Vec<(char, f64)> = full.intended_err().iter().map(|(&k, &v)| (k, v)).collect();
    keys.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let high_n = (keys.len() as f64 * 0.25).ceil() as usize;
    let high_keys: HashSet<char> = keys.iter().take(high_n).map(|(k, _)| *k).collect();

    struct Eval {
        hit: bool,
        confidence: f64,
        offset_only: bool,
        high: bool,
        count: f64,
    }
    let mut evals: Vec<Eval> = Vec::with_capacity(n);
    let mut examples: Vec<(String, String)> = Vec::new(); // typed, guess
    for (idx, p) in raw.patterns.iter().enumerate() {
        let model = build_model(pairs(&raw.patterns, Some(idx)));
        let g = guess(&model, &p.typed, lex);
        // A pair is "high" if any of its error ops touches a high-error key.
        let high = intended_error_chars(&p.target, &p.typed)
            .iter()
            .any(|c| high_keys.contains(c));
        let (hit, confidence, offset_only) = match &g {
            Some(g) => (g.word == p.target, g.confidence, g.offset_only),
            None => (false, 0.0, false),
        };
        if show_examples && hit && examples.len() < 5 {
            examples.push((p.typed.clone(), p.target.clone()));
        }
        evals.push(Eval {
            hit,
            confidence,
            offset_only,
            high,
            count: p.count as f64,
        });
    }

    let pct = |x: usize, d: usize| {
        if d == 0 {
            0.0
        } else {
            100.0 * x as f64 / d as f64
        }
    };

    println!("# inspect_guesser — {n} distinct pairs (read-only, LOO, DIRECTIONAL ~109 pairs)");
    println!("# errors-only channel; confidence = softmax-share (relative dominance)\n");

    // Overall top-1 accuracy (τ-independent).
    let hits = evals.iter().filter(|e| e.hit).count();
    let w_total: f64 = evals.iter().map(|e| e.count).sum();
    let w_hits: f64 = evals.iter().filter(|e| e.hit).map(|e| e.count).sum();
    println!("## top-1 accuracy (any confidence)");
    println!("  per-pair:       {hits}/{n} = {:.0}%", pct(hits, n));
    println!(
        "  count-weighted: {:.0}%  (secondary)\n",
        100.0 * w_hits / w_total.max(1.0)
    );

    // τ-sweep: the operating-point table. False-fix = confident but wrong.
    println!("## τ-sweep — pick the operating point (wrong fix ≫ worse than a miss)");
    println!(
        "  {:>4}  {:>7}  {:>12}  {:>16}  {:>15}",
        "τ", "fired%", "hit%|fired", "false-fix%|fired", "false-fix%|all"
    );
    for tau in TAU_BUCKETS {
        let fired: Vec<&Eval> = evals.iter().filter(|e| e.confidence >= tau).collect();
        let f = fired.len();
        let fired_hits = fired.iter().filter(|e| e.hit).count();
        let fired_wrong = f - fired_hits;
        println!(
            "  {tau:>4.1}  {:>6.0}%  {:>11.0}%  {:>15.0}%  {:>14.1}%",
            pct(f, n),
            pct(fired_hits, f),
            pct(fired_wrong, f),
            100.0 * fired_wrong as f64 / n.max(1) as f64
        );
    }

    // Breakdown: high-error keys vs rest.
    println!("\n## top-1 accuracy by key class");
    for (label, want_high) in [("high-error keys", true), ("rest", false)] {
        let grp: Vec<&Eval> = evals.iter().filter(|e| e.high == want_high).collect();
        let gh = grp.iter().filter(|e| e.hit).count();
        println!(
            "  {label:>16}: {gh}/{} = {:.0}%",
            grp.len(),
            pct(gh, grp.len())
        );
    }
    println!(
        "  (high-error key set = top quartile by intended-error weight, {} keys)",
        high_keys.len()
    );

    // Hand-offset path.
    let off: Vec<&Eval> = evals.iter().filter(|e| e.offset_only).collect();
    let off_hits = off.iter().filter(|e| e.hit).count();
    println!("\n## whole-hand-offset path");
    println!(
        "  winning candidate came only from the offset path: {} pairs ({} correct)",
        off.len(),
        off_hits
    );
    println!("  (expected ~0 here — severe jumbles aren't in capture-guard ≤2 data)");

    if show_examples {
        println!("\n## example recoveries (CONTAIN REAL WORDS)");
        for (typed, guess) in &examples {
            println!("  {typed} → {guess}");
        }
    } else {
        println!("\n(omitted example guesses to stay word-free; pass --examples to show a few)");
    }
}
