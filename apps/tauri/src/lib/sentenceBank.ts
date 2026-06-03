// Practice Mode — sentence selection.
//
// The curriculum is "real sentences, weighted toward the user's currently weak
// keys, but with other keys appearing naturally so the sentences feel real"
// (design decision #3). We do this by SELECTING from a curated, hand-written
// bank (sentences.txt) — never by generating text. If repetition becomes a real
// problem, the fix is to grow the bank, not to add a model.
//
// Weak keys come from the engine's motor map as `weakest: [char, slip_rate][]`
// (the `engine://motor-stability` payload). A sentence's score is its *density*
// of weak-key characters; we then pick with weighted randomness so denser
// sentences are favoured without the selection ever becoming deterministic.

import raw from "./sentences.txt?raw";

/** `[character, slip_rate]`, slip_rate in [0,1]. Matches StabilityReport.weakest. */
export type WeakKey = [string, number];

/** Parsed once at module load: trimmed, comment/blank lines dropped, validated
 *  all-lowercase (Shift is a two-key chord our users should never need). A line
 *  with any uppercase or shifted punctuation is skipped with a warning rather
 *  than silently shown — a capital would force a Shift mid-practice. */
export const SENTENCES: readonly string[] = Object.freeze(
  raw
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.length > 0 && !l.startsWith("#"))
    .filter((l) => {
      const ok = l === l.toLowerCase() && !/[^a-z ]/.test(l);
      if (!ok) {
        console.warn(`[sentenceBank] skipping non-lowercase sentence: ${l}`);
      }
      return ok;
    }),
);

/** char -> slip_rate, clamped to [0,1]. Only single printable chars are kept. */
function weightsFrom(weakest: WeakKey[]): Map<string, number> {
  const m = new Map<string, number>();
  for (const [ch, rate] of weakest) {
    if (ch && ch.length === 1) m.set(ch, Math.min(1, Math.max(0, rate)));
  }
  return m;
}

/** Density of weak-key weight per character. Dividing by length favours
 *  sentences that are *concentrated* in weak keys over ones that merely happen
 *  to be long. Returns 0 when there are no weak keys (cold start). */
function density(sentence: string, weights: Map<string, number>): number {
  if (weights.size === 0 || sentence.length === 0) return 0;
  let sum = 0;
  for (const ch of sentence) sum += weights.get(ch) ?? 0;
  return sum / sentence.length;
}

/** Which weak-key characters actually appear in the sentence — drives the
 *  snapshot's "keys practiced" line. Returned in weakest-first input order. */
export function weakKeysIn(sentence: string, weakest: WeakKey[]): string[] {
  const present = new Set(sentence);
  const seen = new Set<string>();
  const out: string[] = [];
  for (const [ch] of weakest) {
    if (present.has(ch) && !seen.has(ch)) {
      seen.add(ch);
      out.push(ch);
    }
  }
  return out;
}

// Bias toward high-density sentences without ever being deterministic. `BASE`
// keeps low-density (more "ordinary") sentences in play so the set still feels
// like real language; the exponent sharpens the preference for weak-key density.
const BASE = 0.02;
const SHARPNESS = 2;

/** Pick one sentence, weighted toward the user's weak keys.
 *
 *  - Cold start (no weak keys yet, or the map isn't stable): uniform random over
 *    the bank — a sensible common set until the map has learned the user.
 *  - `exclude` lets the caller avoid immediate repeats within a session.
 */
export function pickSentence(
  weakest: WeakKey[],
  exclude: ReadonlySet<string> = new Set(),
): string {
  const pool = SENTENCES.filter((s) => !exclude.has(s));
  const candidates = pool.length > 0 ? pool : SENTENCES;
  const weights = weightsFrom(weakest);

  // Cold start / no signal: uniform pick.
  if (weights.size === 0) {
    return candidates[Math.floor(Math.random() * candidates.length)];
  }

  const scored = candidates.map(
    (s) => Math.pow(density(s, weights) + BASE, SHARPNESS),
  );
  const total = scored.reduce((a, b) => a + b, 0);
  if (total <= 0) {
    return candidates[Math.floor(Math.random() * candidates.length)];
  }

  let r = Math.random() * total;
  for (let i = 0; i < candidates.length; i++) {
    r -= scored[i];
    if (r <= 0) return candidates[i];
  }
  return candidates[candidates.length - 1]; // float-rounding fallback
}
