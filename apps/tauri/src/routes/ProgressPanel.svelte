<!-- Progress dashboard — the menu-bar window that lets the user read their own
     typing data without Console.app. Lives in its OWN small webview window
     (label "progress"), shown from the tray, anchored under the menu-bar icon,
     hidden on click-away (see lib.rs `on_window_event`). Two tabs:

       Statistics — how I type   (built in step 4, on verified engine numbers)
       Impact     — what the system has learned and is ready to fix

     Navigation is one-hand, no chords (brief): switch by click, or `1`/`2`, or
     `←`/`→`. Never a modifier combo. Scroll = wheel/trackpad or `↑`/`↓`/space.
     Reduce Motion is honoured by having NO tab-switch animation — the highlight
     just jumps. The panel is a fixed height; the Impact ledger scrolls
     internally so the window never grows down toward the Dock.

     Design principles (do not violate): mirror not coach, report observed
     (keystrokes) never assumed (fingers), facts not commentary, mirror not
     scoreboard (neutral single colour, never red), observational + warm voice. -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  type Tab = "statistics" | "impact";

  // One learned correction, as the engine sees it. `obs` is the DECAYED weight
  // (matches the kill-switch's view); `ready` means obs >= the 12-observation
  // threshold. coord/precis tag arrives in step 3 — absent for now.
  interface ImpactPattern {
    typed: string;
    target: string;
    obs: number;
    ready: boolean;
    // "coord" | "precis", derived engine-side from the pair; null for the rare
    // pair that isn't a clean motor slip.
    class: string | null;
  }

  // 12 catches = consent (brief): if the user didn't want it fixed, it wouldn't
  // have been caught 12×. Mirrors TIER1_MIN_OBSERVATIONS engine-side.
  const READY_THRESHOLD = 12;

  // One calendar day's typing rollup, as the engine persists it (UTC-dated, to
  // match the dated motor snapshots). coord + precis === slips always.
  interface ProgressDay {
    date: string;
    words: number;
    slips: number;
    coord: number;
    precis: number;
  }

  let activeTab: Tab = "statistics";
  let patterns: ImpactPattern[] = [];
  let progressDays: ProgressDay[] = [];

  // The command already returns patterns sorted by obs desc, so each group keeps
  // that order (the brief's "sorted by obs desc").
  $: ready = patterns.filter((p) => p.ready);
  $: observing = patterns.filter((p) => !p.ready);

  // ---- Statistics derivations -------------------------------------------
  //
  // Match the engine's UTC civil date for "today" (its rows are UTC-dated), so
  // the right entry is picked regardless of timezone. The two metric cards and
  // the two composition %s are all TODAY's live numbers — and because the
  // engine guarantees coord + precis === slips, coord% + precis% === slip rate
  // exactly. The trend sparklines are the historical shape (weekly rollup).
  const pad2 = (n: number) => String(n).padStart(2, "0");
  const todayKey = (() => {
    const d = new Date();
    return `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`;
  })();

  const round1 = (x: number) => Math.round(x * 10) / 10;
  const fmt1 = (x: number) => x.toFixed(1);

  $: today = progressDays.find((d) => d.date === todayKey) ?? null;
  $: wordsToday = today?.words ?? 0;
  $: hasToday = today !== null && today.words > 0;
  $: coordPct = hasToday ? round1((today!.coord / today!.words) * 100) : null;
  $: precisPct = hasToday ? round1((today!.precis / today!.words) * 100) : null;
  // Slip rate shown = the sum of the two displayed parts, so the brief's
  // "X% + Y% = Z%" holds EXACTLY on screen (rounding each independently could
  // otherwise make 6.2 + 8.3 ≠ 14.6). The engine guarantees coord+precis==slips.
  $: slipPct =
    coordPct !== null && precisPct !== null ? round1(coordPct + precisPct) : null;

  // Last 7 calendar days ending today (UTC), absent days drawn as empty bars.
  // The block is hidden until ≥2 days of data exist (brief).
  $: last7 = (() => {
    const map = new Map(progressDays.map((d) => [d.date, d]));
    const base = Date.parse(`${todayKey}T00:00:00Z`);
    const out: { weekday: string; words: number }[] = [];
    for (let i = 6; i >= 0; i--) {
      const dt = new Date(base - i * 86_400_000);
      const key = `${dt.getUTCFullYear()}-${pad2(dt.getUTCMonth() + 1)}-${pad2(dt.getUTCDate())}`;
      out.push({
        weekday: dt.toLocaleDateString(undefined, { weekday: "short", timeZone: "UTC" }),
        words: map.get(key)?.words ?? 0,
      });
    }
    return out;
  })();
  $: maxBar = Math.max(1, ...last7.map((d) => d.words));
  // Nonzero days get a small floor so a light day still reads as a bar.
  const barHeight = (words: number, max: number) =>
    words === 0 ? 0 : Math.max(8, (words / max) * 100);

  // Weekly rollup for the trend lines: fixed 7-day buckets (epoch-aligned).
  // A line needs ≥2 weeks of points; below that the row shows just the %.
  $: weekly = (() => {
    const buckets = new Map<number, { words: number; coord: number; precis: number }>();
    for (const d of progressDays) {
      const day = Math.floor(Date.parse(`${d.date}T00:00:00Z`) / 86_400_000);
      const b = buckets.get(Math.floor(day / 7)) ?? { words: 0, coord: 0, precis: 0 };
      b.words += d.words;
      b.coord += d.coord;
      b.precis += d.precis;
      buckets.set(Math.floor(day / 7), b);
    }
    return [...buckets.keys()]
      .sort((a, b) => a - b)
      .map((k) => {
        const b = buckets.get(k)!;
        return {
          coord: b.words ? (b.coord / b.words) * 100 : 0,
          precis: b.words ? (b.precis / b.words) * 100 : 0,
        };
      });
  })();
  $: coordSeries = weekly.map((w) => w.coord);
  $: precisSeries = weekly.map((w) => w.precis);

  // Sparkline geometry. Neutral shape only — never good/bad colored; a flat
  // series reads as steady (drawn mid-height).
  const TREND_W = 120;
  const TREND_H = 26;
  function trendPoints(series: number[]): string {
    if (series.length < 2) return "";
    const min = Math.min(...series);
    const max = Math.max(...series);
    const span = max - min || 1;
    const n = series.length;
    return series
      .map((v, i) => {
        const x = (i / (n - 1)) * TREND_W;
        const y = max === min ? TREND_H / 2 : TREND_H - ((v - min) / span) * TREND_H;
        return `${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(" ");
  }
  function trendEnd(series: number[]): { x: number; y: number } | null {
    const pts = trendPoints(series);
    if (!pts) return null;
    const last = pts.split(" ").pop()!.split(",");
    return { x: parseFloat(last[0]), y: parseFloat(last[1]) };
  }

  // Today's date, e.g. "Tuesday, 3 June". Built from parts so the day-before-
  // month order is locale-stable (matches the mockup), not en-US's "June 3".
  const now = new Date();
  const dateLabel = `${now.toLocaleDateString(undefined, {
    weekday: "long",
  })}, ${now.getDate()} ${now.toLocaleDateString(undefined, { month: "long" })}`;

  // Focus anchors. The scroll region is focusable so a no-mouse user can scroll
  // the Impact ledger with ↑/↓/space (it shows a ring when focused).
  let rootEl: HTMLElement;
  let statTabEl: HTMLButtonElement;
  let impactTabEl: HTMLButtonElement;
  let scrollEl: HTMLElement;

  function loadPatterns() {
    invoke<ImpactPattern[]>("read_word_patterns")
      .then((p) => {
        patterns = p ?? [];
      })
      .catch(() => {
        // A read failure leaves the last-known list rather than blanking the
        // ledger; the panel is read-only so there's nothing to retry.
      });
  }

  function loadProgress() {
    invoke<ProgressDay[]>("read_progress_stats")
      .then((d) => {
        progressDays = d ?? [];
      })
      .catch(() => {
        // Read-only; keep the last-known days rather than blanking.
      });
  }

  // `moveFocus` is set for keyboard switching (1/2/←/→) so the focus ring
  // follows the selected tab — roving tabindex keeps the inactive tab out of
  // the tab order, and we move focus onto the newly selected one. A click
  // already focuses its button natively, so it doesn't need this.
  function selectTab(tab: Tab, moveFocus = false) {
    activeTab = tab;
    if (moveFocus) {
      tick().then(() => {
        (tab === "statistics" ? statTabEl : impactTabEl)?.focus();
      });
    }
  }

  function close() {
    getCurrentWindow()
      .hide()
      .catch((e) => console.error("progress panel hide failed:", e));
  }

  // Minimal focus trap — a separate window, so App.svelte's trap doesn't cover
  // it (same pattern as PracticePanel). Also owns the one-hand tab switching.
  function tabbables(): HTMLElement[] {
    if (!rootEl) return [];
    const sel =
      'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])';
    return Array.from(rootEl.querySelectorAll<HTMLElement>(sel)).filter(
      (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length),
    );
  }

  function onRootKeydown(event: KeyboardEvent) {
    // Never react to a modifier combo — the brief bans chords everywhere.
    if (event.metaKey || event.ctrlKey || event.altKey) return;

    if (event.key === "Escape") {
      event.preventDefault();
      close();
      return;
    }

    // Tab switching: 1/2 or ←/→. These commit immediately (the segmented
    // control is two items, not the sidebar). ↑/↓/space are left alone so they
    // scroll the ledger natively.
    if (event.key === "1") {
      event.preventDefault();
      selectTab("statistics", true);
      return;
    }
    if (event.key === "2") {
      event.preventDefault();
      selectTab("impact", true);
      return;
    }
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      event.preventDefault();
      selectTab(activeTab === "statistics" ? "impact" : "statistics", true);
      return;
    }

    // Focus trap on Tab.
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

  // Land the ring on the active tab on open, so the panel never opens ringless.
  async function focusActiveTab() {
    await tick();
    (activeTab === "statistics" ? statTabEl : impactTabEl)?.focus();
  }

  onMount(() => {
    // Re-open always returns to Statistics (the primary mirror) and refreshes
    // the learned-pattern read.
    const offOpen = listen("progress://open", () => {
      activeTab = "statistics";
      loadPatterns();
      loadProgress();
      focusActiveTab();
    });
    loadPatterns();
    loadProgress();
    focusActiveTab();
    return () => {
      offOpen.then((off) => off());
    };
  });
</script>

<svelte:window on:keydown={onRootKeydown} />

<div class="panel" bind:this={rootEl}>
  <button class="close" aria-label="Close progress" on:click={close}>
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round">
      <line x1="6" y1="6" x2="18" y2="18" /><line x1="18" y1="6" x2="6" y2="18" />
    </svg>
  </button>

  <header class="panel-header">
    <h1>Progress</h1>
    <span class="date">{dateLabel}</span>
  </header>

  <div class="tabs" role="tablist" aria-label="Progress views">
    <button
      class="tab"
      class:active={activeTab === "statistics"}
      role="tab"
      id="tab-statistics"
      aria-selected={activeTab === "statistics"}
      aria-controls="panel-progress"
      tabindex={activeTab === "statistics" ? 0 : -1}
      bind:this={statTabEl}
      on:click={() => selectTab("statistics")}
    >
      Statistics
    </button>
    <button
      class="tab"
      class:active={activeTab === "impact"}
      role="tab"
      id="tab-impact"
      aria-selected={activeTab === "impact"}
      aria-controls="panel-progress"
      tabindex={activeTab === "impact" ? 0 : -1}
      bind:this={impactTabEl}
      on:click={() => selectTab("impact")}
    >
      Impact
    </button>
  </div>

  <div
    class="scroll"
    role="tabpanel"
    id="panel-progress"
    tabindex="0"
    aria-labelledby={activeTab === "statistics" ? "tab-statistics" : "tab-impact"}
    bind:this={scrollEl}
  >
    {#if activeTab === "statistics"}
      <!-- Statistics — how I type. Live today numbers + accumulated trends.
           Mirror not scoreboard: neutral single color, never red, no targets;
           a flat trend reads as steady. -->
      {#if progressDays.length === 0}
        <div class="empty">
          <p class="empty-lead">This view mirrors how you type.</p>
          <p class="empty-sub">
            It fills in once there's a day of typing to reflect.
          </p>
        </div>
      {:else}
        <div class="stats">
          <div class="cards">
            <div class="card">
              <div class="card-label">Words today</div>
              <div class="card-value">{wordsToday.toLocaleString()}</div>
            </div>
            <div class="card">
              <div class="card-label">Slip rate</div>
              <div class="card-value">
                {slipPct === null ? "—" : `${fmt1(slipPct)}%`}
              </div>
            </div>
          </div>

          {#if progressDays.length >= 2}
            <section class="block">
              <h2 class="block-title">Last 7 days</h2>
              <div class="bars">
                {#each last7 as d}
                  <div class="bar-col">
                    <div class="bar-track">
                      <div class="bar" style="height: {barHeight(d.words, maxBar)}%"></div>
                    </div>
                    <div class="bar-label">{d.weekday}</div>
                  </div>
                {/each}
              </div>
            </section>
          {/if}

          <section class="block">
            <h2 class="block-title">What the slip rate is made of</h2>
            {#if slipPct === null}
              <p class="quiet-line">No typing yet today.</p>
            {:else}
              {#each [{ name: "Coordination", caption: "right keys, right order", pct: coordPct, series: coordSeries }, { name: "Precision", caption: "right key, clean hit", pct: precisPct, series: precisSeries }] as row}
                {@const pts = trendPoints(row.series)}
                {@const end = trendEnd(row.series)}
                <div class="comp-row">
                  <div class="comp-head">
                    <div class="comp-name">{row.name}</div>
                    <div class="comp-caption">{row.caption}</div>
                  </div>
                  <div class="comp-trend">
                    {#if pts}
                      <svg viewBox="0 0 {TREND_W} {TREND_H}" width={TREND_W} height={TREND_H} aria-hidden="true">
                        <polyline points={pts} fill="none" stroke="var(--trend-line)" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" />
                        {#if end}
                          <circle cx={end.x} cy={end.y} r="2.5" fill="var(--trend-line)" />
                        {/if}
                      </svg>
                    {/if}
                  </div>
                  <div class="comp-pct">{fmt1(row.pct ?? 0)}%</div>
                </div>
              {/each}
              <p class="comp-sum">
                {fmt1(coordPct ?? 0)}% coordination + {fmt1(precisPct ?? 0)}% precision =
                your {fmt1(slipPct)}% slip rate.
              </p>
            {/if}
          </section>
        </div>
      {/if}
    {:else}
      <!-- Impact — what TypeAssist has learned and is ready to smooth. Fully
           read-only: no buttons, no actions, no tap targets (brief). -->
      <div class="impact">
        <p class="banner">
          Observe-only for now — nothing is changed yet. This is what TypeAssist
          is ready to smooth once correction turns on.
        </p>

        {#if patterns.length === 0}
          <p class="impact-empty">
            Nothing learned yet — TypeAssist is still getting to know how you
            type.
          </p>
        {:else}
          <p class="summary">
            {ready.length} {ready.length === 1 ? "pattern" : "patterns"} ready ·
            {observing.length} still observing
          </p>

          {#if ready.length > 0}
            <ul class="ledger">
              {#each ready as p}
                <li class="row">
                  <span class="pair">{p.typed} → {p.target}</span>
                  <span class="meta">
                    {#if p.class}<span class="tag">{p.class}</span>{/if}
                    <span class="obs">{Math.round(p.obs)} obs</span>
                    <span class="pill ready">Ready</span>
                  </span>
                </li>
              {/each}
            </ul>
          {/if}

          {#if observing.length > 0}
            <div class="divider" aria-hidden="true">
              <span>{READY_THRESHOLD}-observation threshold</span>
            </div>
            <ul class="ledger">
              {#each observing as p}
                <li class="row">
                  <span class="pair">{p.typed} → {p.target}</span>
                  <span class="meta">
                    {#if p.class}<span class="tag">{p.class}</span>{/if}
                    <span class="obs">{Math.round(p.obs)} / {READY_THRESHOLD}</span>
                    <span class="pill observing">Observing</span>
                  </span>
                </li>
              {/each}
            </ul>
          {/if}
        {/if}
      </div>
    {/if}
  </div>

  <footer class="panel-footer">
    <svg class="kbd" viewBox="0 0 24 16" width="20" height="14" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round">
      <rect x="1" y="2" width="22" height="12" rx="2" />
      <line x1="5" y1="6" x2="5" y2="6" /><line x1="9" y1="6" x2="9" y2="6" />
      <line x1="13" y1="6" x2="13" y2="6" /><line x1="17" y1="6" x2="17" y2="6" />
      <line x1="8" y1="10" x2="16" y2="10" />
    </svg>
    <span>Switch views — click, or press 1 / 2</span>
  </footer>
</div>

<style>
  /* The progress window is transparent + frameless: paint our own rounded card
     and let the corners outside it be see-through (same as PracticePanel; this
     component only ever mounts in the progress window). */
  :global(body) {
    background: transparent !important;
  }

  .panel {
    position: relative;
    box-sizing: border-box;
    height: 100vh;
    padding: 1.25rem 1.25rem 1rem;
    border-radius: 16px;
    background: canvas;
    border: 1px solid var(--hairline);
    color: canvastext;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    /* Neutral trend-line color — a mirror, never a scoreboard. Single tone,
       never red/green; a flat line just reads as steady. */
    --trend-line: color-mix(in srgb, canvastext 45%, canvas);
  }

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

  /* ---- header: "Progress" + today's date ---- */
  .panel-header {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.75rem;
    padding-right: 2rem; /* clear the close button */
    margin-bottom: 1rem;
  }
  .panel-header h1 {
    margin: 0;
    font-size: 1.5rem;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .date {
    color: var(--text-secondary);
    font-size: 0.9rem;
  }

  /* ---- segmented control. No tab-switch animation: the active highlight just
     jumps (honours Reduce Motion by construction). ---- */
  .tabs {
    display: flex;
    gap: 0.25rem;
    padding: 0.25rem;
    border: 1px solid var(--hairline);
    border-radius: 12px;
    margin-bottom: 1rem;
  }
  .tab {
    flex: 1;
    min-height: 40px;
    padding: 0.5rem 1rem;
    font: inherit;
    font-weight: 600;
    color: var(--text-secondary);
    background: transparent;
    border: none;
    border-radius: 9px;
    cursor: pointer;
  }
  .tab.active {
    color: canvastext;
    background: color-mix(in srgb, canvastext 10%, canvas);
  }
  .tab:hover:not(.active) {
    background: color-mix(in srgb, canvastext 5%, canvas);
  }
  .tab:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }

  /* ---- the internal scroll region — the only thing that scrolls, so the panel
     never grows down toward the Dock. Focusable so ↑/↓/space scroll it. ---- */
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    outline: none;
  }
  .scroll:focus-visible {
    outline: 3px solid var(--focus-ring);
    outline-offset: -3px;
    border-radius: 10px;
  }

  /* ---- Statistics placeholder (replaced in step 4) ---- */
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    text-align: center;
    gap: 0.4rem;
    padding: 1rem;
  }
  .empty-lead {
    margin: 0;
    font-size: 1.05rem;
    font-weight: 600;
  }
  .empty-sub {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.95rem;
    max-width: 18rem;
  }

  /* ---- Statistics ---- */
  .stats {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
  }
  .cards {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.75rem;
  }
  .card {
    padding: 0.85rem 1rem;
    border: 1px solid var(--hairline);
    border-radius: 12px;
  }
  .card-label {
    color: var(--text-secondary);
    font-size: 0.9rem;
    margin-bottom: 0.35rem;
  }
  .card-value {
    font-size: 1.9rem;
    font-weight: 700;
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
  }

  .block-title {
    margin: 0 0 0.7rem;
    font-size: 0.9rem;
    font-weight: 600;
    color: var(--text-secondary);
  }

  /* Last 7 days — uniform neutral bars, no highlighted "today". */
  .bars {
    display: flex;
    align-items: flex-end;
    gap: 0.5rem;
    height: 92px;
  }
  .bar-col {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.35rem;
    height: 100%;
  }
  .bar-track {
    flex: 1;
    width: 100%;
    display: flex;
    align-items: flex-end;
  }
  .bar {
    width: 100%;
    border-radius: 5px 5px 0 0;
    background: color-mix(in srgb, canvastext 22%, canvas);
    min-height: 0;
  }
  .bar-label {
    color: var(--text-secondary);
    font-size: 0.78rem;
  }

  /* Composition — each row: name + caption, a neutral trend line, the %. */
  .comp-row {
    display: grid;
    grid-template-columns: 1fr auto auto;
    align-items: center;
    gap: 0.9rem;
    padding: 0.6rem 0;
    border-bottom: 1px solid var(--hairline);
  }
  .comp-name {
    font-size: 1rem;
    font-weight: 600;
  }
  .comp-caption {
    color: var(--text-secondary);
    font-size: 0.82rem;
  }
  .comp-trend {
    width: 120px;
    height: 26px;
    display: flex;
    align-items: center;
  }
  .comp-pct {
    font-size: 1.05rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    min-width: 3.5rem;
    text-align: right;
  }
  .comp-sum {
    margin: 0.7rem 0 0;
    color: var(--text-secondary);
    font-size: 0.88rem;
    line-height: 1.45;
  }
  .quiet-line {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.95rem;
  }

  /* ---- Impact ---- */
  .impact {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
  }
  .banner {
    margin: 0;
    padding: 0.7rem 0.85rem;
    border-radius: 10px;
    font-size: 0.85rem;
    line-height: 1.4;
    color: canvastext;
    background: color-mix(in srgb, var(--focus-ring) 10%, canvas);
    border: 1px solid color-mix(in srgb, var(--focus-ring) 28%, canvas);
  }
  .impact-empty {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.95rem;
    line-height: 1.5;
  }
  .summary {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.9rem;
  }
  .ledger {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.55rem 0;
    border-bottom: 1px solid var(--hairline);
  }
  .pair {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.98rem;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-shrink: 0;
  }
  .tag {
    color: var(--text-secondary);
    font-size: 0.72rem;
    letter-spacing: 0.02em;
    padding: 0.1rem 0.4rem;
    border-radius: 6px;
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  .obs {
    color: var(--text-secondary);
    font-size: 0.85rem;
    font-variant-numeric: tabular-nums;
  }
  .pill {
    padding: 0.15rem 0.55rem;
    border-radius: 999px;
    font-size: 0.78rem;
    font-weight: 600;
    white-space: nowrap;
  }
  .pill.ready {
    color: canvastext;
    background: color-mix(in srgb, var(--focus-ring) 16%, canvas);
    border: 1px solid color-mix(in srgb, var(--focus-ring) 34%, canvas);
  }
  .pill.observing {
    color: var(--text-secondary);
    background: color-mix(in srgb, canvastext 7%, canvas);
    border: 1px solid var(--hairline);
  }

  /* Threshold divider between the Ready and Observing groups. */
  .divider {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin: 0.35rem 0;
    color: var(--text-secondary);
    font-size: 0.78rem;
  }
  .divider::before,
  .divider::after {
    content: "";
    flex: 1;
    height: 1px;
    background: var(--hairline);
  }

  /* ---- footer hint ---- */
  .panel-footer {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding-top: 0.85rem;
    margin-top: 0.5rem;
    border-top: 1px solid var(--hairline);
    color: var(--text-secondary);
    font-size: 0.82rem;
  }
  .kbd {
    flex-shrink: 0;
  }
</style>
