<!-- Correction cue — the brief, gentle visible signal that a correction just
     happened (M3 correction Step 1, "everything visible for now"). Its own tiny
     frameless, transparent, non-focusable webview window (label "cue"), shown +
     positioned + auto-hidden by Rust (lib.rs) on every `corrections://applied`;
     this component only renders the text.

     Universal by design: a small HUD, NOT anchored to the corrected word — so it
     works in every app (Mail, browsers, native text alike), since per-word
     screen geometry isn't available off native-Cocoa text (Phase 0 finding).

     Reduce Motion is honoured: the fade-in is wrapped in a media query, so it
     simply appears when motion is reduced. Never red, never a score — it states
     what happened, calmly. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";

  interface CorrectionApplied {
    typed: string;
    target: string;
    undo: boolean;
  }

  let typed = "";
  let target = "";
  let undo = false;
  // Bumped on each event so the fade-in animation re-triggers (Svelte keys the
  // block on it) — a second correction re-animates rather than sitting static.
  let nonce = 0;

  onMount(() => {
    const off = listen<CorrectionApplied>("corrections://applied", (e) => {
      typed = e.payload.typed;
      target = e.payload.target;
      undo = e.payload.undo;
      nonce += 1;
    });
    return () => off.then((f) => f());
  });
</script>

<div class="wrap">
  {#key nonce}
    <div class="cue" class:undo>
      <span class="lead">{undo ? "Reverted" : "Corrected"}</span>
      <span class="pair">{typed} → {target}</span>
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
    gap: 0.55rem;
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
  .pair {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.95rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
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
