<!-- Settings — discrete card selectors only (no sliders, ever).
     Today: the Practice word bank's spelling/locale. Native radios styled as
     cards give the full WAI-ARIA radiogroup keyboard behaviour for free (Tab to
     the group, arrows move + select), with the focus ring shown on the card via
     :focus-within. The choice persists to localStorage and is read by the
     separate Practice window at session start (same origin → shared storage). -->
<script lang="ts">
  import { onMount } from "svelte";
  import {
    loadSpellingPref,
    saveSpellingPref,
    type SpellingPref,
  } from "../lib/locale";

  let pref: SpellingPref = "system";
  // Load is async-on-mount, so guard the persist reaction until after it — the
  // initial read must not immediately rewrite the stored value with the default.
  let loaded = false;
  onMount(() => {
    pref = loadSpellingPref();
    loaded = true;
  });
  $: if (loaded) saveSpellingPref(pref);

  const options: { value: SpellingPref; title: string; sub: string }[] = [
    { value: "system", title: "Follow system", sub: "Match your Mac's language & region" },
    { value: "en-US", title: "English (US)", sub: "American spelling — color, gray" },
    { value: "en-GB", title: "English (UK)", sub: "British spelling — colour, grey" },
    { value: "en-IN", title: "English (India)", sub: "British spelling" },
  ];
</script>

<header class="screen-header"><h1>Settings</h1></header>

<section class="group">
  <h2 class="group-title">Warm-up language</h2>
  <p class="group-help">Which spelling the Warm-up word bank uses.</p>
  <fieldset class="card-group">
    <legend class="sr-only">Warm-up language</legend>
    {#each options as o}
      <label class="opt-card" class:selected={pref === o.value}>
        <input type="radio" name="practice-spelling" value={o.value} bind:group={pref} />
        <span class="opt-title">{o.title}</span>
        <span class="opt-sub">{o.sub}</span>
      </label>
    {/each}
  </fieldset>
</section>

<style>
  .group {
    margin-top: 1.25rem;
    max-width: 32rem;
  }
  .group-title {
    margin: 0 0 0.2rem;
    font-size: 1rem;
    font-weight: 600;
  }
  .group-help {
    margin: 0 0 0.85rem;
    color: var(--text-secondary);
    font-size: 0.9rem;
  }

  .card-group {
    border: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  /* Visually hidden but still focusable — the card carries the visible state. */
  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
    border: 0;
  }

  .opt-card {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-height: 44px;
    padding: 0.7rem 0.9rem;
    border: 1px solid var(--hairline);
    border-radius: 12px;
    cursor: pointer;
  }
  .opt-card:hover {
    background: color-mix(in srgb, canvastext 5%, canvas);
  }
  .opt-card.selected {
    border-color: var(--focus-ring);
    background: color-mix(in srgb, var(--focus-ring) 10%, canvas);
  }
  /* The radio is visually hidden, so show the ring on the card when it's
     focused (keyboard nav). Always-on :focus-within, not :focus-visible. */
  .opt-card:focus-within {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .opt-card input {
    position: absolute;
    opacity: 0;
    width: 1px;
    height: 1px;
  }
  .opt-card input:focus {
    outline: none; /* ring is drawn on the card via :focus-within */
  }
  .opt-title {
    font-weight: 600;
  }
  .opt-sub {
    color: var(--text-secondary);
    font-size: 0.85rem;
  }
</style>
