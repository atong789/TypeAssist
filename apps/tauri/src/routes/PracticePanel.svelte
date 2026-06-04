<!-- Practice Mode — the menu-bar "breathing exercise for the affected hand".
     Lives in its OWN small webview window (label "practice"), shown from the
     tray. A calm, four-phase loop:

       ready → typing → continue → (loop) → snapshot

     Design rules (brief + CLAUDE.md):
     - NO real-time feedback while typing: the caret advances and the *target*
       character renders; a wrong key is silently smoothed (the Warm-up model),
       never shown red/amber. No live score, %, timer, or WPM — ever.
     - The user controls the pace: "Continue?" after each sentence.
     - Sentences are real, lowercase, weighted to the user's weak keys
       (sentenceBank). Capture is the OS-wide tap, so typing here is observed by
       the engine with no extra wiring; the snapshot's "observations added" is
       the motor map's total_observations delta across the round.
     - All copy lives in practiceCopy.ts; sentence content in sentences.txt. -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { LogicalSize } from "@tauri-apps/api/dpi";
  import { pickSentence, weakKeysIn, type WeakKey } from "../lib/sentenceBank";
  import {
    loadSpellingPref,
    resolveVariant,
    localizeSentence,
    type SpellingVariant,
  } from "../lib/locale";
  import { copy } from "../lib/practiceCopy";

  type Phase = "ready" | "typing" | "continue" | "snapshot";
  type Segment =
    | { type: "word"; chars: { ch: string; idx: number }[] }
    | { type: "space"; idx: number };

  // The subset of engine://motor-stability we use. weakest = [(char, slip)],
  // weakest first; total_observations is the lifetime motor-map count we diff.
  interface StabilityReport {
    total_observations: number;
    weakest: WeakKey[];
  }

  // engine://practice-trend — per-key slip-rate series from weekly snapshots.
  interface TrendPoint {
    date: string;
    slip_rate: number;
  }
  interface KeyTrend {
    key: string;
    points: TrendPoint[];
  }

  let phase: Phase = "ready";

  // Curriculum + capture signal, refreshed from the engine.
  let weakest: WeakKey[] = [];
  let latestTotalObs: number | null = null;
  let startObs: number | null = null; // total_observations when this round began

  // A round shows BLOCK_SIZE sentences joined into ONE flowing passage (closer
  // to a paragraph than flashcards) — typed straight through, with `Continue?`
  // only at the end. Three is a real-but-not-overcommitting block for someone
  // meaningfully into recovery. Tunable.
  const BLOCK_SIZE = 3;
  let sentencesInBlock = 0; // sentences composing the current passage (for the count)

  // Per-round accumulators (reflective, never scored).
  let sentencesDone = 0;
  const practiced = new Set<string>(); // weak keys the round actually leaned into
  const seen = new Set<string>(); // avoid immediate repeats within a round

  // Current sentence. `typed` holds the user's ACTUAL keystrokes in order, so a
  // slip can be shown honestly in place (see below) — not smoothed away. The
  // caret rides the end of what's been typed.
  let sentence = "";
  let chars: string[] = [];
  let segments: Segment[] = [];
  let typed: string[] = [];
  $: caretPos = typed.length;

  // Per-position display for the typing surface, as a REACTIVE derived array so
  // Svelte re-renders the cells whenever `typed` (or the sentence) changes — a
  // function call like `cellClass(idx)` would hide the `typed` dependency from
  // Svelte's reactivity and never update (the bug that made slips invisible).
  // A slip shows the WRONG key the user actually hit, marked amber + wavy
  // underline + dot (CSS) — visible without colour perception, never red. The
  // caret advances past a slip (it never sticks); backspace removes it.
  type Cell = { ch: string; cls: "done-ch" | "todo-ch" | "slip" };
  $: cells = chars.map((ch, i): Cell => {
    if (i >= typed.length) return { ch, cls: "todo-ch" };
    return { ch: typed[i], cls: typed[i] === ch ? "done-ch" : "slip" };
  });

  // Panel sizing. macOS does NOT expose its Accessibility → Text Size setting to
  // WKWebView content (no CSS/JS hook), so we can't auto-detect it. What we can
  // do: grow the window to fit the content's natural height, so the panel never
  // clips and DOES grow if the text is enlarged by any means (webview zoom, or a
  // future in-app text-size card selector). Width stays fixed; height clamps
  // between the base and a cap so the calm layout holds at normal sizes.
  const PANEL_W = 360;
  const PANEL_BASE_H = 480;
  const PANEL_MAX_H = 760;
  let contentEl: HTMLElement;

  function fitWindowToContent() {
    if (!contentEl) return;
    const desired = Math.min(
      PANEL_MAX_H,
      Math.max(PANEL_BASE_H, Math.ceil(contentEl.scrollHeight) + 48),
    );
    getCurrentWindow()
      .setSize(new LogicalSize(PANEL_W, desired))
      .catch(() => {});
  }

  // Focus anchors (each phase lands the ring on a sensible target).
  let rootEl: HTMLElement;
  let beginEl: HTMLButtonElement;
  let surfaceEl: HTMLDivElement;
  let contAgainEl: HTMLButtonElement;
  let doneEl: HTMLButtonElement;

  // Observations folded in during this round. May lag slightly — some verdicts
  // resolve on idle thresholds — so this is honest-but-conservative, never
  // inflated. Null until we have both endpoints.
  $: observationsAdded =
    latestTotalObs !== null && startObs !== null
      ? Math.max(0, latestTotalObs - startObs)
      : null;

  $: practicedKeys = [...practiced];
  $: readySub = weakest.length === 0 ? copy.coldStart : copy.ready.sub;

  // Trend (Phase 3). Only keys with ≥2 weekly points earn a trend line; with
  // fewer, the snapshot stays "what happened" only — never an invented number.
  let trend: KeyTrend[] = [];
  $: trendKeys = trend.filter((k) => k.points.length >= 2);

  // A key's trend is shown as a neutral sparkline — the line speaks for itself
  // (Facts, not commentary: no invented verdict words like "holding steady").
  // Mirror, not scoreboard: a single neutral tone, never red. The
  // ≥2-weekly-points gate above is unchanged, so the trend still appears once
  // there's enough history — only the wording was removed, not the trend.
  const SPARK_W = 110;
  const SPARK_H = 24;
  function sparkPoints(points: TrendPoint[]): string {
    const ys = points.map((p) => p.slip_rate);
    const min = Math.min(...ys);
    const max = Math.max(...ys);
    const span = max - min || 1;
    const n = points.length;
    return points
      .map((p, i) => {
        const x = (i / (n - 1)) * SPARK_W;
        const y = max === min ? SPARK_H / 2 : SPARK_H - ((p.slip_rate - min) / span) * SPARK_H;
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(" ");
  }
  function sparkEnd(points: TrendPoint[]): { x: number; y: number } {
    const parts = sparkPoints(points).split(" ");
    const [x, y] = parts[parts.length - 1].split(",");
    return { x: parseFloat(x), y: parseFloat(y) };
  }

  function computeSegments(cs: string[]): Segment[] {
    const out: Segment[] = [];
    let i = 0;
    while (i < cs.length) {
      if (cs[i] === " ") {
        out.push({ type: "space", idx: i });
        i += 1;
      } else {
        const ws: { ch: string; idx: number }[] = [];
        while (i < cs.length && cs[i] !== " ") {
          ws.push({ ch: cs[i], idx: i });
          i += 1;
        }
        out.push({ type: "word", chars: ws });
      }
    }
    return out;
  }

  function requestStability() {
    invoke("request_motor_stability").catch(() => {});
  }

  // Open / re-open always returns to the intention moment ("Ready?").
  // Spelling variant for the word bank, resolved from the user's Settings
  // override (or the macOS locale). Re-read on each open so a change in Settings
  // takes effect the next time Practice is opened.
  let spellingVariant: SpellingVariant = resolveVariant(loadSpellingPref());

  function resetSession() {
    phase = "ready";
    sentencesDone = 0;
    practiced.clear();
    seen.clear();
    startObs = null;
    sentence = "";
    chars = [];
    segments = [];
    typed = [];
    sentencesInBlock = 0;
    trend = [];
    spellingVariant = resolveVariant(loadSpellingPref());
    requestStability();
  }

  // Compose one passage from BLOCK_SIZE sentences, joined by spaces, and type
  // through it as a single flowing exercise (not three separate cards).
  function loadBlock() {
    const parts: string[] = [];
    for (let i = 0; i < BLOCK_SIZE; i++) {
      const s = pickSentence(weakest, seen);
      seen.add(s);
      parts.push(s);
    }
    sentencesInBlock = parts.length;
    // Localize the passage to the resolved spelling variant (e.g. grey→gray on
    // a US Mac) before it's rendered/typed.
    sentence = localizeSentence(parts.join(" "), spellingVariant);
    chars = [...sentence];
    segments = computeSegments(chars);
    typed = [];
  }

  function begin() {
    startObs = latestTotalObs; // baseline; delta is computed at the snapshot
    loadBlock();
    phase = "typing";
  }

  // The passage is finished when every position has been typed. Then pause at
  // `Continue?` — no mid-block interruptions.
  function completeBlock() {
    for (const k of weakKeysIn(sentence, weakest)) practiced.add(k);
    sentencesDone += sentencesInBlock;
    phase = "continue";
    requestStability();
  }

  function continueAnother() {
    loadBlock();
    phase = "typing";
  }

  function finish() {
    trend = [];
    phase = "snapshot";
    requestStability();
    // Reconstruct the trend for the keys this round leaned into (async reply
    // on engine://practice-trend). Skip when there were no weak keys to chart.
    if (practicedKeys.length > 0) {
      invoke("request_practice_trend", { keys: practicedKeys }).catch(() => {});
    }
  }

  function done() {
    // Surfaces a permission/IPC failure instead of failing silently (the close
    // button looked dead when `core:window:allow-hide` was missing).
    getCurrentWindow()
      .hide()
      .catch((e) => console.error("practice panel hide failed:", e));
  }

  // Typing surface: record the actual key and show slips live (no smoothing).
  function onSurfaceKeydown(event: KeyboardEvent) {
    if (event.key === "Tab") return; // navigation; handled by the focus trap
    if (event.metaKey || event.ctrlKey || event.altKey) return;

    if (event.key === "Backspace") {
      // Universal backspace: remove the last keystroke (slip or not) to redo.
      event.preventDefault();
      if (typed.length > 0) typed = typed.slice(0, -1);
      return;
    }
    // Any single printable key (incl. space) is recorded as-typed and the caret
    // advances one position. A wrong key shows immediately as a slip — it never
    // blocks the caret. No score, %, timer, or WPM is shown live.
    if (event.key.length === 1) {
      event.preventDefault();
      if (typed.length < chars.length) {
        typed = [...typed, event.key];
        if (typed.length === chars.length) completeBlock();
      }
    }
  }

  // Minimal focus trap — this is a separate window, so App.svelte's trap does
  // not cover it. Tab past the last control → first; Shift+Tab past first →
  // last. Without it, Tab escapes to the host window (a ringless, non-DOM spot)
  // — and here that also blurs the panel, hiding it.
  function tabbables(): HTMLElement[] {
    if (!rootEl) return [];
    const sel = 'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])';
    return Array.from(rootEl.querySelectorAll<HTMLElement>(sel)).filter(
      (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length),
    );
  }
  function onRootKeydown(event: KeyboardEvent) {
    // Esc closes the panel from anywhere — an explicit way out, in addition to
    // hide-on-blur and the visible close button.
    if (event.key === "Escape") {
      event.preventDefault();
      done();
      return;
    }
    if (event.key !== "Tab" || !rootEl) return;
    const t = tabbables();
    if (t.length === 0) return;
    const first = t[0];
    const last = t[t.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (!active) return;
    if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    } else if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    }
  }

  // Land the ring on a sensible target every phase change (and on mount).
  $: phase, scheduleFocus();
  async function scheduleFocus() {
    await tick();
    if (phase === "ready") beginEl?.focus();
    else if (phase === "typing") surfaceEl?.focus();
    else if (phase === "continue") contAgainEl?.focus();
    else if (phase === "snapshot") doneEl?.focus();
  }

  onMount(() => {
    const offStability = listen<StabilityReport>("engine://motor-stability", (e) => {
      weakest = e.payload.weakest ?? [];
      if (typeof e.payload.total_observations === "number") {
        latestTotalObs = e.payload.total_observations;
      }
    });
    const offOpen = listen("practice://open", () => resetSession());
    const offTrend = listen<{ keys: KeyTrend[] }>("engine://practice-trend", (e) => {
      trend = e.payload.keys ?? [];
    });
    requestStability();

    // Grow/shrink the panel window to fit content (incl. enlarged text). The
    // observer fires on phase changes and on any text-size change.
    let ro: ResizeObserver | undefined;
    if (contentEl && "ResizeObserver" in window) {
      ro = new ResizeObserver(() => fitWindowToContent());
      ro.observe(contentEl);
    }
    fitWindowToContent();

    return () => {
      offStability.then((off) => off());
      offOpen.then((off) => off());
      offTrend.then((off) => off());
      ro?.disconnect();
    };
  });
</script>

<!-- Focus trap lives on the window (like App.svelte) so the root div needs no
     interactive role; it only acts on Tab at the edges. -->
<svelte:window on:keydown={onRootKeydown} />

<div class="panel" bind:this={rootEl}>
  <button class="close" aria-label="Close practice" on:click={done}>
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round">
      <line x1="6" y1="6" x2="18" y2="18" /><line x1="18" y1="6" x2="6" y2="18" />
    </svg>
  </button>
  <div class="content" bind:this={contentEl}>
  {#if phase === "ready"}
    <div class="stage ready">
      <div class="breath" aria-hidden="true"><span class="ring"></span></div>
      <h1 class="title">{copy.ready.title}</h1>
      <p class="sub">{readySub}</p>
      <button class="primary" bind:this={beginEl} aria-label={copy.ready.beginLabel} on:click={begin}>
        begin
      </button>
      <p class="hint">{copy.ready.hint}</p>
    </div>
  {:else if phase === "typing"}
    <div class="stage typing">
      <div
        class="surface"
        tabindex="0"
        role="textbox"
        aria-multiline="false"
        aria-label={copy.typing.surfaceLabel}
        bind:this={surfaceEl}
        on:keydown={onSurfaceKeydown}
      >{#each segments as seg}{#if seg.type === "word"}<span class="word">{#each seg.chars as c}{#if c.idx === caretPos}<span class="caret" aria-hidden="true"></span>{/if}<span class={cells[c.idx].cls}>{cells[c.idx].ch}</span>{/each}</span>{:else}{#if seg.idx === caretPos}<span class="caret" aria-hidden="true"></span>{/if}{#if cells[seg.idx].cls === "slip"}<span class="slip">{cells[seg.idx].ch}</span>{:else}{' '}{/if}{/if}{/each}</div>
      <p class="hint">{copy.typing.reassurance}</p>
    </div>
  {:else if phase === "continue"}
    <div class="stage continue">
      <div class="breath" aria-hidden="true"><span class="ring"></span></div>
      <h1 class="title">{copy.cont.title}</h1>
      <div class="choices">
        <button class="primary" bind:this={contAgainEl} aria-label={copy.cont.againLabel} on:click={continueAnother}>
          {copy.cont.again}
        </button>
        <button class="secondary" aria-label={copy.cont.finishLabel} on:click={finish}>
          {copy.cont.finish}
        </button>
      </div>
      <p class="hint">{copy.cont.hint}</p>
    </div>
  {:else}
    <div class="stage snapshot">
      <h1 class="title">{copy.snapshot.title}</h1>
      <div class="readout">
        <p class="keys">{copy.snapshot.keysLine(practicedKeys)}</p>
        {#if copy.snapshot.observationsLine(observationsAdded)}
          <p class="meta">{copy.snapshot.observationsLine(observationsAdded)}</p>
        {/if}
        {#if copy.snapshot.sentencesLine(sentencesDone)}
          <p class="meta quiet">{copy.snapshot.sentencesLine(sentencesDone)}</p>
        {/if}
      </div>
      {#if trendKeys.length > 0}
        <div class="trend">
          <p class="trend-head">{copy.snapshot.trendHeader}</p>
          <ul class="trend-list">
            {#each trendKeys as kt}
              {@const pts = sparkPoints(kt.points)}
              {@const e = sparkEnd(kt.points)}
              <li class="trend-row">
                <span class="trend-key">{kt.key}</span>
                <span class="trend-spark">
                  <svg
                    viewBox="0 0 {SPARK_W} {SPARK_H}"
                    width={SPARK_W}
                    height={SPARK_H}
                    role="img"
                    aria-label={`${kt.key}: slip-rate trend over ${kt.points.length} weeks`}
                  >
                    <polyline points={pts} fill="none" stroke="var(--trend-line)" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
                    <circle cx={e.x} cy={e.y} r="2.5" fill="var(--trend-line)" />
                  </svg>
                </span>
              </li>
            {/each}
          </ul>
        </div>
      {/if}
      <div class="choices">
        <button class="primary" bind:this={doneEl} aria-label={copy.snapshot.doneLabel} on:click={done}>
          {copy.snapshot.done}
        </button>
      </div>
    </div>
  {/if}
  </div>
</div>

<style>
  /* The practice window is transparent + frameless: paint our own rounded card
     and let the corners outside it be see-through. app.css sets a solid body
     background for the main window; override it to transparent HERE (this
     component only ever mounts in the practice window). */
  :global(body) {
    background: transparent !important;
  }

  .panel {
    position: relative;
    box-sizing: border-box;
    min-height: 100vh;
    padding: 1.5rem;
    border-radius: 16px;
    background: canvas;
    border: 1px solid var(--hairline);
    color: canvastext;
    overflow: hidden;
    /* Centre the natural-height content; the window itself is resized to fit
       the content (see fitWindowToContent), so larger text grows the panel. */
    display: flex;
    flex-direction: column;
    justify-content: center;
    /* Neutral trend-line color — mirror, not scoreboard; never red. */
    --trend-line: color-mix(in srgb, canvastext 45%, canvas);
  }

  /* Natural-height wrapper the ResizeObserver measures — must NOT be stretched
     to the viewport, or it couldn't report content growth. */
  .content {
    width: 100%;
  }

  /* Explicit way out, on every phase — alongside Esc and hide-on-blur. */
  .close {
    position: absolute;
    top: 0.6rem;
    right: 0.6rem;
    width: 32px;
    height: 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: none;
    border-radius: 8px;
    background: transparent;
    color: var(--text-secondary);
    cursor: pointer;
  }
  .close:hover {
    background: color-mix(in srgb, canvastext 8%, canvas);
    color: canvastext;
  }
  .close:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }

  .stage {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    gap: 1rem;
  }

  .title {
    margin: 0;
    font-size: 1.5rem;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .sub {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.95rem;
    max-width: 22rem;
  }
  .hint {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.85rem;
  }

  /* ---- breathing motion — the visual metaphor. Calm, slow, ~5s cycle.
     Reduce Motion: hold a still ring (no pulse) — never animate. ---- */
  .breath {
    width: 84px;
    height: 84px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .ring {
    width: 56px;
    height: 56px;
    border-radius: 50%;
    border: 2px solid color-mix(in srgb, var(--focus-ring) 60%, canvas);
    background: color-mix(in srgb, var(--focus-ring) 12%, canvas);
    animation: breathe 5s ease-in-out infinite;
  }
  @keyframes breathe {
    0%,
    100% {
      transform: scale(0.72);
      opacity: 0.55;
    }
    50% {
      transform: scale(1);
      opacity: 1;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .ring {
      animation: none;
      transform: scale(0.9);
      opacity: 0.9;
    }
  }

  /* ---- buttons — large hit areas, visible ring, no slider anywhere ---- */
  .primary,
  .secondary {
    min-height: 44px;
    padding: 0.6rem 1.6rem;
    font: inherit;
    font-weight: 600;
    border-radius: 12px;
    cursor: pointer;
    color: inherit;
  }
  .primary {
    border: 1px solid color-mix(in srgb, var(--focus-ring) 55%, canvas);
    background: color-mix(in srgb, var(--focus-ring) 14%, canvas);
  }
  .secondary {
    border: 1px solid var(--hairline);
    background: transparent;
  }
  .primary:hover {
    background: color-mix(in srgb, var(--focus-ring) 22%, canvas);
  }
  .secondary:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  .primary:focus,
  .secondary:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .choices {
    display: flex;
    gap: 0.75rem;
  }

  /* ---- typing surface — no feedback: target chars only, calm caret ---- */
  .surface {
    width: 100%;
    padding: 1.1rem 1.25rem;
    border: 2px solid var(--hairline);
    border-radius: 14px;
    font-size: 1.3rem;
    line-height: 1.7;
    cursor: text;
    outline: none;
    /* let words wrap only at spaces */
    white-space: pre-wrap;
  }
  .surface:focus {
    border-color: var(--focus-ring);
  }
  .word {
    white-space: nowrap;
  }
  .done-ch {
    color: canvastext;
  }
  .todo-ch {
    color: color-mix(in srgb, canvastext 55%, canvas);
  }
  /* A slip is shown live but gently: amber + wavy underline + a dot marker
     above the character. Triple-coded so it reads without colour perception
     (the underline and dot each carry it alone). Never red, never a score. */
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
  .caret {
    display: inline-block;
    width: 2px;
    height: 1.15em;
    background: var(--focus-ring);
    vertical-align: text-bottom;
  }

  /* ---- snapshot ---- */
  .readout {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .keys {
    margin: 0;
    font-size: 1.05rem;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .meta {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.95rem;
  }
  .meta.quiet {
    font-size: 0.85rem;
  }

  /* ---- trend (snapshot) — capability framing, never deficit ---- */
  .trend {
    width: 100%;
    max-width: 18rem;
  }
  .trend-head {
    margin: 0 0 0.4rem;
    color: var(--text-secondary);
    font-size: 0.85rem;
  }
  .trend-list {
    margin: 0;
    padding: 0;
    list-style: none;
    text-align: left;
  }
  .trend-row {
    display: grid;
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: 1rem;
    padding: 0.4rem 0;
    border-bottom: 1px solid var(--hairline);
  }
  .trend-key {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-weight: 600;
  }
  .trend-spark {
    justify-self: end;
    display: flex;
    align-items: center;
    color: var(--text-secondary);
  }
</style>
