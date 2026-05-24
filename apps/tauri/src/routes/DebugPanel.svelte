<!--
  Builder's debug view — hidden behind Cmd+Shift+D.

  Not a user feature. Utilitarian, monospace, exempt from the calm app voice.
  Lives on top of the regular UI so you can keep using the app while watching
  what the engine is doing.

  Listens to engine events emitted from `apps/tauri/src-tauri/src/engine.rs`:
    engine://keystroke       — every key (or backspace) + dwell + ingest cost
    engine://decision        — at a word boundary
    engine://injection       — when a correction fired
    engine://model-snapshot  — L2 BehaviouralModel state (per-key + per-finger)

  Layout:
    [resize handle — drag to resize, height persisted in sessionStorage]
    [strip: totals + Clear]
    [body: feed column | model column]
      feed:  scrolls independently
      model: title (fixed) + per-finger (pinned) + per-key (scrolls independently)
-->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  type KeystrokePayload = {
    key: string;
    dwell_ms: number;
    ingest_latency_ms: number;
  };
  type DecisionPayload = {
    word: string;
    matched: boolean;
    replacement: string | null;
    decision_latency_ms: number;
  };
  type InjectionPayload = {
    delete_count: number;
    replacement: string;
    injection_latency_ms: number;
  };

  // Mirrors `behavioural_model::ModelSnapshot`.
  type Hand = "left" | "right";
  type Finger = "thumb" | "index" | "middle" | "ring" | "pinky";
  type KeyTimingRow = {
    key: string;
    count: number;
    avg_dwell_ms: number;
    avg_interval_ms: number;
    hand: Hand | null;
    finger: Finger | null;
  };
  type FingerTimingRow = {
    hand: Hand;
    finger: Finger;
    total_count: number;
    avg_dwell_ms: number;
    avg_interval_ms: number;
  };
  type ModelSnapshot = {
    timing: { per_key: KeyTimingRow[]; per_finger: FingerTimingRow[] };
  };

  type Row =
    | { id: number; kind: "key"; key: string; dwell_ms: number; ingest_latency_ms: number }
    | {
        id: number;
        kind: "decision";
        word: string;
        matched: boolean;
        replacement: string | null;
        decision_latency_ms: number;
      }
    | {
        id: number;
        kind: "injection";
        delete_count: number;
        replacement: string;
        injection_latency_ms: number;
      };

  // Cap the feed so a long session doesn't pin unbounded memory / DOM.
  const MAX_ROWS = 500;

  let rows: Row[] = [];
  let nextId = 0;

  // Running totals — kept separate from `rows` so the strip stays accurate
  // even after old rows are evicted from the visible feed.
  let keystrokeCount = 0;
  let correctionCount = 0;
  let latencySum = 0;
  let latencyMax = 0;
  $: latencyAvg = correctionCount === 0 ? 0 : latencySum / correctionCount;

  // Latest L2 snapshot. Replaced wholesale on every model-snapshot event.
  let modelRows: KeyTimingRow[] = [];
  let fingerRows: FingerTimingRow[] = [];

  let feedEl: HTMLDivElement;
  let unlistens: UnlistenFn[] = [];

  // ---- Resize support ---------------------------------------------------
  // Panel height (px) is user-controlled via the top drag handle and survives
  // toggling the panel (Cmd+Shift+D close/reopen) via sessionStorage. Stays a
  // builder tool: no keyboard arrow support, no a11y wiring.
  const STORAGE_KEY = "typeassist:debug-panel-height";
  const MIN_HEIGHT = 220;
  // Leave at least this much of the regular app visible above the panel.
  const MIN_APP_VISIBLE = 80;

  let panelHeight = 480; // sensible default before onMount loads window/storage
  let dragging = false;
  let dragStartY = 0;
  let dragStartHeight = 0;

  function clampHeight(px: number): number {
    const max = Math.max(MIN_HEIGHT, window.innerHeight - MIN_APP_VISIBLE);
    return Math.max(MIN_HEIGHT, Math.min(max, Math.round(px)));
  }

  function setPanelHeight(px: number) {
    panelHeight = clampHeight(px);
    try {
      sessionStorage.setItem(STORAGE_KEY, String(panelHeight));
    } catch {
      // sessionStorage can be unavailable in some webview contexts — ignore.
    }
  }

  function onResizeDown(e: PointerEvent) {
    dragging = true;
    dragStartY = e.clientY;
    dragStartHeight = panelHeight;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault();
  }
  function onResizeMove(e: PointerEvent) {
    if (!dragging) return;
    // Dragging UP grows the panel (clientY decreases).
    const delta = dragStartY - e.clientY;
    setPanelHeight(dragStartHeight + delta);
  }
  function onResizeUp(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      // releasePointerCapture throws if capture was already lost — fine.
    }
  }

  function push(row: Row) {
    const next = [...rows, row];
    if (next.length > MAX_ROWS) next.splice(0, next.length - MAX_ROWS);
    rows = next;
    // Auto-scroll to bottom (newest) after Svelte flushes.
    queueMicrotask(() => {
      if (feedEl) feedEl.scrollTop = feedEl.scrollHeight;
    });
  }

  function clear() {
    rows = [];
    keystrokeCount = 0;
    correctionCount = 0;
    latencySum = 0;
    latencyMax = 0;
    // Note: Clear only resets the *view*. The L2 model in the backend keeps
    // its own counts — modelRows/fingerRows will repopulate on the next keystroke.
  }

  onMount(async () => {
    // Restore height: stored value if present, otherwise a tall 60% default.
    let initial: number | null = null;
    try {
      const stored = sessionStorage.getItem(STORAGE_KEY);
      if (stored) {
        const n = parseInt(stored, 10);
        if (Number.isFinite(n)) initial = n;
      }
    } catch {
      // ignore
    }
    panelHeight = clampHeight(initial ?? Math.round(window.innerHeight * 0.6));

    unlistens.push(
      await listen<KeystrokePayload>("engine://keystroke", (e) => {
        keystrokeCount += 1;
        push({
          id: nextId++,
          kind: "key",
          key: e.payload.key,
          dwell_ms: e.payload.dwell_ms,
          ingest_latency_ms: e.payload.ingest_latency_ms,
        });
      }),
    );
    unlistens.push(
      await listen<DecisionPayload>("engine://decision", (e) => {
        push({
          id: nextId++,
          kind: "decision",
          word: e.payload.word,
          matched: e.payload.matched,
          replacement: e.payload.replacement,
          decision_latency_ms: e.payload.decision_latency_ms,
        });
      }),
    );
    unlistens.push(
      await listen<InjectionPayload>("engine://injection", (e) => {
        correctionCount += 1;
        latencySum += e.payload.injection_latency_ms;
        if (e.payload.injection_latency_ms > latencyMax) {
          latencyMax = e.payload.injection_latency_ms;
        }
        push({
          id: nextId++,
          kind: "injection",
          delete_count: e.payload.delete_count,
          replacement: e.payload.replacement,
          injection_latency_ms: e.payload.injection_latency_ms,
        });
      }),
    );
    unlistens.push(
      await listen<ModelSnapshot>("engine://model-snapshot", (e) => {
        modelRows = e.payload.timing.per_key;
        fingerRows = e.payload.timing.per_finger;
      }),
    );
  });

  onDestroy(() => {
    for (const u of unlistens) u();
    unlistens = [];
  });

  // Render-helpers — keep templates terse.
  function fmtMs(n: number): string {
    return n.toFixed(2);
  }
  function fmtKey(k: string): string {
    if (k === " ") return "␣";
    if (k === "\t") return "⇥";
    if (k === "\n" || k === "\r") return "⏎";
    return k;
  }
  function fmtFinger(hand: Hand, finger: Finger): string {
    const h = hand === "left" ? "L" : "R";
    return `${h} ${finger}`;
  }
  function fmtKeyFinger(row: KeyTimingRow): string {
    if (!row.hand || !row.finger) return "—";
    return fmtFinger(row.hand, row.finger);
  }
</script>

<section
  class="debug"
  aria-label="Engine debug panel"
  style="height: {panelHeight}px"
>
  <!-- Drag handle (top edge). Pointer events; height persists in sessionStorage. -->
  <div
    class="resize-handle"
    class:dragging
    role="separator"
    aria-orientation="horizontal"
    aria-label="Resize debug panel"
    on:pointerdown={onResizeDown}
    on:pointermove={onResizeMove}
    on:pointerup={onResizeUp}
    on:pointercancel={onResizeUp}
  >
    <div class="resize-grip" />
  </div>

  <header class="strip">
    <div class="stat"><span class="stat-label">KEYS</span><span class="stat-val">{keystrokeCount}</span></div>
    <div class="stat"><span class="stat-label">CORR</span><span class="stat-val">{correctionCount}</span></div>
    <div class="stat"><span class="stat-label">AVG</span><span class="stat-val">{fmtMs(latencyAvg)} ms</span></div>
    <div class="stat"><span class="stat-label">MAX</span><span class="stat-val">{fmtMs(latencyMax)} ms</span></div>
    <div class="spacer" />
    <button type="button" class="clear" on:click={clear}>Clear</button>
  </header>

  <div class="body">
    <!-- Left column: live event feed (scrolls independently). -->
    <section class="col col-feed" aria-label="Event feed">
      <div class="col-head">FEED · newest at bottom</div>
      <div class="feed" bind:this={feedEl}>
        {#each rows as row (row.id)}
          {#if row.kind === "key"}
            <div class="row row-key">
              <span class="tag">KEY</span>
              <span class="col-key">{fmtKey(row.key)}</span>
              <span class="col-dwell">dwell {row.dwell_ms}ms</span>
              <span class="col-lat">ingest {fmtMs(row.ingest_latency_ms)} ms</span>
            </div>
          {:else if row.kind === "decision"}
            {#if row.matched}
              <div class="row row-decision row-matched">
                <span class="tag">DECISION</span>
                <span class="badge badge-corrected">CORRECTED</span>
                <span class="col-word">{row.word} → {row.replacement}</span>
                <span class="col-lat">decided in {fmtMs(row.decision_latency_ms)} ms</span>
              </div>
            {:else}
              <div class="row row-decision">
                <span class="tag">DECISION</span>
                <span class="badge badge-left">LEFT ALONE</span>
                <span class="col-word">{row.word} — no match</span>
                <span class="col-lat">decided in {fmtMs(row.decision_latency_ms)} ms</span>
              </div>
            {/if}
          {:else if row.kind === "injection"}
            <div class="row row-injection">
              <span class="tag">INJECT</span>
              <span class="col-word">deleted {row.delete_count}, typed "{row.replacement}"</span>
              <span class="col-lat">e2e {fmtMs(row.injection_latency_ms)} ms</span>
            </div>
          {/if}
        {/each}
        {#if rows.length === 0}
          <div class="empty">waiting for keystrokes…</div>
        {/if}
      </div>
    </section>

    <!-- Right column: L2 model state. Per-finger pinned at top, per-key scrolls. -->
    <section class="col col-model" aria-label="L2 model state">
      <div class="col-head">MODEL STATE</div>

      <!-- Pinned: small enough to always fit, important enough not to scroll away. -->
      <div class="model-pinned">
        <div class="model-sub">PER FINGER · anatomical order</div>
        <div class="finger-table">
          <div class="finger-row finger-head">
            <span class="col-ff">finger</span>
            <span class="col-fn num">count</span>
            <span class="col-fd num">avg dwell</span>
            <span class="col-fi num">avg interval</span>
          </div>
          {#if fingerRows.length === 0}
            <div class="empty">no fingers observed yet…</div>
          {:else}
            {#each fingerRows as f (`${f.hand}-${f.finger}`)}
              <div class="finger-row" class:finger-left={f.hand === "left"} class:finger-right={f.hand === "right"}>
                <span class="col-ff">{fmtFinger(f.hand, f.finger)}</span>
                <span class="col-fn num">{f.total_count}</span>
                <span class="col-fd num">{fmtMs(f.avg_dwell_ms)} ms</span>
                <span class="col-fi num">{fmtMs(f.avg_interval_ms)} ms</span>
              </div>
            {/each}
          {/if}
        </div>
      </div>

      <!-- Scrolling: per-key table, header sticks while you scroll. -->
      <div class="model-scroll">
        <div class="model-sub model-sub-sticky">PER KEY · most-typed first</div>
        <div class="model-table">
          <div class="model-row model-head">
            <span class="col-mk">key</span>
            <span class="col-mf">finger</span>
            <span class="col-mn num">count</span>
            <span class="col-md num">avg dwell</span>
            <span class="col-mi num">avg interval</span>
          </div>
          {#if modelRows.length === 0}
            <div class="empty">no keys observed yet…</div>
          {:else}
            {#each modelRows as r (r.key)}
              <div class="model-row">
                <span class="col-mk">{fmtKey(r.key)}</span>
                <span class="col-mf">{fmtKeyFinger(r)}</span>
                <span class="col-mn num">{r.count}</span>
                <span class="col-md num">{fmtMs(r.avg_dwell_ms)} ms</span>
                <span class="col-mi num">{fmtMs(r.avg_interval_ms)} ms</span>
              </div>
            {/each}
          {/if}
        </div>
      </div>
    </section>
  </div>
</section>

<style>
  /* Utilitarian on purpose — this panel is a builder tool, not part of the
     calm user surface. Monospace, dense, dark background, terminal feel. */
  .debug {
    position: fixed;
    inset: auto 0 0 0;
    /* height is set inline from `panelHeight`; CSS only declares the floor. */
    min-height: 220px;
    background: #0b0d10;
    color: #e6e6e6;
    border-top: 1px solid #2a2f36;
    display: flex;
    flex-direction: column;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px;
    /* Above app content; below any future native menus. */
    z-index: 9999;
    box-shadow: 0 -8px 24px rgba(0, 0, 0, 0.4);
  }

  /* ---- Resize handle (top edge) ---------------------------------------- */
  .resize-handle {
    height: 10px;
    flex: 0 0 10px;
    cursor: ns-resize;
    background: #15191f;
    border-bottom: 1px solid #2a2f36;
    display: flex;
    align-items: center;
    justify-content: center;
    touch-action: none; /* let pointer events drive the drag, not scroll */
  }
  .resize-handle:hover,
  .resize-handle.dragging {
    background: #1c222a;
  }
  .resize-grip {
    width: 48px;
    height: 3px;
    background: #3a414a;
    border-radius: 2px;
  }
  .resize-handle:hover .resize-grip,
  .resize-handle.dragging .resize-grip {
    background: #5a6470;
  }

  /* ---- Top stats strip -------------------------------------------------- */
  .strip {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 1.5rem;
    padding: 0.55rem 0.9rem;
    background: #15191f;
    border-bottom: 1px solid #2a2f36;
  }
  .stat {
    display: flex;
    align-items: baseline;
    gap: 0.45rem;
  }
  .stat-label {
    color: #7f8a96;
    letter-spacing: 0.08em;
  }
  .stat-val {
    color: #e6e6e6;
    font-weight: 600;
  }
  .spacer { flex: 1; }
  .clear {
    font: inherit;
    color: #e6e6e6;
    background: #2a2f36;
    border: 1px solid #3a414a;
    border-radius: 4px;
    padding: 0.3rem 0.85rem;
    cursor: pointer;
  }
  .clear:hover { background: #353c45; }
  .clear:focus { outline: 2px solid var(--focus-ring); outline-offset: 1px; }

  /* ---- Two equal columns: feed | model state --------------------------- */
  .body {
    flex: 1;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    min-height: 0; /* allow children to shrink-and-scroll instead of overflowing */
  }
  .col {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
  }
  .col-feed { border-right: 1px solid #2a2f36; }
  .col-model { background: #0d1117; }
  /* Section banner — one shared style so both columns read as peers. */
  .col-head {
    flex: 0 0 auto;
    padding: 0.45rem 0.75rem;
    color: #cdd5de;
    letter-spacing: 0.08em;
    font-weight: 600;
    background: #11161c;
    border-bottom: 1px solid #2a2f36;
  }

  /* ---- Feed (scrolls independently) ------------------------------------ */
  .feed {
    flex: 1;
    overflow-y: auto;
    padding: 0.4rem 0.55rem 0.6rem 0.55rem;
  }
  .empty {
    color: #7f8a96;
    padding: 0.55rem 0.75rem;
    font-style: italic;
  }
  .row {
    display: grid;
    grid-template-columns: 70px auto minmax(0, 1fr) auto;
    gap: 0.85rem;
    align-items: baseline;
    padding: 0.25rem 0.45rem;
    border-radius: 3px;
    white-space: nowrap;
  }
  .row-key { grid-template-columns: 70px auto minmax(0, 1fr) auto; }
  .row-injection { grid-template-columns: 70px minmax(0, 1fr) auto; }
  .row:nth-child(even) { background: rgba(255, 255, 255, 0.025); }

  .tag {
    color: #7f8a96;
    letter-spacing: 0.07em;
  }
  /* Distinct background tints + text labels — never color alone. */
  .badge {
    padding: 0 0.45rem;
    border-radius: 3px;
    letter-spacing: 0.07em;
    font-weight: 600;
  }
  .badge-corrected {
    background: #1f3a1f;
    color: #b6e3b6;
    border: 1px solid #2e5a2e;
  }
  .badge-left {
    background: #1f1f1f;
    color: #c8c8c8;
    border: 1px solid #3a3a3a;
  }

  .col-key { color: #e6e6e6; font-weight: 600; }
  .col-dwell, .col-lat { color: #8aa1b8; }
  .col-word { color: #d5d5d5; overflow: hidden; text-overflow: ellipsis; }

  /* Subtle left border on injection rows so the eye can scan correction events
     without relying on the green badge. */
  .row-injection,
  .row-matched {
    border-left: 2px solid #2e5a2e;
    padding-left: 0.6rem;
  }

  /* ---- Model state column --------------------------------------------- */
  /* Pinned section — never scrolls. Sized by content. */
  .model-pinned {
    flex: 0 0 auto;
    border-bottom: 1px solid #2a2f36;
    background: #0d1117;
  }
  /* Scrolling section — flexes to fill, scrolls when needed. */
  .model-scroll {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
    background: #0d1117;
  }
  .model-sub {
    padding: 0.45rem 0.75rem 0.25rem;
    color: #8893a0;
    letter-spacing: 0.08em;
    font-size: 11px;
  }
  .model-sub-sticky {
    position: sticky;
    top: 0;
    background: #0d1117;
    z-index: 1;
  }

  /* Per-finger rollup table. */
  .finger-table {
    padding: 0 0 0.35rem;
  }
  .finger-row {
    display: grid;
    grid-template-columns: 90px 60px minmax(0, 1fr) minmax(0, 1fr);
    column-gap: 0.6rem;
    align-items: baseline;
    padding: 0.22rem 0.75rem;
    white-space: nowrap;
  }
  .finger-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.3rem;
    margin-bottom: 0.15rem;
  }
  /* Subtle hand grouping — slightly different row tints so left/right read as
     two clusters at a glance, but "L"/"R" labels still carry the meaning so it
     isn't color-only. */
  .finger-left:not(.finger-head) { background: rgba(120, 160, 220, 0.045); }
  .finger-right:not(.finger-head) { background: rgba(220, 160, 120, 0.045); }
  .col-ff { color: #e6e6e6; font-weight: 600; }
  .col-fn { color: #d5d5d5; }
  .col-fd, .col-fi { color: #8aa1b8; }

  /* Per-key table — scrolls inside .model-scroll. Header is sticky inside it. */
  .model-table {
    padding: 0.25rem 0 0.4rem;
  }
  .model-row {
    display: grid;
    grid-template-columns: 36px 84px 56px minmax(0, 1fr) minmax(0, 1fr);
    column-gap: 0.55rem;
    align-items: baseline;
    padding: 0.22rem 0.75rem;
    white-space: nowrap;
  }
  .model-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-top: 0.25rem;
    padding-bottom: 0.3rem;
  }
  .model-row:not(.model-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .col-mk { color: #e6e6e6; font-weight: 600; }
  .col-mf { color: #8aa1b8; }
  .col-mn { color: #d5d5d5; }
  .col-md, .col-mi { color: #8aa1b8; }

  /* Right-align every numeric column across both tables. Keeps the digit
     stacks lined up so the gradient is easy to read. Tabular numerals so
     proportional fonts don't reshape the column on each update. */
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
</style>
