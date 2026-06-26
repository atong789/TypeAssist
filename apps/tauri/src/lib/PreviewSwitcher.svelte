<!-- DEV-ONLY state-preview overlay (same spirit as the Cmd+Shift+D debug panel):
     hidden until summoned by its shortcut, excluded from release builds entirely
     (mounted via a guarded dynamic import in App.svelte). It forces Today's
     state and replays onboarding WITHOUT touching real data, so a Mac with lots
     of history can still review the empty/welcome screen and onboarding. -->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import {
    onboarded,
    devTodayState,
    devProgressState,
    type DevTodayState,
    type DevProgressState,
  } from "./previewSettings";

  // Force-show the correction bubble with dummy content — isolates the
  // window/UI path from the engine's fire decision.
  function showBubble() {
    invoke("dev_show_bubble").catch(() => {});
  }

  const TODAY: { value: DevTodayState; label: string }[] = [
    { value: "auto", label: "Auto (real data)" },
    { value: "empty", label: "Empty / welcome" },
    { value: "normal", label: "Normal / live" },
  ];

  const PROGRESS: { value: DevProgressState; label: string }[] = [
    { value: "auto", label: "Auto (real data)" },
    { value: "empty", label: "No data (day one)" },
  ];
</script>

<div class="dev-preview" role="group" aria-label="Developer state preview">
  <span class="dev-title">Dev · state preview</span>

  <div class="dev-block">
    <span class="dev-cap">Today</span>
    <div class="dev-btns">
      {#each TODAY as t}
        <button
          class="dev-btn"
          class:on={$devTodayState === t.value}
          aria-pressed={$devTodayState === t.value}
          on:click={() => devTodayState.set(t.value)}>{t.label}</button
        >
      {/each}
    </div>
  </div>

  <div class="dev-block">
    <span class="dev-cap">Progress</span>
    <div class="dev-btns">
      {#each PROGRESS as p}
        <button
          class="dev-btn"
          class:on={$devProgressState === p.value}
          aria-pressed={$devProgressState === p.value}
          on:click={() => devProgressState.set(p.value)}>{p.label}</button
        >
      {/each}
    </div>
  </div>

  <div class="dev-block">
    <span class="dev-cap">Bubble</span>
    <div class="dev-btns">
      <button class="dev-btn" on:click={showBubble}>Show bubble (dummy)</button>
    </div>
  </div>

  <button class="dev-btn dev-wide" on:click={() => onboarded.set(false)}>
    Replay onboarding
  </button>
</div>

<style>
  /* Floating, unmistakably a dev tool — dashed accent border, pinned top-centre,
     above everything. Never shipped (the whole component is dropped from release
     builds), so it doesn't need to obey the app's a11y/layout rules. */
  .dev-preview {
    position: fixed;
    top: 8px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 9999;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.6rem 0.75rem;
    border: 1px dashed var(--focus-ring);
    border-radius: 10px;
    background: color-mix(in srgb, canvas 92%, var(--focus-ring));
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.18);
  }
  .dev-title {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-secondary);
  }
  .dev-block {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .dev-cap {
    font-size: 0.78rem;
    font-weight: 600;
    color: var(--text-secondary);
  }
  .dev-btns {
    display: flex;
    gap: 0.3rem;
  }
  .dev-btn {
    font: inherit;
    font-size: 0.78rem;
    padding: 0.3rem 0.6rem;
    border: 1px solid var(--hairline);
    border-radius: 6px;
    background: canvas;
    color: var(--text-secondary);
    cursor: pointer;
  }
  .dev-btn:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  .dev-btn.on {
    background: color-mix(in srgb, var(--focus-ring) 18%, canvas);
    color: canvastext;
    font-weight: 600;
    border-color: color-mix(in srgb, var(--focus-ring) 40%, canvas);
  }
  .dev-wide {
    width: 100%;
  }
  .dev-btn:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
</style>
