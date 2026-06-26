//! Per-user **guesser** — recovers the intended word from a garbled one
//! instead of memorising `typed → target` pairs (M3 accuracy-gated suggestion
//! work, Phase 1).
//!
//! This is the prototype from `examples/inspect_guesser.rs` promoted to a
//! callable module so the engine can run it **live and silently** and measure
//! how often its top guess matches the word the user actually fixes to (see
//! [`crate::guess_ledger`]). The logic is reused **as-is** — same noisy-channel
//! model, same tunable knobs, same candidate generation.
//!
//! ## HOW IT WORKS (noisy-channel)
//!
//! Learn this user's per-key error tendencies from their aligned correction
//! pairs ([`build_model`]); then for a garbled token generate plausible
//! real-word reversals (single-edit via [`crate::edit1`] + whole-hand row
//! shifts) and rank by
//!   `score = ALPHA·log10(freq) + BETA·channel_loglik(this user's errors)`.
//! [`Guess::confidence`] is a **relative dominance score** (softmax share of
//! the top candidate), NOT a calibrated probability.
//!
//! ## DELIBERATE: the motor map is NOT used
//!
//! A 2026-06-05 experiment calibrating the channel with `motor_map.json`'s
//! per-key slip rate **hurt** accuracy (top-1 49%→39%): the motor map's "slip"
//! is a *timing* flag (graze-dwell), not a correction error — its rate is
//! ~0.3% and near-uniform, so it adds no per-key discrimination and lets word
//! frequency dominate. The channel is therefore **errors-only**, derived from
//! correction pairs. Only the keyboard layout (adjacency / rows) is universal;
//! everything user-specific is learned. Do not reintroduce the motor map here.

use std::collections::{HashMap, HashSet};

use crate::{edit1, keyboard, Lexicon};

// ---- tunable knobs (carried over verbatim from the prototype) --------------
const SMOOTH_K: f64 = 0.5; // add-k so unseen edits aren't impossible
const ADJ_BONUS: f64 = 1.0; // physically-adjacent substitution gets a floor
const MATCH_BONUS: f64 = 1.5; // reward per matched char (prefers fewer edits)
const ALPHA: f64 = 1.0; // weight on word commonness (log10 freq)
const BETA: f64 = 1.0; // weight on the learned channel
const TEMP: f64 = 1.0; // softmax temperature for confidence

/// Canonical confidence cut-offs for the τ-sweep — the operating points the
/// accuracy scoreboard accumulates (see [`crate::guess_ledger`]) and the
/// offline `inspect_guesser` example prints. One source of truth so the live
/// and offline tables line up.
pub const TAU_BUCKETS: [f64; 5] = [0.5, 0.6, 0.7, 0.8, 0.9];

/// One aligned position between an intended word and what was actually typed.
pub enum Op {
    Match,
    Sub { intended: char, typed: char },
    Drop { intended: char },
    Extra { typed: char },
}

/// Backtracking Levenshtein alignment of `intended` vs `actual`, returning
/// every position (matches included — the channel scorer rewards matches).
pub fn align(intended: &str, actual: &str) -> Vec<Op> {
    let a: Vec<char> = intended.chars().collect();
    let b: Vec<char> = actual.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut cost = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in cost[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            let sub = cost[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cost[i][j] = sub.min(cost[i - 1][j] + 1).min(cost[i][j - 1] + 1);
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (m, n);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && a[i - 1] == b[j - 1] && cost[i][j] == cost[i - 1][j - 1] {
            ops.push(Op::Match);
            i -= 1;
            j -= 1;
        } else if i > 0 && j > 0 && cost[i][j] == cost[i - 1][j - 1] + 1 {
            ops.push(Op::Sub {
                intended: a[i - 1],
                typed: b[j - 1],
            });
            i -= 1;
            j -= 1;
        } else if i > 0 && cost[i][j] == cost[i - 1][j] + 1 {
            ops.push(Op::Drop { intended: a[i - 1] });
            i -= 1;
        } else {
            ops.push(Op::Extra { typed: b[j - 1] });
            j -= 1;
        }
    }
    ops
}

const ROWS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

fn hand_of(c: char) -> Option<&'static str> {
    match c {
        'q' | 'w' | 'e' | 'r' | 't' | 'a' | 's' | 'd' | 'f' | 'g' | 'z' | 'x' | 'c' | 'v' | 'b' => {
            Some("left")
        }
        'y' | 'u' | 'i' | 'o' | 'p' | 'h' | 'j' | 'k' | 'l' | 'n' | 'm' => Some("right"),
        _ => None,
    }
}

/// Same-row neighbour one position in `dir` (-1 left, +1 right), or None at a
/// row edge / for a non-letter.
fn row_neighbor(c: char, dir: i32) -> Option<char> {
    for row in ROWS {
        let chars: Vec<char> = row.chars().collect();
        if let Some(idx) = chars.iter().position(|&x| x == c) {
            let ni = idx as i32 + dir;
            if ni >= 0 && (ni as usize) < chars.len() {
                return Some(chars[ni as usize]);
            }
            return None;
        }
    }
    None
}

/// Whole-hand-offset candidates: shift each maximal contiguous same-hand run
/// (length ≥ 2) by ±1 along its keyboard row. Models loss of hand position-sense
/// where several keys drift together. (Single-key shifts are already covered by
/// [`crate::edit1`].)
fn hand_offset_candidates(typed: &str) -> Vec<String> {
    let chars: Vec<char> = typed.chars().collect();
    let hands: Vec<Option<&str>> = chars.iter().map(|&c| hand_of(c)).collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if let Some(h) = hands[i] {
            let mut j = i;
            while j + 1 < n && hands[j + 1] == Some(h) {
                j += 1;
            }
            if j > i {
                for dir in [-1i32, 1] {
                    let mut cand = chars.clone();
                    let mut ok = true;
                    for (k, slot) in cand.iter_mut().enumerate().take(j + 1).skip(i) {
                        match row_neighbor(chars[k], dir) {
                            Some(nc) => *slot = nc,
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok {
                        out.push(cand.into_iter().collect());
                    }
                }
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

/// This user's learned, **errors-only** channel: how often each kind of slip
/// happens, weighted by the (decayed) occurrence weight of the pairs it was
/// learned from. Built by [`build_model`].
#[derive(Default, Clone)]
pub struct ErrorModel {
    sub: HashMap<(char, char), f64>,
    drop: HashMap<char, f64>,
    extra: HashMap<char, f64>,
    /// Total error weight where a key is the INTENDED side (sub + drop) — for
    /// ranking the user's high-error keys (the offline diagnostic reads this).
    intended_err: HashMap<char, f64>,
}

impl ErrorModel {
    /// Total error weight (sub + drop) per INTENDED key — where the learned
    /// channel has the most signal. Read by the offline `inspect_guesser`
    /// diagnostic to bucket pairs into high-error-key vs rest.
    pub fn intended_err(&self) -> &HashMap<char, f64> {
        &self.intended_err
    }
}

/// Build the error model from correction pairs, each `(intended, actual,
/// weight)` — `intended` is the word the user meant (the target), `actual` is
/// what they typed, `weight` is the pair's (decayed) occurrence weight. The
/// caller chooses the source: live, the engine feeds the word-pattern store's
/// snapshots; offline, the example feeds `word_patterns.json` rows (and can
/// leave one out for an out-of-sample LOO read).
pub fn build_model<'a, I>(pairs: I) -> ErrorModel
where
    I: IntoIterator<Item = (&'a str, &'a str, f64)>,
{
    let mut m = ErrorModel::default();
    for (intended, actual, w) in pairs {
        for op in align(intended, actual) {
            match op {
                Op::Sub { intended, typed } => {
                    *m.sub.entry((intended, typed)).or_default() += w;
                    *m.intended_err.entry(intended).or_default() += w;
                }
                Op::Drop { intended } => {
                    *m.drop.entry(intended).or_default() += w;
                    *m.intended_err.entry(intended).or_default() += w;
                }
                Op::Extra { typed } => *m.extra.entry(typed).or_default() += w,
                Op::Match => {}
            }
        }
    }
    m
}

/// Channel log-likelihood that `cand` (hypothesised intended) became `typed`
/// under this user's learned (errors-only) tendencies.
fn channel_loglik(model: &ErrorModel, cand: &str, typed: &str) -> f64 {
    let mut s = 0.0;
    for op in align(cand, typed) {
        s += match op {
            Op::Match => MATCH_BONUS,
            Op::Sub { intended, typed } => {
                let adj = if keyboard::are_adjacent(intended, typed) {
                    ADJ_BONUS
                } else {
                    0.0
                };
                (model.sub.get(&(intended, typed)).copied().unwrap_or(0.0) + adj + SMOOTH_K).ln()
            }
            Op::Drop { intended } => {
                (model.drop.get(&intended).copied().unwrap_or(0.0) + SMOOTH_K).ln()
            }
            Op::Extra { typed } => {
                (model.extra.get(&typed).copied().unwrap_or(0.0) + SMOOTH_K).ln()
            }
        };
    }
    s
}

/// The guesser's verdict for one garbled token.
pub struct Guess {
    pub word: String,
    /// Relative dominance (softmax share of the top candidate). NOT a
    /// calibrated probability — use it only to rank operating points (τ).
    pub confidence: f64,
    /// The winning candidate was reachable ONLY via the hand-offset path (a
    /// multi-letter shift [`crate::edit1`] doesn't produce).
    pub offset_only: bool,
}

/// Recover the intended word from `typed` using the channel + lexicon. No
/// access to any target. `None` when no real-word candidate exists.
pub fn guess(model: &ErrorModel, typed: &str, lex: &Lexicon) -> Option<Guess> {
    let edit1_set: HashSet<String> = edit1(typed)
        .into_iter()
        .filter(|w| lex.is_known(w))
        .collect();
    let offset_set: HashSet<String> = hand_offset_candidates(typed)
        .into_iter()
        .filter(|w| lex.is_known(w))
        .collect();
    let mut all: HashSet<String> = edit1_set.clone();
    all.extend(offset_set.iter().cloned());
    if all.is_empty() {
        return None;
    }

    let scored: Vec<(String, f64)> = all
        .iter()
        .map(|w| {
            let freq = lex.frequency(w);
            let score =
                ALPHA * ((freq as f64) + 1.0).log10() + BETA * channel_loglik(model, w, typed);
            (w.clone(), score)
        })
        .collect();

    let max = scored.iter().map(|(_, s)| *s).fold(f64::MIN, f64::max);
    let denom: f64 = scored.iter().map(|(_, s)| ((s - max) / TEMP).exp()).sum();
    let (best, best_score) = scored
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .cloned()
        .unwrap();
    let confidence = ((best_score - max) / TEMP).exp() / denom; // top == max ⇒ 1/denom
    let offset_only = offset_set.contains(&best) && !edit1_set.contains(&best);
    Some(Guess {
        word: best,
        confidence,
        offset_only,
    })
}
