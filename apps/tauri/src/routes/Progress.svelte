<!-- Progress — where the meaning lives (CLAUDE.md "Insight system" → Progress).
     Reached only from Today's "See your progress" link (deliberately not in
     the sidebar). All data here is placeholder until the engine lands.

     Tab stops in DOM order: back chevron, "See all fingers" (and the Today
     tab in the sidebar). Everything else is read-only text. Today stays lit
     in the sidebar while this view is open, so the sidebar remains
     keyboard-reachable. Layout is sized to fit the launch window without
     scrolling — read-only content is not keyboard-reachable, so it must not
     live below the fold (see CLAUDE.md "Fit the launch window"). -->
<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";

  // `navigate-back` (instead of `navigate`) signals "I'm returning to my
  // opener" — the parent restores focus to the control that opened this view
  // (see CLAUDE.md "Navigation lands focus on a sensible target").
  const dispatch = createEventDispatcher();

  // v2: Week / Month / All time range selector lives here. Hidden in v1 so
  // the screen ships with the meaning before the controls.

  // ---------- Hero ----------

  // Honest variation across 6 weeks (good and bad weeks both belong).
  const sparkPoints = [82, 84, 80, 86, 85, 88];
  const sparkW = 200;
  const sparkH = 48;
  const sparkPadX = 8;
  const sparkPadY = 5;
  const sparkMin = Math.min(...sparkPoints);
  const sparkRange = Math.max(...sparkPoints) - sparkMin || 1;
  function sparkX(i: number) {
    return sparkPadX + (i / (sparkPoints.length - 1)) * (sparkW - 2 * sparkPadX);
  }
  function sparkY(v: number) {
    return sparkH - sparkPadY - ((v - sparkMin) / sparkRange) * (sparkH - 2 * sparkPadY);
  }
  const sparkPolyline = sparkPoints.map((v, i) => `${sparkX(i)},${sparkY(v)}`).join(" ");

  // ---------- "Where your hands are gaining ground" ----------

  // Capability framing only — no red/green, no grades. Three rows by default;
  // "See all fingers" reveals more inline (no navigation, no new tab stops).
  type FingerRow = { finger: string; keys: string[]; status: string };
  const fingerRows: FingerRow[] = [
    { finger: "Right ring finger", keys: ["o", "l", "p"], status: "steadier this week" },
    { finger: "Left middle finger", keys: ["e", "d", "c"], status: "gaining ground" },
    { finger: "Right index", keys: ["h", "j", "y"], status: "still finding it" },
    { finger: "Left index", keys: ["f", "r", "v"], status: "steady" },
    { finger: "Left ring finger", keys: ["w", "s", "x"], status: "steady" },
    { finger: "Right middle finger", keys: ["i", "k", ","], status: "steady" },
    { finger: "Left pinky", keys: ["q", "a", "z"], status: "still getting to know this one" },
    { finger: "Right pinky", keys: [";", "'", "/"], status: "still getting to know this one" },
  ];

  // Cap expanded list so "See all fingers" never triggers a scroll at the
  // default launch window. v2: measure dynamically against window.innerHeight.
  const MAX_EXPANDED_ROWS = 6;

  let expanded = false;
  $: visibleRows = expanded
    ? fingerRows.slice(0, MAX_EXPANDED_ROWS)
    : fingerRows.slice(0, 3);

  function toggleExpanded() {
    expanded = !expanded;
    // Focus stays on the button naturally — its identity doesn't change.
  }

  // ---------- Focus on open ----------

  // When Progress mounts (the user arrived from Today's link), put focus on
  // the back arrow immediately so the ring is visible the moment the screen
  // appears — no first Tab needed. Relies on the always-visible :focus ring.
  let backEl: HTMLButtonElement;
  onMount(() => {
    backEl?.focus();
  });
</script>

<div class="progress">
  <header class="screen-header">
    <button
      class="screen-back"
      aria-label="Back to Today"
      bind:this={backEl}
      on:click={() => dispatch("navigate-back")}
    >
      <svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
        <polyline points="15 18 9 12 15 6" />
      </svg>
    </button>
    <h1>Progress</h1>
  </header>

  <section class="hero" aria-label="Clean rate">
    <p class="hero-eyebrow">Your typing landed clean</p>
    <div class="hero-row">
      <p class="hero-number">88%</p>
      <svg class="sparkline" viewBox="0 0 {sparkW} {sparkH}" aria-hidden="true">
        <polyline
          points={sparkPolyline}
          fill="none"
          stroke="var(--spark)"
          stroke-width="2"
          stroke-linejoin="round"
          stroke-linecap="round"
        />
        {#each sparkPoints as v, i}
          <circle cx={sparkX(i)} cy={sparkY(v)} r="2.5" fill="var(--spark)" />
        {/each}
      </svg>
    </div>
    <p class="hero-caption">
      <span aria-hidden="true" class="trend">↗</span>
      up from 82% last month · some weeks dip, that's normal
    </p>
  </section>

  <section class="cards" aria-label="This week at a glance">
    <div class="card">
      <p class="card-num">~14,000 words</p>
      <p class="card-sub">your everyday typing this week</p>
    </div>
    <div class="card">
      <p class="card-num">312 slips</p>
      <p class="card-sub">quietly smoothed for you</p>
    </div>
  </section>

  <section class="fingers" aria-labelledby="fingers-label">
    <h2 id="fingers-label" class="fingers-label">Where your hands are gaining ground</h2>
    <ul>
      {#each visibleRows as row}
        <li class="finger-row">
          <span class="finger-name">{row.finger}</span>
          <span class="finger-keys" aria-label={`keys ${row.keys.join(", ")}`}>{row.keys.join(" · ")}</span>
          <span class="finger-status">{row.status}</span>
        </li>
      {/each}
    </ul>
    <button class="see-all" aria-expanded={expanded} on:click={toggleExpanded}>
      {expanded ? "See fewer" : "See all fingers"}
    </button>
  </section>

  <!-- Footer is just the privacy line. Therapist-share is a v2 footer element. -->
  <footer class="foot">
    <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <rect x="4" y="11" width="16" height="10" rx="2" />
      <path d="M8 11V7a4 4 0 0 1 8 0v4" />
    </svg>
    Everything stays on your Mac
  </footer>
</div>

<style>
  :root {
    --spark: color-mix(in srgb, var(--focus-ring) 80%, canvastext);
  }

  .progress {
    display: flex;
    flex-direction: column;
    gap: 1.5rem;
    max-width: 920px;
    min-height: 100%;
  }

  /* Header uses the shared .screen-header + .screen-back classes. */

  /* ---------- Hero ---------- */

  .hero {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .hero-eyebrow {
    margin: 0;
    font-size: 0.95rem;
    color: var(--text-secondary);
  }
  .hero-row {
    display: flex;
    align-items: center;
    gap: 1.25rem;
  }
  .hero-number {
    margin: 0;
    font-size: 3rem;
    font-weight: 800;
    letter-spacing: -0.03em;
    line-height: 1;
  }
  .sparkline {
    width: 200px;
    height: 48px;
    flex-shrink: 0;
  }
  .hero-caption {
    margin: 0;
    font-size: 0.95rem;
    color: var(--text-secondary);
  }
  .trend {
    display: inline-block;
    color: color-mix(in srgb, var(--focus-ring) 70%, canvastext);
    font-weight: 600;
  }

  /* ---------- Stat cards ---------- */

  .cards {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
  }
  .card {
    padding: 1.1rem 1.4rem;
    /* Sunk fill: darker tile in dark mode, subtle grey in light. */
    background: rgba(0, 0, 0, 0.18);
    border-radius: 12px;
  }
  .card-num {
    margin: 0 0 0.2rem;
    font-size: 1.5rem;
    font-weight: 700;
    letter-spacing: -0.01em;
  }
  .card-sub {
    margin: 0;
    font-size: 0.9rem;
    color: var(--text-secondary);
  }

  /* ---------- Fingers section ---------- */

  .fingers {
    display: flex;
    flex-direction: column;
  }
  .fingers-label {
    margin: 0 0 0.2rem;
    padding-bottom: 0.65rem;
    border-bottom: 1px solid var(--hairline);
    font-size: 1rem;
    font-weight: 600;
    color: var(--text-secondary);
  }
  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .finger-row {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 1.25rem;
    padding: 0.7rem 0;
    border-bottom: 1px solid var(--hairline);
  }
  .finger-name {
    font-weight: 600;
  }
  .finger-keys {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.9rem;
    color: var(--text-secondary);
  }
  .finger-status {
    text-align: right;
    color: var(--text-secondary);
  }
  .see-all {
    align-self: flex-start;
    margin-top: 0.65rem;
    padding: 0.3rem 0.2rem;
    background: transparent;
    border: none;
    font: inherit;
    font-weight: 600;
    color: color-mix(in srgb, var(--focus-ring) 72%, canvastext);
    cursor: pointer;
  }
  .see-all:hover {
    text-decoration: underline;
  }
  .see-all:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 3px;
    border-radius: 4px;
  }

  /* ---------- Footer ---------- */

  .foot {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: auto;
    padding-top: 0.75rem;
    font-size: 0.9rem;
    color: var(--text-secondary);
  }
</style>
