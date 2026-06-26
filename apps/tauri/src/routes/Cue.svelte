<!-- Correction bubble (M3 go-live) — the suggest / accept / undo surface. A small
     Jordan-branded card in a frameless, transparent, non-focusable webview
     (label "cue"), shown + positioned top-right by Rust (lib.rs) on
     `corrections://suggested` and `corrections://applied`; this component renders
     the card and hides its own window when the content's lifetime ends.

     A fixed top-right HUD (NOT caret-anchored, NOT an inline Auto-Correct chip),
     so it works the same in every app — native and Electron alike. Nothing on
     screen changes until the user taps Shift (engine-side accept); this surface
     only shows the guess.

     Two stages, by count of ACCEPTED corrections (tunable):
       LEARNING (first N) — before → after with the changed letters soft-blue,
         "Shift to accept"; after accept, "Fixed — Esc to undo" holds for the 6s.
       FAMILIAR (≥ N) — just the corrected word + a small "Shift" hint; no undo
         cue (but Esc still reverts for the same 6s, silently — engine-side).

     Reduce Motion honoured (fade wrapped in a media query). Never red. -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import CorrectionPair from "../lib/CorrectionPair.svelte";

  // Stage trigger: corrections the user has ACCEPTED. Start at 5. Tunable.
  const FAMILIAR_THRESHOLD = 5;
  // Display lifetimes. The engine now OWNS the suggest lifecycle — it emits
  // `corrections://dismissed` on edit / type-past-cap / window-expiry (its ~4s
  // window), and the bubble hides on that. SUGGEST_MS is only a backstop in case
  // a dismiss is ever missed, so it sits a little ABOVE the engine window.
  const SUGGEST_MS = 6000;
  const ACCEPTED_LEARNING_MS = 6000;
  const ACCEPTED_FAMILIAR_MS = 1100;
  const REVERTED_MS = 1600;

  interface Suggested {
    typed: string;
    target: string;
    highlight: number[];
  }
  interface Applied {
    typed: string;
    target: string;
    undo: boolean;
  }

  type Mode = "hidden" | "suggest" | "accepted" | "reverted";
  type Stage = "learning" | "familiar";
  let mode: Mode = "hidden";
  let typed = "";
  let target = "";
  let highlight: number[] = [];
  // The stage the CURRENT card renders in — captured per-event (an accept renders
  // in the stage its suggestion was shown in, even if it tips us over N).
  let renderStage: Stage = "learning";
  // Re-trigger the fade on each event so a second correction re-animates.
  let nonce = 0;

  $: hiSet = new Set(highlight);
  $: targetChars = [...target];

  // Accepted-count persisted in this window's localStorage — the engine doesn't
  // track it; the stage is purely a display concern.
  function readAccepted(): number {
    try {
      return parseInt(localStorage.getItem("ta.correctionsAccepted") ?? "0", 10) || 0;
    } catch {
      return 0;
    }
  }
  let accepted = readAccepted();
  $: stage = (accepted >= FAMILIAR_THRESHOLD ? "familiar" : "learning") as Stage;
  function bumpAccepted() {
    accepted += 1;
    try {
      localStorage.setItem("ta.correctionsAccepted", String(accepted));
    } catch {
      /* private mode — the stage just won't graduate; harmless */
    }
  }

  const win = getCurrentWindow();
  let hideTimer: ReturnType<typeof setTimeout> | undefined;
  function scheduleHide(ms: number) {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      mode = "hidden";
      win.hide();
    }, ms);
  }

  onMount(() => {
    const offSuggest = listen<Suggested>("corrections://suggested", (e) => {
      // FIRE_TIMING t3_js — instant this webview's JS handler ran. If this lags
      // far behind t3_rust, the hidden-webview JS/timer throttle is the culprit.
      const t3js = Date.now();
      typed = e.payload.typed;
      target = e.payload.target;
      highlight = e.payload.highlight ?? [];
      renderStage = stage;
      mode = "suggest";
      nonce += 1;
      scheduleHide(SUGGEST_MS);
      // FIRE_TIMING t4_paint — after Svelte flushes the DOM and the browser
      // paints the next frame. Reported to Rust so the whole chain lands in one
      // log on one clock. Measurement only.
      tick().then(() => {
        requestAnimationFrame(() => {
          invoke("log_bubble_timing", { t3Js: t3js, t4Paint: Date.now() }).catch(
            () => {},
          );
        });
      });
    });
    const offDismiss = listen("corrections://dismissed", () => {
      // The suggestion was dropped (edited / typed on / caret moved / timed out).
      // Hide immediately so a visible bubble always means Shift will work. Only
      // affects a live suggestion — an accepted/reverted cue keeps its own timer.
      if (mode === "suggest") {
        clearTimeout(hideTimer);
        mode = "hidden";
        win.hide();
      }
    });
    const offApplied = listen<Applied>("corrections://applied", (e) => {
      typed = e.payload.typed;
      target = e.payload.target;
      nonce += 1;
      if (!e.payload.undo) {
        // Accept — render in the stage it was shown in, THEN graduate the count.
        renderStage = stage;
        bumpAccepted();
        mode = "accepted";
        scheduleHide(renderStage === "familiar" ? ACCEPTED_FAMILIAR_MS : ACCEPTED_LEARNING_MS);
      } else {
        // Esc revert (payload is target → typed). A brief acknowledgement.
        mode = "reverted";
        scheduleHide(REVERTED_MS);
      }
    });
    return () => {
      offSuggest.then((f) => f());
      offDismiss.then((f) => f());
      offApplied.then((f) => f());
      clearTimeout(hideTimer);
    };
  });
</script>

<div class="wrap">
  {#key nonce}
    <div class="bubble" role="status" aria-live="polite">
      <span class="brand" aria-hidden="true"></span>

      {#if mode === "suggest"}
        {#if renderStage === "learning"}
          <div class="body">
            <CorrectionPair {typed} {target} {highlight} />
            <div class="hint"><kbd>Shift</kbd> to accept</div>
          </div>
        {:else}
          <div class="body">
            <span class="word" aria-label={`${target}`}>
              {#each targetChars as ch, i}<span class:hl={hiSet.has(i)}>{ch}</span>{/each}
            </span>
            <div class="hint"><kbd>Shift</kbd></div>
          </div>
        {/if}
      {:else if mode === "accepted"}
        <div class="body">
          <span class="lead">Fixed</span>
          {#if renderStage === "learning"}
            <div class="hint"><kbd>Esc</kbd> to undo</div>
          {/if}
        </div>
      {:else if mode === "reverted"}
        <div class="body">
          <span class="lead">Reverted</span>
        </div>
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

  /* The Jordan-branded card: its own shape, never an inline chip. */
  .bubble {
    display: flex;
    align-items: flex-start;
    gap: 0.6rem;
    max-width: 100%;
    padding: 0.65rem 0.85rem;
    border-radius: 12px;
    background: canvas;
    border: 1px solid var(--hairline);
    box-shadow: 0 6px 20px rgb(0 0 0 / 0.18);
    color: canvastext;
    animation: cue-in 140ms ease-out;
  }
  /* Jordan accent — a small blue dot reading as "Jordan", not a status colour. */
  .brand {
    flex-shrink: 0;
    width: 8px;
    height: 8px;
    margin-top: 0.32rem;
    border-radius: 50%;
    background: var(--focus-ring);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    min-width: 0;
  }
  .lead {
    font-size: 0.95rem;
    font-weight: 600;
  }
  /* FAMILIAR: just the corrected word, changed letters soft-blue (CorrectionPair
     palette), bold so the mark never relies on colour alone. */
  .word {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.98rem;
    white-space: nowrap;
  }
  .word .hl {
    color: #7fb6ee;
    font-weight: 700;
  }
  .hint {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.8rem;
    color: var(--text-secondary);
    white-space: nowrap;
  }
  .hint kbd {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.72rem;
    line-height: 1;
    padding: 0.2rem 0.4rem;
    border-radius: 5px;
    border: 1px solid var(--hairline);
    background: color-mix(in srgb, canvastext 7%, canvas);
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
    .bubble {
      animation: none;
    }
  }
</style>
