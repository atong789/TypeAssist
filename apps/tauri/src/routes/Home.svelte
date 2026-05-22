<!-- Home view — a quiet landing. Greets the user, reflects today at a glance,
     and offers (never insists on) a warm-up. Voice is warm and human; copy
     says "helping", never "fixing". See CLAUDE.md product principles. -->
<script lang="ts">
  import { createEventDispatcher } from "svelte";

  const dispatch = createEventDispatcher<{ navigate: string }>();

  // TODO: wire to the real user name later.
  const name = "Soumyo";

  function greeting(): string {
    const hour = new Date().getHours();
    if (hour < 12) return "Good morning";
    if (hour < 18) return "Good afternoon";
    return "Good evening";
  }

  // TODO: wire to real session data from the engine later.
  const todayNote = "A light morning so far. Your rhythm is steady.";
</script>

<div class="home">
  <header>
    <h1>{greeting()}, {name}.</h1>
    <p class="subline">TypeAssist is quietly helping as you type, everywhere on your Mac.</p>
  </header>

  <div class="today">
    <span class="today-label">Today</span>
    <span class="today-note">{todayNote}</span>
  </div>

  <!-- The whole card is one large, mouse-forgiving button: click anywhere to
       start. The "Start" pill is a visible affordance only — the card itself
       is the control, so there is a single tab stop, activatable with
       Enter and Space. Keeping it a <button> (with phrasing-content spans, no
       nested button or heading) keeps the markup valid and accessible. -->
  <button class="warmup" on:click={() => dispatch("navigate", "warmup")}>
    <span class="warmup-copy">
      <span class="warmup-title">A quick warm-up?</span>
      <span class="warmup-desc">30 seconds to help me tune to your hands today. Entirely optional.</span>
    </span>
    <span class="start" aria-hidden="true">Start</span>
  </button>

  <p class="status">
    <span class="dot" aria-hidden="true"></span>
    TypeAssist is active and helping
  </p>
</div>

<style>
  .home {
    display: flex;
    flex-direction: column;
    gap: 2.5rem;
    max-width: 880px;
  }

  header h1 {
    margin: 0 0 0.6rem;
    font-size: 2.5rem;
    font-weight: 700;
    letter-spacing: -0.02em;
  }

  .subline {
    margin: 0;
    font-size: 1.25rem;
    color: var(--text-secondary);
  }

  .today {
    display: flex;
    align-items: baseline;
    gap: 1rem;
    padding: 1.1rem 0;
    border-top: 1px solid var(--hairline);
    border-bottom: 1px solid var(--hairline);
  }

  .today-label {
    font-weight: 600;
  }

  .today-note {
    color: var(--text-secondary);
  }

  /* The entire card is the button. Reset native button styling and lay it out
     like a card; large target spanning the content width. */
  .warmup {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1.5rem;
    width: 100%;
    padding: 1.5rem 1.75rem;
    font: inherit;
    color: inherit;
    text-align: left;
    background: transparent;
    border: 1px solid var(--hairline);
    border-radius: 14px;
    cursor: pointer;
  }

  .warmup:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }

  .warmup:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 3px;
  }

  .warmup-copy {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
  }

  .warmup-title {
    font-size: 1.2rem;
    font-weight: 600;
  }

  .warmup-desc {
    line-height: 1.5;
    color: var(--text-secondary);
  }

  /* Visible affordance only (aria-hidden); the card around it is the control. */
  .start {
    flex-shrink: 0;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-height: 44px;
    padding: 0.6rem 1.5rem;
    font-weight: 600;
    border: 1px solid color-mix(in srgb, canvastext 28%, canvas);
    border-radius: 10px;
  }

  .warmup:hover .start {
    background: color-mix(in srgb, canvastext 10%, canvas);
  }

  .status {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    margin: 0;
    font-size: 0.95rem;
    color: var(--text-secondary);
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: #34c759;
  }
</style>
