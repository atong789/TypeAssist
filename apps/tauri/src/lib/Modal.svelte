<!-- Reusable modal dialog: a real, accessible modal. Moves focus into the dialog
     on open (to the [data-autofocus] element — the safe default), traps Tab
     inside it, Escape cancels, and focus returns to the trigger on close. While
     open it sets the shared `modalOpen` flag so App.svelte's global focus trap
     stands down (the modal owns Tab). Content + buttons are slotted in. -->
<script lang="ts">
  import { onMount, onDestroy, tick, createEventDispatcher } from "svelte";
  import { fade } from "svelte/transition";
  import { modalOpen } from "./modal";

  export let titleId: string;
  // Opt-in: play a brief, quiet fade-out when the dialog is dismissed (matches the
  // correction cue's gentle feel). Off by default so other dialogs close instantly.
  // Honoured-by-construction for Reduce Motion (duration → 0).
  export let fadeOut = false;
  const reduceMotion =
    typeof window !== "undefined" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  $: outMs = fadeOut && !reduceMotion ? 200 : 0;
  const dispatch = createEventDispatcher<{ cancel: void }>();

  let dialogEl: HTMLElement;
  let prevFocus: HTMLElement | null = null;

  function tabbables(): HTMLElement[] {
    if (!dialogEl) return [];
    const sel =
      'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';
    return Array.from(dialogEl.querySelectorAll<HTMLElement>(sel)).filter(
      (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length),
    );
  }

  function onKeydown(event: KeyboardEvent) {
    if (!dialogEl) return;
    if (event.key === "Escape") {
      event.preventDefault();
      dispatch("cancel");
      return;
    }
    if (event.key !== "Tab") return;
    const t = tabbables();
    if (t.length === 0) return;
    const first = t[0];
    const last = t[t.length - 1];
    const active = document.activeElement as HTMLElement | null;
    // Pull focus back if it ever escapes the dialog, then wrap at the edges.
    if (active && !dialogEl.contains(active)) {
      event.preventDefault();
      first.focus();
      return;
    }
    if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    } else if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    }
  }

  onMount(() => {
    prevFocus = document.activeElement as HTMLElement | null;
    modalOpen.set(true);
    tick().then(() => {
      const def = dialogEl?.querySelector<HTMLElement>("[data-autofocus]") ?? tabbables()[0];
      def?.focus();
    });
  });
  onDestroy(() => {
    modalOpen.set(false);
    prevFocus?.focus?.();
  });
</script>

<svelte:window on:keydown={onKeydown} />

<div class="overlay" out:fade={{ duration: outMs }}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby={titleId}
    bind:this={dialogEl}
  >
    <slot />
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1.5rem;
    background: color-mix(in srgb, #000 48%, transparent);
  }
  .dialog {
    width: 100%;
    max-width: 26rem;
    padding: 1.4rem 1.5rem 1.2rem;
    border-radius: 14px;
    background: canvas;
    border: 1px solid var(--hairline);
    box-shadow: 0 14px 44px color-mix(in srgb, #000 38%, transparent);
    color: canvastext;
  }
</style>
