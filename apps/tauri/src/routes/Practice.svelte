<!-- Practice — opt-in, measured. Top-level sidebar destination (no back
     arrow). Three states on one screen: level select, the round (typing),
     end-of-round growth readout. See CLAUDE.md "Insight system" → Practice
     and "Typing surfaces" → Practice. -->
<script lang="ts">
  import { tick } from "svelte";

  type Level = "gentle" | "steady" | "spirited";
  type State = "level-select" | "round" | "end";
  type TypedEntry = { ch: string; ok: boolean };
  type Segment =
    | { type: "word"; chars: { ch: string; idx: number }[] }
    | { type: "space"; idx: number };
  type KeyRow = { key: string; status: string };

  // Placeholder passages — real selection draws from the volatility map.
  // All lowercase, no Shift required (CLAUDE.md "Typing surfaces").
  // Personalised content lives on the engine side, not here.
  const PASSAGES: Record<Level, string> = {
    gentle: "happy play hello yellow",
    steady: "jolly yellow puppy happily plays",
    spirited: "enjoy the jolly loyal puppy by the playful yellow lily",
  };

  // Placeholder "vs last round" readout — the engine will compute this from
  // real per-key data. Honest framing: include at least one "still finding it".
  const keyRows: KeyRow[] = [
    { key: "j", status: "steadier than last round" },
    { key: "y", status: "gaining ground" },
    { key: "h", status: "still finding it" },
  ];

  let state: State = "level-select";
  let level: Level = "gentle";
  let typed: TypedEntry[] = [];

  let gentleEl: HTMLButtonElement;
  let passageEl: HTMLDivElement;
  let practiceAgainEl: HTMLButtonElement;

  // Focus lands on a sensible visible target on every state change — see
  // CLAUDE.md "Navigation lands focus on a sensible target". The reactive
  // block fires on initial mount too, so level-select gets a ring on the
  // Gentle card without any Tab.
  $: state, scheduleFocus();
  async function scheduleFocus() {
    await tick();
    if (state === "level-select") gentleEl?.focus();
    else if (state === "round") passageEl?.focus();
    else if (state === "end") practiceAgainEl?.focus();
  }

  function startRound(chosen: Level) {
    level = chosen;
    typed = [];
    state = "round";
  }
  function practiceAgain() {
    typed = [];
    state = "round";
  }
  function changeLevel() {
    typed = [];
    state = "level-select";
  }

  // ---------- Round state ----------

  $: targetText = PASSAGES[level];
  $: targetChars = targetText.split("");
  // Number of correctly-typed target characters; the caret sits here.
  $: nextTargetIdx = typed.reduce((n, t) => (t.ok ? n + 1 : n), 0);
  // For each target index, the list of slip characters typed while expecting
  // that target char. They render BEFORE the target char (and before the
  // caret when it's at this position), and persist until backspaced.
  $: slipsAt = computeSlipsAt(typed, targetChars);
  $: segments = computeSegments(targetChars);

  function computeSlipsAt(typedArr: TypedEntry[], chars: string[]): string[][] {
    const arr: string[][] = chars.map(() => []);
    let cur = 0;
    for (const t of typedArr) {
      if (t.ok) cur += 1;
      else if (cur < arr.length) arr[cur].push(t.ch);
    }
    return arr;
  }

  // Group target chars into word tiles + space separators so wrapping
  // happens only at the spaces between words (same model as Warm-up).
  function computeSegments(chars: string[]): Segment[] {
    const out: Segment[] = [];
    let i = 0;
    while (i < chars.length) {
      if (chars[i] === " ") {
        out.push({ type: "space", idx: i });
        i += 1;
      } else {
        const ws: { ch: string; idx: number }[] = [];
        while (i < chars.length && chars[i] !== " ") {
          ws.push({ ch: chars[i], idx: i });
          i += 1;
        }
        out.push({ type: "word", chars: ws });
      }
    }
    return out;
  }

  function onKeydown(event: KeyboardEvent) {
    // Tab is navigation, not typed input — pass to the focus trap.
    if (event.key === "Tab") return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;

    if (event.key === "Backspace") {
      // Universal backspace (CLAUDE.md "Typing surfaces"). Pops the most
      // recent entry — slip or correct — so the user can retype.
      event.preventDefault();
      typed = typed.slice(0, -1);
      return;
    }

    if (event.key.length === 1) {
      event.preventDefault();
      if (nextTargetIdx >= targetChars.length) return;
      const expected = targetChars[nextTargetIdx];
      const ok = event.key === expected;
      typed = [...typed, { ch: event.key, ok }];
      // Did the user just complete the passage? Check the fresh `typed`.
      const correct = typed.reduce((n, t) => (t.ok ? n + 1 : n), 0);
      if (correct === targetChars.length) state = "end";
      return;
    }
    // Arrows / Enter / Esc / function keys — ignore.
  }

  function levelLabel(l: Level): string {
    return l === "gentle" ? "Gentle" : l === "steady" ? "Steady" : "Spirited";
  }
</script>

{#if state === "level-select"}
  <div class="practice level-select">
    <header class="screen-header">
      <h1>Practice</h1>
    </header>
    <p class="instruction">Pick the kind of round that fits today.</p>
    <div class="levels">
      <button class="level-card" bind:this={gentleEl} on:click={() => startRound("gentle")}>
        <span class="level-icon" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <path d="M20.24 12.24a6 6 0 0 0-8.49-8.49L5 10.5V19h8.5z" />
            <line x1="16" y1="8" x2="2" y2="22" />
            <line x1="17.5" y1="15" x2="9" y2="15" />
          </svg>
        </span>
        <span class="level-text">
          <span class="level-name">Gentle</span>
          <span class="level-desc">A short, easy round — for stiff or tired hands.</span>
        </span>
      </button>

      <button class="level-card" on:click={() => startRound("steady")}>
        <span class="level-icon" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />
          </svg>
        </span>
        <span class="level-text">
          <span class="level-name">Steady</span>
          <span class="level-desc">A balanced round on your everyday tricky keys.</span>
        </span>
      </button>

      <button class="level-card" on:click={() => startRound("spirited")}>
        <span class="level-icon" aria-hidden="true">
          <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
            <polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2" />
          </svg>
        </span>
        <span class="level-text">
          <span class="level-name">Spirited</span>
          <span class="level-desc">A longer reach into the genuinely hard ones.</span>
        </span>
      </button>
    </div>
  </div>
{:else if state === "round"}
  <div class="practice round">
    <!-- Passage is first in DOM so Tab from it lands on the chevron button
         next; the header is visually placed at the top via flex `order: -1`. -->
    <div class="passage" tabindex="0" role="textbox" aria-multiline="false" aria-label={`Practice passage, ${levelLabel(level)} level. Type the words shown.`} bind:this={passageEl} on:keydown={onKeydown}>{#each segments as seg}{#if seg.type === "word"}<span class="word">{#each seg.chars as cInfo}{#each slipsAt[cInfo.idx] as slipCh}<span class="slip">{slipCh}</span>{/each}{#if cInfo.idx === nextTargetIdx}<span class="caret" aria-hidden="true"></span>{/if}<span class={cInfo.idx < nextTargetIdx ? "typed" : "untyped"}>{cInfo.ch}</span>{/each}</span>{:else}{#each slipsAt[seg.idx] as slipCh}<span class="slip">{slipCh}</span>{/each}{#if seg.idx === nextTargetIdx}<span class="caret" aria-hidden="true"></span>{/if}{' '}{/if}{/each}</div>
    <header class="screen-header round-header">
      <button class="screen-back" aria-label="Back to level select" on:click={changeLevel}>
        <svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
          <polyline points="15 18 9 12 15 6" />
        </svg>
      </button>
      <h1>Practice<span class="screen-context"> · {levelLabel(level)}</span></h1>
    </header>
    <p class="reassurance">Slipped on one? Just backspace and try it again — that's the practice.</p>
  </div>
{:else}
  <div class="practice end">
    <h1>Round done.</h1>
    <p class="subtitle">You worked the hard keys — and most are moving the right way.</p>
    <ul class="keys-list">
      {#each keyRows as row}
        <li class="key-row">
          <span class="key-name">{row.key}</span>
          <span class="key-status">{row.status}</span>
        </li>
      {/each}
    </ul>
    <div class="actions">
      <button class="action" bind:this={practiceAgainEl} on:click={practiceAgain}>Practice again</button>
      <button class="action" on:click={changeLevel}>Change level</button>
    </div>
  </div>
{/if}

<style>
  /* Centred vertically and horizontally in the panel — every state fits the
     launch window with no scrolling (CLAUDE.md "Fit the launch window"). */
  .practice {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 1.25rem;
    min-height: 100%;
    max-width: 760px;
    margin: 0 auto;
  }

  /* ---------- State 1: level select ---------- */

  /* Title uses the shared .screen-header h1 in app.css. */
  .instruction {
    margin: 0;
    color: var(--text-secondary);
    font-size: 1rem;
  }
  .levels {
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
  }
  /* Each level card is one tab stop. No sliders, no difficulty grade — the
     copy frames each as the kind of day, not a level of "skill". */
  .level-card {
    display: flex;
    align-items: center;
    gap: 1.25rem;
    width: 100%;
    padding: 1.1rem 1.4rem;
    background: transparent;
    color: inherit;
    text-align: left;
    border: 1px solid var(--hairline);
    border-radius: 14px;
    font: inherit;
    cursor: pointer;
  }
  .level-card:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  .level-card:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .level-icon {
    flex-shrink: 0;
    display: flex;
    color: color-mix(in srgb, var(--focus-ring) 75%, canvastext);
  }
  .level-text {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }
  .level-name {
    font-size: 1.1rem;
    font-weight: 600;
  }
  .level-desc {
    color: var(--text-secondary);
    font-size: 0.95rem;
  }

  /* ---------- State 2: round ---------- */

  /* Header uses the shared .screen-header + .screen-back classes. `order: -1`
     keeps it visually at the top while DOM order puts it after the passage,
     so Tab from the passage lands on the chevron next. */
  .round-header {
    order: -1;
  }
  /* Same typing surface as Warm-up: per-character feedback, words-as-tiles
     for wrapping. Differs in that wrong keys show as visible slips. */
  .passage {
    padding: 1.5rem 1.75rem;
    border: 2px solid var(--hairline);
    border-radius: 14px;
    font-size: 1.5rem;
    line-height: 1.7;
    cursor: text;
    outline: none;
  }
  .passage:focus {
    border-color: var(--focus-ring);
  }
  .word {
    white-space: nowrap;
  }
  .passage .typed {
    color: canvastext;
  }
  .passage .untyped {
    color: color-mix(in srgb, canvastext 70%, canvas);
  }
  /* A slip is amber AND wavy-underlined — the wavy underline carries the
     meaning on its own, so the slip is visible without colour perception
     (CLAUDE.md "never rely on colour alone"). Never red. */
  .passage .slip {
    color: var(--warning);
    text-decoration: underline wavy;
    text-decoration-color: var(--warning);
    text-decoration-thickness: 1.5px;
    text-underline-offset: 4px;
  }
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

  /* ---------- State 3: end ---------- */

  .end {
    align-items: center;
    text-align: center;
  }
  .end h1 {
    margin: 0;
    font-size: 1.85rem;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .end .subtitle {
    margin: 0;
    color: var(--text-secondary);
    font-size: 1rem;
  }
  .keys-list {
    width: 100%;
    max-width: 520px;
    margin: 0.5rem 0 0.25rem;
    padding: 0;
    list-style: none;
    text-align: left;
  }
  /* Same evenly-weighted, hairline-separated treatment as Progress's
     finger rows — the words carry the meaning, no colour-coding. */
  .key-row {
    display: grid;
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: 1.25rem;
    padding: 0.7rem 0;
    border-bottom: 1px solid var(--hairline);
  }
  .key-name {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 1rem;
    font-weight: 600;
  }
  .key-status {
    text-align: right;
    color: var(--text-secondary);
  }
  /* Equal-weight buttons — neither emphasised over the other. */
  .actions {
    display: flex;
    gap: 0.85rem;
    margin-top: 0.5rem;
  }
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

