<!-- Today — the daily mirror, and the home of the warm-up + the Corrections offer
     (Home is retired; its invite lives here now). ONE data-reflecting screen (no
     app-state machine):
       • No learned data yet → the welcome / "How I learn" state (design doc v40,
         "Today, early / no-data"): a gentle lede, the delete-key demo, and a
         "Start here" warm-up. It gives way on its own as data accrues.
       • Once there's data → the normal Today: dateline, one Jordan line, warm-up
         card, the ≥3-ready Corrections offer (or a persistent on/off read), and a
         slim Progress pointer.
     All within the 780×620 window without scrolling. Copy is in Jordan's voice;
     confirm exact strings vs design doc v40. -->
<script lang="ts">
  import { createEventDispatcher, onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { correctionsOfferDismissed, devTodayState } from "../lib/previewSettings";

  const dispatch = createEventDispatcher<{ navigate: string }>();

  // Dateline date, e.g. "Saturday, June 7" (locale-aware).
  const dateStr = new Date().toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  });

  // One warm-up, two doors: this opens the canonical round — the menu-bar
  // Practice panel — the same flow the tray "Warm-up" item opens.
  function startWarmup() {
    invoke("open_practice");
  }

  // ---- Data presence + Corrections state -----------------------------------
  // `hadAnyData` decides welcome-vs-normal: any learned correction means Jordan
  // has started learning the user's hands. `loaded` gates the body so a relaunch
  // with data never flashes the welcome before snapping to the normal screen.
  // The offer card appears once ≥3 patterns are Suggest-ready (the same
  // read_word_patterns `ready` verdict the Progress ledger uses), Corrections is
  // OFF, and the user hasn't declined — checked on mount, never mid-session.
  const READY_THRESHOLD = 3;
  interface ImpactPattern {
    ready: boolean;
  }
  interface AllowListState {
    correction_enabled: boolean;
  }
  let loaded = false;
  let hadAnyData = false;
  let readyCount = 0;
  let correctionEnabled = false;

  $: showOffer =
    !correctionEnabled && readyCount >= READY_THRESHOLD && !$correctionsOfferDismissed;

  // DEV-ONLY override (Cmd+Shift+P overlay). In a release build the guard folds
  // to false, so `forcedEmpty`/`forcedNormal` are always false and Today follows
  // real data exactly as before.
  $: forcedEmpty = import.meta.env.DEV && $devTodayState === "empty";
  $: forcedNormal = import.meta.env.DEV && $devTodayState === "normal";
  // Which screen to show: a forced state wins; otherwise wait for the store read,
  // then pick empty (no data) vs normal.
  $: showLoading = !forcedEmpty && !forcedNormal && !loaded;
  $: showEmpty = forcedEmpty || (!forcedNormal && loaded && !hadAnyData);

  function turnOnCorrections() {
    correctionEnabled = true; // optimistic; the engine echo confirms
    invoke("set_correction_enabled", { enabled: true }).catch(() => {});
  }
  function dismissOffer() {
    correctionsOfferDismissed.set(true);
  }

  onMount(() => {
    invoke<AllowListState>("read_allow_list")
      .then((al) => (correctionEnabled = !!al?.correction_enabled))
      .catch(() => {});
    invoke("request_allow_list").catch(() => {});
    invoke<ImpactPattern[]>("read_word_patterns")
      .then((p) => {
        const list = p ?? [];
        hadAnyData = list.length > 0;
        readyCount = list.filter((x) => x.ready).length;
      })
      .catch(() => {})
      .finally(() => (loaded = true));
    const off = listen<AllowListState>("corrections://state", (e) => {
      correctionEnabled = !!e.payload?.correction_enabled;
    });
    return () => off.then((f) => f());
  });
</script>

{#if showLoading}
  <!-- Loading the on-device store (near-instant); show only the title so neither
       state flashes before we know which to render. -->
  <header class="screen-header"><h1>Today</h1></header>
{:else if showEmpty}
  <!-- Empty / no-data — the welcome. Jordan is learning, not correcting, so the
       demo shows the USER fixing their own slip (the delete key), never an arrow. -->
  <header class="screen-header"><h1>Today</h1></header>
  <p class="lede">
    Welcome. You don’t type for me — you just type, anywhere on your Mac, and I learn
    your hands as you go.
  </p>

  <div class="card">
    <div class="card-label">How I learn</div>
    <div
      class="demo"
      role="img"
      aria-label="You type “teh”, fix it yourself with the delete key, and it becomes “the” — that’s how I learn your hands."
    >
      <span class="demo-slip" aria-hidden="true"
        >t<span class="slip">e</span><span class="slip">h</span></span
      >
      <span class="demo-fix" aria-hidden="true">
        <kbd class="demo-key">⌫ delete</kbd>
        <span class="demo-fix-label">you fix it</span>
      </span>
      <span class="demo-fixed" aria-hidden="true">the</span>
    </div>
    <p class="card-text" style="margin: 0;">
      You slip, you fix it, I learn how your hands type. That’s all it takes.
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
{:else}
  <!-- Normal Today — there's learned data to reflect. Wrapped in `.today-live`
       so a TIGHTER vertical rhythm (scoped below) keeps the whole page — lede,
       Warm-up, Corrections, and the Open Progress pointer — on screen without a
       scroll, WITHOUT affecting the roomier empty/welcome state above. -->
  <div class="today-live">
    <header class="screen-header">
      <h1>Today <span class="screen-context">· {dateStr}</span></h1>
    </header>

    <!-- (b) One Jordan line — encouragement + a small, honest insight. -->
    <p class="lede">
    Nice and steady. I’m getting to know the keys your hands lean on — a few small slips along
    the way, nothing that slowed you down.
  </p>

  <!-- (c) Warm-up card — the one primary daily action. Offered, never insisted on. -->
  <div class="card card-hero">
    <div class="card-label">Warm-up</div>
    <p class="card-text">
      A minute or two, weighted to the keys you’re still finding. No scores, no pressure.
    </p>
    <button class="btn-primary" on:click={startWarmup}>Start warm-up</button>
  </div>

  <!-- (d) Corrections — the full offer card once Jordan has enough to help, else a
       slim persistent on/off read. -->
  {#if showOffer}
    <div class="offer">
      <div class="offer-head">
        <i class="ti ti-bulb" aria-hidden="true"></i>
        <span class="offer-label">Corrections — Off</span>
      </div>
      <p class="offer-copy">
        I’ve learned enough to start helping. When this is on, I’ll suggest a fix right after a
        word — accept it with a tap of Shift, or press Esc to undo. Some words I’m still
        learning; those join in over time. You’re always the one typing.
      </p>
      <div class="offer-actions">
        <button class="btn-primary" on:click={turnOnCorrections}>Turn on Corrections</button>
        <button class="offer-dismiss" on:click={dismissOffer}>Not now</button>
      </div>
    </div>
  {:else}
    <div class="corr-read" class:on={correctionEnabled}>
      <span class="corr-dot" aria-hidden="true"></span>
      <span>Corrections — {correctionEnabled ? "On" : "Off"}</span>
    </div>
  {/if}

  <!-- (e) Slim Progress pointer. -->
    <button class="progress-link" on:click={() => dispatch("navigate", "progress")}>
      See how it’s shaping up → Open Progress
    </button>
  </div>
{/if}

<style>
  .lede {
    margin: 0 0 1rem;
    max-width: 40rem;
    font-size: 1.12rem;
    line-height: 1.6;
    color: canvastext;
  }

  /* ---- live Today: tightened vertical rhythm so the whole page (incl. the
     Open Progress pointer) fits the fixed window with no scroll. SCOPED to
     `.today-live` so the empty/welcome state keeps its roomier spacing. The
     copy is untouched — only gaps and card padding shrink. */
  .today-live {
    display: flex;
    flex-direction: column;
  }
  .today-live .lede {
    margin-bottom: 0.55rem;
    font-size: 1.05rem;
    line-height: 1.5;
  }
  .today-live .card,
  .today-live .offer {
    margin: 0.25rem 0 0.6rem;
    padding: 0.8rem 1.05rem;
  }
  .today-live .card-label,
  .today-live .offer-head {
    margin-bottom: 0.3rem;
  }
  .today-live .card-text,
  .today-live .offer-copy {
    margin-bottom: 0.7rem;
    line-height: 1.5;
  }
  .today-live .corr-read {
    margin: 0.25rem 0 0.6rem;
  }
  .today-live .progress-link {
    margin-top: 0.1rem;
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
  /* Hero card — accent-tinted, for the more inviting nudge. */
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

  /* ---- "how I learn": [teh, marked]  [⌫ you fix it]  the ---- */
  .demo {
    display: flex;
    align-items: center;
    gap: 1.1rem;
    /* Generous TOP room so the amber dots above the slipped letters clear the
       "How I learn" label and never collide with it. */
    margin: 1.8rem 0 1rem;
    font-size: 1.55rem;
    line-height: 1.3;
  }
  /* The calm correction — normal text, no green/reward colour. */
  .demo-fixed {
    color: canvastext;
  }
  /* The middle step — the user fixing their own slip: a Mac delete keycap with a
     small muted "you fix it" label beneath. Quiet by design; this is the user
     acting, not Jordan correcting. */
  .demo-fix {
    display: inline-flex;
    flex-direction: column;
    align-items: center;
    gap: 0.3rem;
  }
  .demo-key {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.85rem;
    line-height: 1;
    padding: 0.35rem 0.55rem;
    border-radius: 7px;
    border: 1px solid var(--hairline);
    background: color-mix(in srgb, canvastext 6%, canvas);
    color: var(--text-secondary);
    white-space: nowrap;
  }
  .demo-fix-label {
    font-size: 0.7rem;
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

  /* ---- Corrections offer card (Jordan's invite, once there's enough) ---- */
  .offer {
    max-width: 40rem;
    margin: 0.4rem 0 1rem;
    padding: 1.1rem 1.25rem;
    border-radius: 12px;
    background: color-mix(in srgb, var(--focus-ring) 10%, canvas);
    border: 1px solid color-mix(in srgb, var(--focus-ring) 28%, canvas);
  }
  .offer-head {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 0.45rem;
  }
  .offer-head .ti {
    font-size: 1.1rem;
    color: var(--focus-ring);
    flex-shrink: 0;
  }
  .offer-label {
    font-size: 0.82rem;
    font-weight: 600;
    color: var(--text-secondary);
  }
  .offer-copy {
    margin: 0 0 0.9rem;
    font-size: 0.95rem;
    line-height: 1.55;
    color: canvastext;
  }
  .offer-actions {
    display: flex;
    align-items: center;
    gap: 1rem;
  }
  .offer-dismiss {
    padding: 0.4rem 0.2rem;
    font: inherit;
    font-size: 0.9rem;
    color: var(--text-secondary);
    background: transparent;
    border: none;
    cursor: pointer;
  }
  .offer-dismiss:hover {
    color: canvastext;
    text-decoration: underline;
  }

  /* ---- persistent on/off read (when the full offer card isn't shown) ---- */
  .corr-read {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    margin: 0.4rem 0 1rem;
    padding: 0.45rem 0.75rem;
    border: 1px solid var(--hairline);
    border-radius: 9px;
    font-size: 0.9rem;
    color: var(--text-secondary);
    background: color-mix(in srgb, canvastext 3%, canvas);
  }
  .corr-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
    border: 1.5px solid color-mix(in srgb, canvastext 35%, canvas);
    background: transparent;
  }
  .corr-read.on .corr-dot {
    background: var(--focus-ring);
    border-color: var(--focus-ring);
  }

  /* ---- buttons ---- */
  .btn-primary {
    font: inherit;
    font-size: 0.95rem;
    min-height: 38px;
    padding: 0.5rem 1.1rem;
    border-radius: 9px;
    cursor: pointer;
    border: none;
    background: var(--focus-ring);
    color: #fff;
  }
  .btn-primary:hover {
    background: color-mix(in srgb, var(--focus-ring) 88%, black);
  }
  .btn-primary:focus,
  .offer-dismiss:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
    border-radius: 5px;
  }

  /* ---- "shaping up → Open Progress" pointer ---- */
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
