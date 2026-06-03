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
    beginLabel: "Begin a practice round",
  },

  typing: {
    /** aria label on the typing surface */
    surfaceLabel:
      "Practice sentence. Type the words shown; a slip is marked but never scored — backspace to redo.",
    reassurance: "a slip just shows, gently — backspace to redo, nothing is scored",
  },

  cont: {
    title: "Continue?",
    again: "Another",
    finish: "Finish",
    hint: "press space or tap another",
    againLabel: "Practice another sentence",
    finishLabel: "Finish and see what happened",
  },

  snapshot: {
    title: "Nicely done.",
    again: "Again",
    done: "Done",
    againLabel: "Start another round",
    doneLabel: "Close practice",

    /** "what happened": which weak keys the round leaned into. Capability
     *  framing — these are keys the user *worked*, never keys they "failed". */
    keysLine(keys: string[]): string {
      if (keys.length === 0) return "a few calm sentences — that all counts.";
      const shown = keys.slice(0, 4).join("  ");
      return `you leaned into ${shown}`;
    },

    /** fresh observations folded into the motor map this round. Reflective —
     *  it's what the app *noticed*, not a target hit. Null while unknown. */
    observationsLine(added: number | null): string | null {
      if (added === null || added <= 0) return null;
      const noun = added === 1 ? "observation" : "observations";
      return `${added} fresh ${noun} added`;
    },

    /** sentences-completed line — a gentle volume note, never a goal/streak. */
    sentencesLine(n: number): string {
      if (n <= 0) return "";
      return n === 1 ? "one sentence" : `${n} sentences`;
    },

    /** Trend section header (Phase 3). Capability voice. */
    trendHeader: "where these keys are heading",
    /** Shown when there isn't enough weekly history yet for a trend. */
    trendEmpty: "the trend will appear as the weeks add up",
  },

  /** Honest empty state if the panel opens before the engine has any signal. */
  coldStart: "we'll start with some everyday words while TypeAssist gets to know your hands.",
} as const;
