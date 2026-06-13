<!-- Today — the daily mirror. Observational narrative + the one call to action,
     the warm-up card (offered, never insisted on; per-key detail lives in
     Progress). State-aware, reading the shared `appState`:
       Day one        — welcome + "how it works" + Start here (warm-up)
       Building back   — dateline + narrative + a gentle warm-up card
       Fluent          — dateline + narrative + a quiet warm-up card
     Copy is verbatim from the locked front-end design. -->
<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { appState } from "../lib/previewSettings";

  const dispatch = createEventDispatcher<{ navigate: string }>();

  // Dateline date, e.g. "Saturday, June 7" (locale-aware).
  const dateStr = new Date().toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });

  // One warm-up, two doors: this opens the canonical round — the menu-bar
  // Practice panel (PracticePanel.svelte) — the same flow the tray "Warm-up"
  // item opens, via the `open_practice` command.
  function startWarmup() {
    invoke("open_practice");
  }
</script>

{#if $appState === "day1"}
  <header class="screen-header"><h1>Today</h1></header>
  <p class="lede">
    Welcome. You don’t type for me — you just type, anywhere on your Mac, and I learn
    your hands as you go.
  </p>

  <div class="card">
    <div class="card-label">How it works</div>
    <!-- One visual language for a slip: the gentle dots-above + wavy underline
         marker (same as the live typing surface; never red, never a score). The
         resolution follows — a quiet muted arrow, then the calm corrected word
         in normal text. No green "correct" reward. -->
    <div
      class="demo"
      role="img"
      aria-label="A slip like “teh”, marked gently, then resolved to “the”."
    >
      <span class="demo-slip" aria-hidden="true"
        >t<span class="slip">e</span><span class="slip">h</span></span
      >
      <i class="ti ti-arrow-right demo-arrow" aria-hidden="true"></i>
      <span class="demo-fixed" aria-hidden="true">the</span>
    </div>
    <p class="card-text" style="margin: 0;">
      When you mistype and fix it yourself, I notice. That’s all it takes.
    </p>
  </div>

  <div class="card card-hero">
    <div class="card-label">Start here</div>
    <p class="card-text">
      The easiest way to begin is a quick warm-up. Have a play — I’ll start learning
      straight away.
    </p>
    <button class="btn-primary" on:click={startWarmup}>Start warm-up</button>
  </div>
{:else if $appState === "building"}
  <header class="screen-header">
    <h1>Today <span class="screen-context">· {dateStr}</span></h1>
  </header>
  <p class="lede">
    You’re building back, and it shows — your hands are steadier this week than last.
  </p>

  <div class="card card-hero">
    <div class="card-label">A gentle warm-up</div>
    <p class="card-text">
      A minute or two, weighted to the keys you’re still finding. No scores, no pressure.
    </p>
    <button class="btn-primary" on:click={startWarmup}>Start warm-up</button>
  </div>

  <button class="progress-link" on:click={() => dispatch("navigate", "progress")}>
    Want the detail? It’s all in Progress.
  </button>
{:else}
  <header class="screen-header">
    <h1>Today <span class="screen-context">· {dateStr}</span></h1>
  </header>
  <p class="lede">
    A steady afternoon. Your hands found their rhythm early and kept it, and I smoothed
    a few small slips as you went — nothing that slowed you down.
  </p>

  <div class="card">
    <div class="card-label">Warm-up</div>
    <p class="card-text">A quick warm-up, whenever you want one.</p>
    <button class="btn" on:click={startWarmup}>Start warm-up</button>
  </div>

  <button class="progress-link" on:click={() => dispatch("navigate", "progress")}>
    The full detail’s in Progress.
  </button>
{/if}

<style>
  .lede {
    margin: 0 0 1rem;
    max-width: 40rem;
    font-size: 1.12rem;
    line-height: 1.6;
    color: canvastext;
  }

  /* ---- cards ---- */
  .card {
    max-width: 40rem;
    margin: 0.4rem 0 1rem;
    padding: 1.1rem 1.25rem;
    border: 1px solid var(--hairline);
    border-radius: 12px;
    background: color-mix(in srgb, canvastext 3%, canvas);
  }
  /* Hero card — accent-tinted, for the more inviting nudge (Day one / Building). */
  .card-hero {
    background: color-mix(in srgb, var(--focus-ring) 8%, canvas);
    border-color: color-mix(in srgb, var(--focus-ring) 28%, canvas);
  }
  .card-label {
    margin-bottom: 0.5rem;
    font-size: 0.82rem;
    color: var(--text-secondary);
  }
  .card-text {
    margin: 0 0 0.9rem;
    font-size: 0.95rem;
    line-height: 1.55;
    color: canvastext;
  }

  /* ---- "how it works": [teh, marked]  →  the ---- */
  .demo {
    display: flex;
    align-items: center;
    gap: 1.1rem;
    /* Generous TOP room so the amber dots above the slipped letters clear the
       "How it works" label and never collide with it. */
    margin: 1.8rem 0 1rem;
    font-size: 1.55rem;
    line-height: 1.3;
  }
  /* The calm correction — normal text, no green/reward colour. */
  .demo-fixed {
    color: canvastext;
  }
  /* A quiet, muted arrow between the slip and its resolution. */
  .demo-arrow {
    font-size: 1.15rem;
    color: var(--text-secondary);
  }
  /* The slip marker — identical to the live typing surface: amber + wavy
     underline + a small dot above each slipped character (here, just e and h).
     Triple-coded so it reads without colour perception; never red, never a
     score. */
  .slip {
    position: relative;
    color: var(--warning);
    text-decoration: underline wavy;
    text-decoration-color: var(--warning);
    text-decoration-thickness: 1.5px;
    text-underline-offset: 4px;
  }
  .slip::before {
    content: "";
    position: absolute;
    top: -0.5em;
    left: 50%;
    transform: translateX(-50%);
    width: 4px;
    height: 4px;
    border-radius: 50%;
    background: var(--warning);
  }

  /* ---- buttons ---- */
  .btn-primary,
  .btn {
    font: inherit;
    font-size: 0.95rem;
    min-height: 38px;
    padding: 0.5rem 1.1rem;
    border-radius: 9px;
    cursor: pointer;
  }
  .btn-primary {
    border: none;
    background: var(--focus-ring);
    color: #fff;
  }
  .btn-primary:hover {
    background: color-mix(in srgb, var(--focus-ring) 88%, black);
  }
  .btn {
    background: transparent;
    border: 1px solid color-mix(in srgb, canvastext 28%, canvas);
    color: canvastext;
  }
  .btn:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  .btn-primary:focus,
  .btn:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }

  /* ---- "the detail's in Progress" pointer — an accent link to the Progress
     tab. Same treatment in every state; colour from the AA-safe --link token. */
  .progress-link {
    display: inline-block;
    margin: 0.4rem 0 0;
    padding: 0.35rem 0.1rem;
    font: inherit;
    font-size: 0.9rem;
    color: var(--link);
    background: transparent;
    border: none;
    text-align: left;
    cursor: pointer;
  }
  .progress-link:hover {
    text-decoration: underline;
  }
  .progress-link:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
    border-radius: 5px;
  }
</style>
