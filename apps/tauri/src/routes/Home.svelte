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

  <section class="warmup" aria-label="Optional warm-up">
    <div class="warmup-copy">
      <h2>A quick warm-up?</h2>
      <p>30 seconds to help me tune to your hands today. Entirely optional.</p>
    </div>
    <button class="start" on:click={() => dispatch("navigate", "warmup")}>Start</button>
  </section>

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
    color: color-mix(in srgb, canvastext 55%, transparent);
  }

  .today {
    display: flex;
    align-items: baseline;
    gap: 1rem;
    padding: 1.1rem 0;
    border-top: 1px solid color-mix(in srgb, canvastext 12%, transparent);
    border-bottom: 1px solid color-mix(in srgb, canvastext 12%, transparent);
  }

  .today-label {
    font-weight: 600;
  }

  .today-note {
    color: color-mix(in srgb, canvastext 55%, transparent);
  }

  .warmup {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1.5rem;
    padding: 1.5rem 1.75rem;
    border: 1px solid color-mix(in srgb, canvastext 14%, transparent);
    border-radius: 14px;
  }

  .warmup-copy h2 {
    margin: 0 0 0.4rem;
    font-size: 1.2rem;
    font-weight: 600;
  }

  .warmup-copy p {
    margin: 0;
    line-height: 1.5;
    color: color-mix(in srgb, canvastext 55%, transparent);
  }

  .start {
    flex-shrink: 0;
    padding: 0.6rem 1.5rem;
    font: inherit;
    font-weight: 600;
    color: inherit;
    background: transparent;
    border: 1px solid color-mix(in srgb, canvastext 22%, transparent);
    border-radius: 10px;
    cursor: pointer;
  }

  .start:hover {
    background: color-mix(in srgb, canvastext 8%, transparent);
  }

  .start:focus-visible {
    outline: 2px solid Highlight;
    outline-offset: 2px;
  }

  .status {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    margin: 0;
    font-size: 0.95rem;
    color: color-mix(in srgb, canvastext 50%, transparent);
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: #34c759;
  }
</style>
