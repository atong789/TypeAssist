//! Read-only backtest — **does the mature motor map + lexicon propose word
//! corrections precisely/confidently enough to act on?**
//!
//! Nothing fires, nothing is written. Loads the persisted motor_map.json +
//! word_patterns.json (defaults to $TYPEASSIST_DATA_DIR or ~/.typeassist).
//!
//! Method (the methodology specified for this analysis):
//!   * Labeled set = the recorded `typed → target` correction pairs
//!     (`word_patterns.json`). `typed` = the garble, `target` = ground truth.
//!   * Candidates = lexicon words within Levenshtein ≤2 of `typed`, length-diff
//!     ≤1 (the existing motor-map / pattern-store guard), via edit1∘edit1.
//!   * MOTOR scorer: score each candidate by P(typed | candidate) under the
//!     motor map's PER-CHARACTER slip probabilities — align candidate→typed
//!     (Levenshtein) and sum log-probs:
//!         Match(c)      += ln P(produce c | intended c)   = correct_c / total_c
//!         Sub{i→j}      += ln P(produce j | intended i)   = incorrect_i[j]/total_i
//!         Ins/Del       += ln (small floor)   (the motor map has no insert/delete
//!                                              model — v0 skips them)
//!   * MOTOR+FREQ: same channel + ALPHA·log10(freq) word-frequency prior.
//!   * BASELINE: ignore the motor map — nearest edit distance, ties by frequency.
//!
//! Confidence = softmax share of the top candidate (relative dominance), the
//! same convention as the production guesser. NOT a calibrated probability.
//!
//! CAVEATS (printed at the end too): the labeled set is small + biased (only
//! words actually fixed; mostly single-obs), and the motor map was fit partly on
//! these same events — though its slip mass is a rounding error next to the
//! correct_count mass from thousands of Kept words, so motor-map overfit here is
//! minimal. Numbers are DIRECTIONAL.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use correction_engine::{classify_slip, edit1, Lexicon, SlipClass};
use serde::Deserialize;

// ---- modeling constants (reported so the reader can judge sensitivity) ------
const ALPHA: f64 = 1.0; // weight on log10(freq) in the MOTOR+FREQ / BASELINE prior
const TEMP: f64 = 1.0; // softmax temperature for the confidence score
/// Floor probability for a substitution the motor map never observed, and for
/// insert/delete ops it has no model for. Set to the map's global slip rate
/// (~0.003) so an unseen single-char error is "rare but not impossible".
const FLOOR_P: f64 = 0.003;
const MAX_EDIT: usize = 2;
const MAX_LEN_DIFF: usize = 1;
/// Confidence thresholds for the precision/coverage sweep.
const TAUS: [f64; 8] = [0.0, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9];

// ---- raw on-disk shapes -----------------------------------------------------
#[derive(Deserialize)]
struct RawPatterns {
    patterns: Vec<RawPat>,
}
#[derive(Deserialize)]
struct RawPat {
    typed: String,
    target: String,
    count: f32,
}

#[derive(Deserialize)]
struct RawMotor {
    dists: HashMap<char, RawDist>,
}
#[derive(Deserialize)]
struct RawDist {
    correct_count: f32,
    incorrect: HashMap<char, f32>,
}

/// Forward per-character channel built from the persisted motor map:
/// `p_correct[i]` = P(produce i | intended i); `p_sub[(i,j)]` = P(produce j |
/// intended i). Decay is already baked into the persisted counts.
struct Channel {
    p_correct: HashMap<char, f64>,
    p_sub: HashMap<(char, char), f64>,
    global_correct: f64,
}

impl Channel {
    fn from_raw(m: &RawMotor) -> Self {
        let mut p_correct = HashMap::new();
        let mut p_sub = HashMap::new();
        let (mut g_corr, mut g_tot) = (0.0f64, 0.0f64);
        for (&i, d) in &m.dists {
            let inc: f64 = d.incorrect.values().map(|&v| v as f64).sum();
            let total = d.correct_count as f64 + inc;
            if total <= 0.0 {
                continue;
            }
            p_correct.insert(i, d.correct_count as f64 / total);
            for (&j, &w) in &d.incorrect {
                p_sub.insert((i, j), w as f64 / total);
            }
            g_corr += d.correct_count as f64;
            g_tot += total;
        }
        let global_correct = if g_tot > 0.0 { g_corr / g_tot } else { 0.997 };
        Channel {
            p_correct,
            p_sub,
            global_correct,
        }
    }

    /// ln P(produce intended-char correctly). Unseen key → global correct rate.
    fn ln_correct(&self, c: char) -> f64 {
        self.p_correct.get(&c).copied().unwrap_or(self.global_correct).max(FLOOR_P).ln()
    }
    /// ln P(produce `j` when `i` intended). Unseen pair → FLOOR_P.
    fn ln_sub(&self, i: char, j: char) -> f64 {
        self.p_sub.get(&(i, j)).copied().unwrap_or(FLOOR_P).max(FLOOR_P).ln()
    }
}

// ---- alignment carrying the characters (intended = cand, actual = typed) ----
enum AOp {
    Match(char),
    Sub { i: char, j: char },
    Ins(char), // extra char in `typed` (cand omitted it)
    Del(char), // char in `cand` absent from `typed`
}

fn align(cand: &[char], typed: &[char]) -> Vec<AOp> {
    let (m, n) = (cand.len(), typed.len());
    let mut dp = vec![vec![0u32; n + 1]; m + 1];
    for i in 0..=m {
        dp[i][0] = i as u32;
    }
    for j in 0..=n {
        dp[0][j] = j as u32;
    }
    for i in 1..=m {
        for j in 1..=n {
            let sub = dp[i - 1][j - 1] + u32::from(cand[i - 1] != typed[j - 1]);
            dp[i][j] = sub.min(dp[i - 1][j] + 1).min(dp[i][j - 1] + 1);
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (m, n);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && cand[i - 1] == typed[j - 1] && dp[i][j] == dp[i - 1][j - 1] {
            ops.push(AOp::Match(cand[i - 1]));
            i -= 1;
            j -= 1;
        } else if i > 0 && j > 0 && dp[i][j] == dp[i - 1][j - 1] + 1 {
            ops.push(AOp::Sub {
                i: cand[i - 1],
                j: typed[j - 1],
            });
            i -= 1;
            j -= 1;
        } else if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
            ops.push(AOp::Del(cand[i - 1]));
            i -= 1;
        } else {
            ops.push(AOp::Ins(typed[j - 1]));
            j -= 1;
        }
    }
    ops
}

/// log P(typed | cand) under the motor channel.
fn channel_loglik(ch: &Channel, cand: &str, typed: &str) -> f64 {
    let c: Vec<char> = cand.chars().collect();
    let t: Vec<char> = typed.chars().collect();
    let mut s = 0.0;
    for op in align(&c, &t) {
        s += match op {
            AOp::Match(c) => ch.ln_correct(c),
            AOp::Sub { i, j } => ch.ln_sub(i, j),
            AOp::Ins(_) | AOp::Del(_) => FLOOR_P.ln(),
        };
    }
    s
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let n = b.len();
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Lexicon candidates within Levenshtein ≤2 / len-diff ≤1 of `typed` (edit1∘edit1).
fn candidates(typed: &str, lex: &Lexicon) -> Vec<String> {
    let seeds = edit1(typed);
    let mut set: HashSet<String> = HashSet::new();
    for s in &seeds {
        set.insert(s.clone());
        for s2 in edit1(s) {
            set.insert(s2);
        }
    }
    set.into_iter()
        .filter(|c| {
            c != typed
                && lex.is_known(c)
                && edit_distance(typed, c) <= MAX_EDIT
                && typed.chars().count().abs_diff(c.chars().count()) <= MAX_LEN_DIFF
        })
        .collect()
}

/// Softmax share of the top score (TEMP-scaled). 1.0 when a single candidate.
fn top_confidence(scores: &[f64]) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }
    let max = scores.iter().cloned().fold(f64::MIN, f64::max);
    let denom: f64 = scores.iter().map(|s| ((s - max) / TEMP).exp()).sum();
    (0.0_f64).exp() / denom // top == max ⇒ exp(0)=1 over denom
}

/// Per-event result for one scorer.
struct Ev {
    rank: Option<usize>, // 1-based rank of target among candidates; None if unreachable
    confidence: f64,
    count: f64,
    class: Option<SlipClass>,
    reachable: bool,
}

/// Rank `target` under a scoring closure; bigger score = better.
fn evaluate<F: Fn(&str) -> f64>(
    cands: &[String],
    target: &str,
    count: f64,
    class: Option<SlipClass>,
    score: F,
) -> Ev {
    let mut scored: Vec<(String, f64)> = cands.iter().map(|c| (c.clone(), score(c))).collect();
    // Sort by score desc, alpha tiebreak (deterministic).
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let reachable = cands.iter().any(|c| c == target);
    let rank = scored.iter().position(|(w, _)| w == target).map(|p| p + 1);
    let confidence = top_confidence(&scored.iter().map(|(_, s)| *s).collect::<Vec<_>>());
    Ev {
        rank,
        confidence,
        count,
        class,
        reachable,
    }
}

fn pct(x: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * x as f64 / d as f64
    }
}

fn report(name: &str, evs: &[Ev]) {
    let n = evs.len();
    let top1 = evs.iter().filter(|e| e.rank == Some(1)).count();
    let top3 = evs.iter().filter(|e| matches!(e.rank, Some(r) if r <= 3)).count();
    let reachable = evs.iter().filter(|e| e.reachable).count();
    let r_top1 = evs.iter().filter(|e| e.reachable && e.rank == Some(1)).count();
    let w_tot: f64 = evs.iter().map(|e| e.count).sum();
    let w_top1: f64 = evs.iter().filter(|e| e.rank == Some(1)).map(|e| e.count).sum();

    println!("### {name}");
    println!(
        "  top-1 precision:  {top1}/{n} = {:.0}%      top-3: {top3}/{n} = {:.0}%",
        pct(top1, n),
        pct(top3, n)
    );
    println!(
        "  count-weighted top-1: {:.0}%   (secondary)",
        100.0 * w_top1 / w_tot.max(1.0)
    );
    println!(
        "  target reachable in candidate set: {reachable}/{n} = {:.0}%   →  ranking-only top-1 (of reachable): {r_top1}/{reachable} = {:.0}%",
        pct(reachable, n),
        pct(r_top1, reachable)
    );
    // by slip class
    for (label, want) in [("coordination", SlipClass::Coordination), ("precision", SlipClass::Precision)] {
        let grp: Vec<&Ev> = evs.iter().filter(|e| e.class == Some(want)).collect();
        let gh = grp.iter().filter(|e| e.rank == Some(1)).count();
        println!("    {label:>13}: top-1 {gh}/{} = {:.0}%", grp.len(), pct(gh, grp.len()));
    }
}

/// Precision/coverage sweep over the confidence threshold.
fn precision_coverage(name: &str, evs: &[Ev]) {
    let n = evs.len();
    println!("### precision/coverage — {name}");
    println!("  {:>5}  {:>10}  {:>14}", "τ", "coverage", "precision|fired");
    for tau in TAUS {
        let fired: Vec<&Ev> = evs.iter().filter(|e| e.confidence >= tau).collect();
        let f = fired.len();
        let hits = fired.iter().filter(|e| e.rank == Some(1)).count();
        println!(
            "  {tau:>5.2}  {:>8.0}% ({f:>3})  {:>12.0}%",
            pct(f, n),
            pct(hits, f)
        );
    }
}

fn main() {
    let arg = std::env::args().skip(1).find(|a| !a.starts_with("--"));
    let dir = arg.map(PathBuf::from).unwrap_or_else(|| {
        std::env::var("TYPEASSIST_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").expect("HOME unset")).join(".typeassist")
            })
    });

    let motor: RawMotor =
        serde_json::from_slice(&std::fs::read(dir.join("motor_map.json")).expect("read motor_map"))
            .expect("parse motor_map");
    let pats: RawPatterns = serde_json::from_slice(
        &std::fs::read(dir.join("word_patterns.json")).expect("read word_patterns"),
    )
    .expect("parse word_patterns");
    let ch = Channel::from_raw(&motor);
    let lex = Lexicon::shared();

    // Build the scoreable event set: typed≠target, target a real word, within
    // the ≤2 / len-diff≤1 guard, and ≥1 candidate generated.
    let total_raw = pats.patterns.len();
    let mut dropped_eq = 0; // typed == target after normalization
    let mut dropped_far = 0; // edit distance > 2 or len diff > 1 (a rewrite, not a slip)
    let mut dropped_tgt_unknown = 0; // target not a real word (store noise)
    let mut dropped_no_cands = 0; // no lexicon candidate within the guard

    struct Item {
        typed: String,
        target: String,
        count: f64,
        class: Option<SlipClass>,
        cands: Vec<String>,
    }
    let mut items: Vec<Item> = Vec::new();

    for p in &pats.patterns {
        let typed = p.typed.to_ascii_lowercase();
        let target = p.target.to_ascii_lowercase();
        if typed == target {
            dropped_eq += 1;
            continue;
        }
        let d = edit_distance(&typed, &target);
        let ld = typed.chars().count().abs_diff(target.chars().count());
        if d > MAX_EDIT || ld > MAX_LEN_DIFF {
            dropped_far += 1;
            continue;
        }
        if !lex.is_known(&target) {
            dropped_tgt_unknown += 1;
            continue;
        }
        let cands = candidates(&typed, lex);
        if cands.is_empty() {
            dropped_no_cands += 1;
            continue;
        }
        items.push(Item {
            class: classify_slip(&typed, &target),
            typed,
            target,
            count: p.count as f64,
            cands,
        });
    }

    let usable = items.len();
    let avg_cands = items.iter().map(|i| i.cands.len()).sum::<usize>() as f64 / usable.max(1) as f64;

    println!("# backtest_motor_map — does the motor map + lexicon rank corrections well enough to act?\n");
    println!("## corpus");
    println!("  motor map: {} keys tracked, global correct rate {:.4} (slip {:.4})",
        motor.dists.len(), ch.global_correct, 1.0 - ch.global_correct);
    println!("  word_patterns: {total_raw} pairs");
    println!("    dropped — typed==target: {dropped_eq}");
    println!("    dropped — distance>2 / len-diff>1 (rewrites): {dropped_far}");
    println!("    dropped — target not a real word (store noise): {dropped_tgt_unknown}");
    println!("    dropped — no lexicon candidate within ≤2: {dropped_no_cands}");
    println!("  USABLE scoreable events: {usable}   (avg {avg_cands:.1} candidates/event)\n");

    // ---- scorers ----
    let motor_evs: Vec<Ev> = items
        .iter()
        .map(|it| {
            evaluate(&it.cands, &it.target, it.count, it.class, |c| {
                channel_loglik(&ch, c, &it.typed)
            })
        })
        .collect();

    let motorfreq_evs: Vec<Ev> = items
        .iter()
        .map(|it| {
            evaluate(&it.cands, &it.target, it.count, it.class, |c| {
                channel_loglik(&ch, c, &it.typed)
                    + ALPHA * ((lex.frequency(c) as f64) + 1.0).log10()
            })
        })
        .collect();

    // Baseline: nearest edit distance, ties broken by frequency. Encode as a
    // score: -1000*distance + log10(freq) so smaller distance always wins.
    let base_evs: Vec<Ev> = items
        .iter()
        .map(|it| {
            evaluate(&it.cands, &it.target, it.count, it.class, |c| {
                let d = edit_distance(&it.typed, c) as f64;
                -1000.0 * d + ((lex.frequency(c) as f64) + 1.0).log10()
            })
        })
        .collect();

    println!("## top-1 / top-3 precision\n");
    report("MOTOR channel only  (P(typed|cand) from per-char slip probs)", &motor_evs);
    println!();
    report("MOTOR channel + freq prior", &motorfreq_evs);
    println!();
    report("BASELINE  (nearest edit distance, ties by frequency — no motor map)", &base_evs);

    println!("\n## confidence sweep — is there a τ with precision ≥ ~90% at non-trivial coverage?\n");
    precision_coverage("MOTOR channel only", &motor_evs);
    println!();
    precision_coverage("MOTOR channel + freq prior", &motorfreq_evs);

    println!("\n## caveats");
    println!("  * Small, biased set ({usable} usable; 420/469 raw pairs are single-observation).");
    println!("  * In-sample: the motor map saw these corrections — but its slip mass is ~0.3% of");
    println!("    total weight (dwarfed by correct_count from Kept words), so motor overfit is minimal.");
    println!("  * Ins/Del + unseen subs use a flat floor P={FLOOR_P}; the channel has no insert/delete model.");
    println!("  * Confidence = softmax dominance share, NOT a calibrated probability.");
}
