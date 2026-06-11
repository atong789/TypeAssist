<!-- TEMPORARY dev-only preview switcher. Flips the SHARED app state (Day one /
     Building back / Fluent) by hand so every state-aware screen (Home, Today, …)
     can be reviewed in each state from wherever you are. Lives in the shell
     (rendered once in App.svelte), not in any one screen. Remove this component
     and its mount when automatic state detection from engine data is wired. -->
<script lang="ts">
  import { appState, APP_STATES, onboarded } from "./previewSettings";
</script>

<div class="preview" role="group" aria-label="Preview app state (developer)">
  <span class="preview-label">Preview</span>
  {#each APP_STATES as s}
    <button
      class="preview-btn"
      class:on={$appState === s.value}
      aria-pressed={$appState === s.value}
      on:click={() => appState.set(s.value)}>{s.label}</button
    >
  {/each}
  <span class="preview-sep" aria-hidden="true"></span>
  <button class="preview-btn" on:click={() => onboarded.set(false)}>Replay onboarding</button>
</div>

<style>
  .preview {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    margin: 0 0 1.25rem;
    padding: 0.3rem 0.4rem;
    border: 1px dashed var(--hairline);
    border-radius: 9px;
    width: fit-content;
  }
  .preview-label {
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-secondary);
    padding: 0 0.4rem;
  }
  .preview-btn {
    font: inherit;
    font-size: 0.8rem;
    padding: 0.3rem 0.6rem;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
  }
  .preview-btn:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  .preview-btn.on {
    background: color-mix(in srgb, var(--focus-ring) 16%, canvas);
    color: canvastext;
    font-weight: 600;
  }
  .preview-btn:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .preview-sep {
    width: 1px;
    align-self: stretch;
    margin: 0.1rem 0.3rem;
    background: var(--hairline);
  }
</style>
