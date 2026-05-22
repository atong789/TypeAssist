<!-- Today — the daily mirror (see CLAUDE.md "Insight system"). Read-only: a
     gentle narrative readback + descriptive "what I noticed" notes. The ONLY
     interactive element is the quiet link into Progress. No timeline, no
     back-arrow. Filled vs. empty morning is a simple data-driven switch. -->
<script lang="ts">
  import { createEventDispatcher } from "svelte";

  const dispatch = createEventDispatcher<{ navigate: string }>();

  // TODO: wire to real session data. Flip to false to preview the empty morning.
  const hasData = true;

  const dateLabel = new Date().toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });

  const narrative = hasData
    ? "A steady afternoon. Your hands found their rhythm early and kept it, and I smoothed a few small slips as you went — nothing that slowed you down."
    : "The day's just beginning. I'll be paying attention as you go.";

  // Descriptive observations only — never prescriptive. Plain text, evenly
  // weighted, no per-type colour-coding (we don't want a graded/traffic-light
  // read); the hairline dividers do the separating.
  const noticed = hasData
    ? [
        "Right thumb on the spacebar was your trickiest key today.",
        "Right index drifted toward Y a few times — I caught them.",
        "Your left hand was steady all day.",
      ]
    : [];

  const emptyNote = "Nothing to share yet — insights will appear as you type through the day.";
</script>

<div class="today">
  <p class="eyebrow">Today · {dateLabel}</p>

  <p class="narrative">{narrative}</p>

  <section class="noticed" aria-labelledby="noticed-label">
    <h2 id="noticed-label" class="section-label">What I noticed</h2>
    {#if noticed.length}
      <ul>
        {#each noticed as note}
          <li>{note}</li>
        {/each}
      </ul>
    {:else}
      <p class="empty">{emptyNote}</p>
    {/if}
  </section>

  <button class="progress-link" on:click={() => dispatch("navigate", "progress")}>
    See your progress<span class="arrow" aria-hidden="true"> →</span>
  </button>
</div>

<style>
  .today {
    display: flex;
    flex-direction: column;
    gap: 2rem;
    max-width: 820px;
  }

  .eyebrow {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--text-secondary);
  }

  .narrative {
    margin: 0;
    font-size: 1.75rem;
    line-height: 1.35;
    letter-spacing: -0.01em;
  }

  .noticed {
    display: flex;
    flex-direction: column;
  }

  .section-label {
    margin: 0 0 0.25rem;
    padding-bottom: 0.85rem;
    border-bottom: 1px solid var(--hairline);
    font-size: 0.8rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--text-secondary);
  }

  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    padding: 1rem 0;
    border-bottom: 1px solid var(--hairline);
    font-size: 1.05rem;
  }

  .empty {
    margin: 0.75rem 0 0;
    color: var(--text-secondary);
  }

  /* The only interactive element on Today. Accent colour is mixed toward the
     text colour so it clears WCAG AA contrast in both light and dark mode. */
  .progress-link {
    align-self: flex-start;
    margin-top: 0.25rem;
    padding: 0.4rem 0.2rem;
    background: transparent;
    border: none;
    font: inherit;
    font-weight: 600;
    color: color-mix(in srgb, var(--focus-ring) 72%, canvastext);
    cursor: pointer;
  }
  .progress-link:hover {
    text-decoration: underline;
  }
  .progress-link:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 3px;
    border-radius: 4px;
  }
</style>
