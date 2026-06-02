# Funnel calibration passages (M2.5)

Canonical fixed passages for the **capture-integrity calibration run** (Principle #8 — the
funnel must show healthy adjacent-stage ratios before any M4 / kill-switch work). Unlike the
earlier Gate 1A/1B runs — which were **free composition**, so they could never be re-typed
identically — these are versioned, fixed texts so every future run is reproducible against the
same baseline.

## Properties (every passage)

- **Exactly 100 words**, all-lowercase, real prose, natural word boundaries (so each word seals).
- No capitals, no shifted punctuation — Shift is a hard two-key chord for our users; a
  calibration text must never require it (same rule as Warm-up).
- **Weighted toward the user's current weakest keys** so the run exercises the keys most likely
  to produce slips, while other keys still appear naturally.

## passage-001.txt

- **Derived:** 2026-06-02 from the live motor map (`~/.typeassist/motor_map.json`,
  `total_observations=4630`, 30 keys), slip rate = `incorrect / total` per key, min 20 samples.
- **Weighted toward (weakest first):** `i` (1.09%), `y` (0.82%), `o` (0.69%), `b` (0.65%),
  `p` (0.51%), `h` (0.48%), `m` (0.46%), `t` (0.45%). Dominant confusions baked into word
  choices: o→i, i→u, i→n, h→u/j, b→y/i.
- Re-derive a new passage (`passage-002`, …) when the weakest-key profile has drifted; don't
  edit an existing one — the point is a stable baseline per file.

## How to run a calibration pass

1. App running (`just dev`), `capture health -> Live` in the log.
2. **Cmd+Shift+R** — reset the funnel to zero (start a clean run).
3. Type the passage once, naturally (self-corrections are fine — they're signal).
4. **Cmd+Shift+F** — dump the `FUNNEL_DUMP` line (auto-resets / closes the run).
5. Read the adjacent-stage ratios: `received → accepted → sealed → admitted → verdict →
   observations`. For a 100-word passage, expect `sealed ≈ admitted ≈ 100` and
   `verdict ≈ sealed` (the cliff Gate 1A found was `sealed → verdict`; it must stay closed).
