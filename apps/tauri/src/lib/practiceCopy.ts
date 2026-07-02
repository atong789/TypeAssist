// Practice Mode — ALL user-facing wording lives here.
//
// This is deliberately the single home for Practice copy (setup, "Ready?",
// "Continue?", snapshot, aria labels) so we can iterate the words in one place
// after seeing the panel in motion — never scatter strings through the
// component. Treat everything here as v1 PLACEHOLDER. Tone (per the brief and
// CLAUDE.md): reflective, not instructive; capability, never deficit; no
// scores, streaks, grades, or guilt. Sentence *content* lives separately in
// sentences.txt.

export const copy = {
  ready: {
    title: "Ready?",
    sub: "a short, calm round — no scores, no clock",
    hint: "press space or tap begin to start",
    /** aria label on the begin control */
    beginLabel: "Begin a warm-up round",
  },

  typing: {
    /** aria label on the typing surface */
    surfaceLabel:
      "Warm-up sentence. Type the words shown; a slip is marked but never scored — backspace to redo.",
    reassurance: "If you slip, it just shows — backspace to redo. Nothing is scored.",
  },

  cont: {
    title: "Continue?",
    again: "Another",
    finish: "Finish",
    hint: "press space or tap another",
    againLabel: "Warm up another sentence",
    finishLabel: "Finish and see what happened",
  },

  snapshot: {
    title: "Nicely done.",
    done: "Done",
    doneLabel: "Close warm-up",

    /** Reassurance line — capability framing (the round warmed the user's
     *  weak keys), never a score or target. Static; the numbers live in
     *  `subLine` below. */
    line: "That warmed up the keys you slip on most — a little easier next time.",

    /** Volume sub-line: an approximate word count (the motor observations
     *  folded in this round) + the sentence count. "around" keeps it a soft,
     *  reflective note, never a precise score/goal. Words null/0 → sentences
     *  only; neither → empty (hidden). */
    subLine(words: number | null, sentences: number): string {
      const parts: string[] = [];
      if (words !== null && words > 0) {
        parts.push(`around ${words} ${words === 1 ? "word" : "words"} in`);
      }
      if (sentences > 0) {
        parts.push(sentences === 1 ? "one sentence" : `${sentences} sentences`);
      }
      return parts.join(" · ");
    },

    /** Trend section header (Phase 3). Capability voice. */
    trendHeader: "where these keys are heading",
    /** Shown when there isn't enough weekly history yet for a trend. */
    trendEmpty: "the trend will appear as the weeks add up",
  },

  /** Honest empty state if the panel opens before the engine has any signal. */
  coldStart: "we'll start with some everyday words while TenCalmDigits gets to know your hands.",
} as const;
