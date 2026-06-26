// Throwaway diagnostic: for each clean-slate non-word, print the single-motor-edit
// dictionary candidates with their Norvig unigram frequency, and the convergence
// verdict (the bold/first-sighting lane). Shows whether a dominant candidate exists.
use correction_engine::{shadow_convergence_scan, Lexicon, MotorMap};

fn main() {
    let lex = Lexicon::shared();
    let mm = MotorMap::new();
    for typed in ["teh", "waht", "haev", "nootes", "whcih", "tje", "becuase", "thier"] {
        let scan = shadow_convergence_scan(typed, lex, &mm);
        let mut ranked: Vec<(String, u64)> = scan
            .candidates
            .iter()
            .map(|c| (c.clone(), lex.frequency(c)))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        println!(
            "{:>8}  convergent={:<5}  n={}  ranked={:?}",
            typed,
            scan.convergent,
            scan.candidates.len(),
            ranked,
        );
    }
}
