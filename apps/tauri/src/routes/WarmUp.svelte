<!-- Warm-up — opt-in, optional, no score. Two states on one screen:
     "in-progress" (typing) and "done". The warm-up CANNOT be failed: no red,
     no error flash, no timer, no accuracy %, no WPM, no streak — anywhere.
     Leaving is via the sidebar (no back-arrow; this is a top-level
     destination like Today). See CLAUDE.md "Typing surfaces" for the
     backspace-always-works rule and Warm-up's smooth-and-advance behaviour. -->
<script lang="ts">
  import { createEventDispatcher, tick } from "svelte";

  const dispatch = createEventDispatcher<{ navigate: string }>();

  // Placeholder passage — real words that quietly favour trickier keys
  // (l, o, p, y, h, j, and the spacebar). All-lowercase, no proper nouns,
  // no shifted punctuation — see CLAUDE.md "Typing surfaces" → Warm-up.
  // Later this draws from the volatility map.
  const PASSAGE = "people happily enjoy a lovely lazy holiday by the yellow harbor";
  // TODO: switch to Array.from() for Unicode-correct splitting when we wire
  // real passages from the engine. ASCII-only is fine for the placeholder.
  const chars = PASSAGE.split("");

  // Group characters into words + spaces so each word renders as a single
  // non-breaking unit. Line wrapping then happens ONLY at the spaces between
  // words — the typed half, caret, and untyped half of one word always stay
  // together on the same line.
  type Segment =
    | { type: "word"; chars: { ch: string; idx: number }[] }
    | { type: "space"; idx: number };
  const segments: Segment[] = (() => {
    const out: Segment[] = [];
    let i = 0;
    while (i < chars.length) {
      if (chars[i] === " ") {
        out.push({ type: "space", idx: i });
        i += 1;
      } else {
        const wordChars: { ch: string; idx: number }[] = [];
        while (i < chars.length && chars[i] !== " ") {
          wordChars.push({ ch: chars[i], idx: i });
          i += 1;
        }
        out.push({ type: "word", chars: wordChars });
      }
    }
    return out;
  })();

  type State = "in-progress" | "done";
  let state: State = "in-progress";

  // Number of characters typed so far. Characters to the LEFT of caretPos
  // are bright ("typed"); characters at or to the RIGHT are dim ("untyped");
  // a thin caret rides the boundary. We never render the user's actual
  // keystrokes — only the target characters — so a "wrong" key in this
  // unmeasured surface cannot produce a visible error state. See CLAUDE.md
  // "Typing surfaces" → Warm-up: smooth and advance.
  let caretPos = 0;

  let passageEl: HTMLDivElement;
  let warmAgainEl: HTMLButtonElement;

  // Focus on arrival and on every state transition: ring lands on a sensible
  // visible target so the user can start typing or see the next step
  // immediately. See CLAUDE.md "Navigation lands focus on a sensible target".
  $: state, scheduleFocus();
  async function scheduleFocus() {
    await tick();
    if (state === "done") warmAgainEl?.focus();
    else passageEl?.focus();
  }

  function onKeydown(event: KeyboardEvent) {
    // Tab is navigation, not typed input — let it pass to the focus trap.
    if (event.key === "Tab") return;
    // Don't intercept modifier-prefixed shortcuts (Cmd-W, etc.).
    if (event.metaKey || event.ctrlKey || event.altKey) return;

    if (event.key === "Backspace") {
      // Backspace always works on every typing surface (CLAUDE.md "Typing
      // surfaces"). The last typed character returns from bright to grey.
      event.preventDefault();
      if (caretPos > 0) caretPos -= 1;
      return;
    }

    // Any single printable key advances the caret one character — including
    // space. The target character appears bright; we never echo the user's
    // actual key, so a slip is silently smoothed. Real slip-vs-intent
    // judgment lands with the engine; this is the placeholder behaviour.
    if (event.key.length === 1) {
      event.preventDefault();
      if (caretPos < chars.length) {
        caretPos += 1;
        if (caretPos === chars.length) {
          state = "done";
        }
      }
      return;
    }
    // Arrows / Enter / Esc / function keys — ignore.
  }

  function restart() {
    caretPos = 0;
    state = "in-progress";
  }
</script>

{#if state === "in-progress"}
  <div class="warmup">
    <header class="screen-header">
      <h1>Warm-up</h1>
    </header>
    <p class="instruction">Type the words below to loosen up — no score, no rush.</p>

    <div class="passage" tabindex="0" role="textbox" aria-label="Warm-up passage. Type the words shown." aria-multiline="false" bind:this={passageEl} on:keydown={onKeydown}>{#each segments as seg}{#if seg.type === "word"}<span class="word">{#each seg.chars as c}{#if c.idx === caretPos}<span class="caret" aria-hidden="true"></span>{/if}<span class={c.idx < caretPos ? "typed" : "untyped"}>{c.ch}</span>{/each}</span>{:else}{#if seg.idx === caretPos}<span class="caret" aria-hidden="true"></span>{/if}{' '}{/if}{/each}</div>

    <p class="reassurance">Nothing here is scored. Slips get smoothed as you go.</p>
  </div>
{:else}
  <div class="warmup done">
    <svg class="flame" viewBox="0 0 24 24" width="42" height="42" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" stroke-linecap="round">
      <path d="M12 2c1 3 5 5 5 9.5a5 5 0 0 1-10 0c0-1.6.6-2.6 1.6-3.2 0 1.1 1 2.1 1.6 2.1 0-1.6 0-3.6 1.8-8.4z" />
      <path d="M12 13.5c.8.9 1.6 1.8 1.6 3a1.6 1.6 0 0 1-3.2 0c0-.9.4-1.4.8-1.8 0 .5.4.9.8.9 0-.5 0-1.3 0-2.1z" />
    </svg>
    <h1>Nicely warmed up.</h1>
    <p class="subtitle">Your hands are ready for the day.</p>
    <div class="actions">
      <button class="action" bind:this={warmAgainEl} on:click={restart}>Warm up again</button>
      <button class="action" on:click={() => dispatch("navigate", "practice")}>Start practicing</button>
    </div>
  </div>
{/if}

<style>
  /* Whole screen is centred vertically and horizontally inside the panel,
     so all content sits within the launch window without scrolling
     (see CLAUDE.md "Fit the launch window"). */
  .warmup {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 1.25rem;
    min-height: 100%;
    max-width: 880px;
    margin: 0 auto;
  }

  /* ---------- In-progress state ---------- */

  .instruction {
    margin: 0;
    color: var(--text-secondary);
    font-size: 1rem;
  }

  /* The passage is the only content-area tab stop. Its focus indicator is the
     border colour change (not the global outline) so it reads as a single
     clean blue box, matching the design. */
  .passage {
    padding: 1.5rem 1.75rem;
    border: 2px solid var(--hairline);
    border-radius: 14px;
    font-size: 1.5rem;
    line-height: 1.6;
    cursor: text;
    outline: none;
    /* Default white-space (normal) on the parent. Each .word below is nowrap,
       so a single word tile can't be cut; the explicit space text nodes
       between tiles render visibly and serve as the only line-break
       opportunities. */
  }
  .passage:focus {
    border-color: var(--focus-ring);
  }
  /* Each word is non-breaking so wrapping happens only at the spaces between
     words. The parent stays white-space: pre-wrap, so spaces between words
     remain wrap opportunities; the inline-block caret inside a nowrap word
     can no longer create a mid-word break. */
  .word {
    white-space: nowrap;
  }

  /* Two tiers — both meet WCAG AA contrast in light and dark mode. */
  .passage .typed {
    color: canvastext;
  }
  .passage .untyped {
    color: color-mix(in srgb, canvastext 70%, canvas);
  }

  /* Static caret marker rides the boundary between typed and untyped. The
     2px width slots between adjacent character spans without shifting any
     other character's final position. */
  .caret {
    display: inline-block;
    width: 2px;
    height: 1.15em;
    background: var(--focus-ring);
    vertical-align: text-bottom;
  }

  .reassurance {
    margin: 0;
    color: color-mix(in srgb, canvastext 55%, canvas);
    font-size: 0.9rem;
  }

  /* ---------- Done state ---------- */

  .warmup.done {
    align-items: center;
    text-align: center;
    gap: 0.5rem;
  }
  .flame {
    color: color-mix(in srgb, var(--focus-ring) 80%, canvastext);
    margin-bottom: 0.5rem;
  }
  .done h1 {
    margin: 0;
    font-size: 1.85rem;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .done .subtitle {
    margin: 0;
    color: var(--text-secondary);
    font-size: 1rem;
  }
  .actions {
    display: flex;
    gap: 0.85rem;
    margin-top: 1.25rem;
  }
  /* Equal-weight buttons — neither emphasised over the other. */
  .action {
    min-height: 44px;
    padding: 0.55rem 1.4rem;
    font: inherit;
    font-weight: 600;
    color: inherit;
    background: transparent;
    border: 1px solid color-mix(in srgb, canvastext 28%, canvas);
    border-radius: 10px;
    cursor: pointer;
  }
  .action:hover {
    background: color-mix(in srgb, canvastext 8%, canvas);
  }
  .action:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
</style>
