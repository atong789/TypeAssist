//! Read-only diagnostic — letter-level confusion analysis of the learned
//! word-pattern store. **Observe-only; nothing fires, nothing is written.**
//! Aligns each `typed → target` pair and tallies the per-letter errors, so we
//! can see which single-key confusions dominate (hunting the u/i/o cluster).
//!
//! Run locally so pattern content never leaves the device:
//!   cargo run -p correction-engine --example inspect_confusions -- [path]
//! Defaults to ~/.typeassist/word_patterns.json.
//!
//! SCOPE CAVEAT: only sees pairs that passed the capture guard (edit-distance
//! ≤ 2 / length-diff ≤ 1), so single-letter confusions are captured well but
//! severe multi-letter jumbles are absent. Fine for the single-key hunt.
//!
//! The shared `edit_distance` is distance-only (no backtrace), so this
//! implements a backtracking Levenshtein alignment locally. Counts are weighted
//! by each pair's raw observation `count` (the primary number); a pair seen N
//! times contributes N to each of its confusions.

use std::collections::HashMap;
use std::path::PathBuf;

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

/// One aligned letter-level error. `intended` is the target letter, `typed` is
/// what came out.
enum Op {
    /// Wrong key: intended `a`, typed `b`.
    Sub { intended: char, typed: char },
    /// Omission: an intended letter the user didn't type.
    Drop { intended: char },
    /// Insertion: an extra letter the user typed.
    Extra { typed: char },
}

/// Backtracking Levenshtein alignment of `target` (intended) vs `typed`
/// (actual), returning the letter-level error ops. Full DP matrix so we can
/// recover the edit script (the shared helper only returns the distance).
fn align(target: &str, typed: &str) -> Vec<Op> {
    let a: Vec<char> = target.chars().collect(); // intended
    let b: Vec<char> = typed.chars().collect(); // actual
    let (m, n) = (a.len(), b.len());
    let mut cost = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=n {
        cost[0][j] = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            let sub = cost[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cost[i][j] = sub.min(cost[i - 1][j] + 1).min(cost[i][j - 1] + 1);
        }
    }

    // Backtrace (m,n) -> (0,0).
    let mut ops = Vec::new();
    let (mut i, mut j) = (m, n);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && a[i - 1] == b[j - 1] && cost[i][j] == cost[i - 1][j - 1] {
            i -= 1;
            j -= 1; // match
        } else if i > 0 && j > 0 && cost[i][j] == cost[i - 1][j - 1] + 1 {
            ops.push(Op::Sub {
                intended: a[i - 1],
                typed: b[j - 1],
            });
            i -= 1;
            j -= 1;
        } else if i > 0 && cost[i][j] == cost[i - 1][j] + 1 {
            ops.push(Op::Drop { intended: a[i - 1] }); // intended letter omitted
            i -= 1;
        } else {
            ops.push(Op::Extra { typed: b[j - 1] }); // extra letter typed
            j -= 1;
        }
    }
    ops
}

const UIO: [char; 3] = ['u', 'i', 'o'];

/// Standard touch-typing QWERTY hand split (inferred map; internal diagnostic
/// only — never surfaced in UI, per the finger-attribution caveat). u/i/o are
/// all right-hand.
fn hand_of(c: char) -> Option<&'static str> {
    match c {
        'q' | 'w' | 'e' | 'r' | 't' | 'a' | 's' | 'd' | 'f' | 'g' | 'z' | 'x' | 'c' | 'v' | 'b' => {
            Some("left")
        }
        'y' | 'u' | 'i' | 'o' | 'p' | 'h' | 'j' | 'k' | 'l' | 'n' | 'm' => Some("right"),
        _ => None,
    }
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

    // Accumulators, weighted by each pair's raw count.
    let mut subs: HashMap<(char, char), f64> = HashMap::new(); // (intended, typed) -> count
    let mut drops: HashMap<char, f64> = HashMap::new();
    let mut extras: HashMap<char, f64> = HashMap::new();
    let (mut sub_total, mut drop_total, mut extra_total) = (0.0f64, 0.0f64, 0.0f64);
    let mut by_hand: HashMap<&'static str, f64> = HashMap::new();

    for p in &raw.patterns {
        let w = p.count as f64;
        for op in align(&p.target, &p.typed) {
            match op {
                Op::Sub { intended, typed } => {
                    *subs.entry((intended, typed)).or_default() += w;
                    sub_total += w;
                    if let Some(h) = hand_of(intended) {
                        *by_hand.entry(h).or_default() += w;
                    }
                }
                Op::Drop { intended } => {
                    *drops.entry(intended).or_default() += w;
                    drop_total += w;
                }
                Op::Extra { typed } => {
                    *extras.entry(typed).or_default() += w;
                    extra_total += w;
                }
            }
        }
    }

    let r = |x: f64| x.round() as i64;
    println!(
        "# inspect_confusions — {} patterns (read-only, capture-guard ≤2)\n",
        raw.patterns.len()
    );

    // ---- 1. Top 20 substitutions ----
    println!("## 1. Top 20 letter substitutions  (intended → typed : count)");
    let mut sub_vec: Vec<_> = subs.iter().collect();
    sub_vec.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap().then(a.0.cmp(b.0)));
    for ((intended, typed), c) in sub_vec.iter().take(20) {
        println!("  {intended} → {typed} : {}", r(**c));
    }

    // ---- 2. u/i/o cluster focus ----
    println!("\n## 2. Vowel-cluster focus — u / i / o");
    let is_uio = |c: char| UIO.contains(&c);
    let mut intra = 0.0;
    let mut as_intended = 0.0; // intended u/i/o, typed something else
    let mut as_typed = 0.0; // intended other, typed u/i/o
    let mut intra_rows: Vec<((char, char), f64)> = Vec::new();
    let mut as_intended_rows: Vec<((char, char), f64)> = Vec::new();
    let mut as_typed_rows: Vec<((char, char), f64)> = Vec::new();
    for ((intended, typed), c) in &subs {
        match (is_uio(*intended), is_uio(*typed)) {
            (true, true) => {
                intra += *c;
                intra_rows.push(((*intended, *typed), *c));
            }
            (true, false) => {
                as_intended += *c;
                as_intended_rows.push(((*intended, *typed), *c));
            }
            (false, true) => {
                as_typed += *c;
                as_typed_rows.push(((*intended, *typed), *c));
            }
            (false, false) => {}
        }
    }
    let sort_rows = |v: &mut Vec<((char, char), f64)>| {
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
    };
    sort_rows(&mut intra_rows);
    sort_rows(&mut as_intended_rows);
    sort_rows(&mut as_typed_rows);
    println!(
        "  within-cluster (one of u/i/o typed for another) — total {}:",
        r(intra)
    );
    for ((a, b), c) in &intra_rows {
        println!("    {a} → {b} : {}", r(*c));
    }
    println!("  u/i/o intended, other typed — total {}:", r(as_intended));
    for ((a, b), c) in &as_intended_rows {
        println!("    {a} → {b} : {}", r(*c));
    }
    println!("  other intended, u/i/o typed — total {}:", r(as_typed));
    for ((a, b), c) in &as_typed_rows {
        println!("    {a} → {b} : {}", r(*c));
    }

    // ---- 3. Error-type totals ----
    println!("\n## 3. Error-type totals (count-weighted)");
    println!("  substitutions: {}", r(sub_total));
    println!("  dropped (omissions): {}", r(drop_total));
    println!("  extra (insertions): {}", r(extra_total));

    // ---- 4. Substitutions by hand of intended key ----
    println!("\n## 4. Substitutions by hand of intended key");
    let left = by_hand.get("left").copied().unwrap_or(0.0);
    let right = by_hand.get("right").copied().unwrap_or(0.0);
    let other = sub_total - left - right;
    println!("  left:  {}", r(left));
    println!("  right: {}  (includes the u/i/o cluster)", r(right));
    println!("  non-letter/other: {}", r(other));
}
