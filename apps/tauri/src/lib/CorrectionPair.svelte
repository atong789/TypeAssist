<!-- One reframed Impact row's "typed → target" pair, with the corrected letter(s)
     in the TARGET word softly marked in SOFT BLUE (#7fb6ee) — never red. Shared
     by both Impact surfaces (the main-window full list and the menu-bar top-5
     glance) so the highlight logic lives in one place.

     `highlight` is the set of char indices in `target` the correction changed
     (computed engine-side in slip_class::corrected_target_indices): one letter
     for a substitution/omission, both swapped letters for a transposition, none
     for a doubling that only removed a key. The soft-blue letter is also bold so
     the mark never relies on colour perception alone; the screen-reader label
     speaks the pair plainly ("couod corrected to could"). -->
<script lang="ts">
  export let typed: string;
  export let target: string;
  export let highlight: number[] = [];

  $: hi = new Set(highlight);
  $: chars = [...target]; // char-wise, matching the engine's index basis
</script>

<span class="pair" aria-label={`${typed} corrected to ${target}`}>
  <span class="typed" aria-hidden="true">{typed}</span>
  <span class="arrow" aria-hidden="true">→</span>
  <span class="target" aria-hidden="true"
    >{#each chars as ch, i}<span class:hl={hi.has(i)}>{ch}</span>{/each}</span
  >
</span>

<style>
  .pair {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.95rem;
    white-space: nowrap;
  }
  .typed {
    color: var(--text-secondary);
  }
  .arrow {
    margin: 0 0.45rem;
    color: var(--text-secondary);
  }
  .target {
    color: canvastext;
  }
  /* The corrected letter — soft blue, bold so colour is never the only signal. */
  .hl {
    color: #7fb6ee;
    font-weight: 700;
  }
</style>
