<!-- Correction cue — the calm corner bubble for the M3 SUGGEST flow. Its own
     tiny frameless, transparent, non-focusable webview window (label "cue"),
     shown / positioned / hidden by Rust (lib.rs) on the correction events; this
     component only renders the text.

     Two states:
       suggest → OFFER: "zzx → hello · Tab to accept" (stays up while live)
       applied → ACCEPTED: "Corrected · zzx → hello" (brief flash, then Rust hides)
     Dismiss just hides the window (Rust), no render needed.

     Universal by design: a small HUD, NOT anchored to the corrected word — so it
     works in every app (browsers included), since per-word screen geometry isn't
     available off native-Cocoa text (Phase 0 finding).

     Accessibility: text affordance ("Tab to accept") so the offer never relies on
     colour alone; tokens meet WCAG AA (4.5:1); the fade-in is wrapped in a
     prefers-reduced-motion query so it simply appears when motion is reduced. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";

  interface Pair {
    typed: string;
    target: string;
  }

  type Mode = "suggest" | "applied";
  let mode: Mode = "suggest";
  let typed = "";
  let target = "";
  // Bumped per event so the fade re-triggers (Svelte keys the block on it).
  let nonce = 0;

  onMount(() => {
    const offSuggest = listen<Pair>("corrections://suggest", (e) => {
      mode = "suggest";
      typed = e.payload.typed;
      target = e.payload.target;
      nonce += 1;
    });
    const offApplied = listen<Pair>("corrections://applied", (e) => {
      mode = "applied";
      typed = e.payload.typed;
      target = e.payload.target;
      nonce += 1;
    });
    return () => {
      offSuggest.then((f) => f());
      offApplied.then((f) => f());
    };
  });
</script>

<div class="wrap">
  {#key nonce}
    <div class="cue">
      {#if mode === "applied"}
        <span class="lead">Corrected</span>
        <span class="dot" aria-hidden="true">·</span>
        <span class="pair">{typed} → {target}</span>
      {:else}
        <span class="pair">{typed} → {target}</span>
        <span class="dot" aria-hidden="true">·</span>
        <span class="accept"><kbd>Tab</kbd> to accept</span>
      {/if}
    </div>
  {/key}
</div>

<style>
  :global(body) {
    background: transparent !important;
    margin: 0;
    overflow: hidden;
  }

  .wrap {
    box-sizing: border-box;
    height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0.5rem;
  }

  .cue {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    max-width: 100%;
    padding: 0.6rem 0.95rem;
    border-radius: 12px;
    background: canvas;
    border: 1px solid var(--hairline);
    box-shadow: 0 6px 20px rgb(0 0 0 / 0.18);
    color: canvastext;
    animation: cue-in 140ms ease-out;
  }

  .lead {
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--text-secondary);
    white-space: nowrap;
  }
  .dot {
    color: var(--text-secondary);
  }
  .pair {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.95rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .accept {
    font-size: 0.8rem;
    color: var(--text-secondary);
    white-space: nowrap;
  }
  kbd {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.74rem;
    padding: 0.05rem 0.35rem;
    border: 1px solid var(--hairline);
    border-radius: 5px;
    background: color-mix(in srgb, canvastext 6%, canvas);
    color: canvastext;
  }

  @keyframes cue-in {
    from {
      opacity: 0;
      transform: translateY(-4px);
    }
    to {
      opacity: 1;
      transform: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .cue {
      animation: none;
    }
  }
</style>
