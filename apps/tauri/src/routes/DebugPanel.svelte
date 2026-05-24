<!--
  Builder's debug view — hidden behind Cmd+Shift+D.

  Not a user feature. Utilitarian, monospace, exempt from the calm app voice.
  Lives on top of the regular UI so you can keep using the app while watching
  what the engine is doing.

  Listens to the three engine events emitted from `apps/tauri/src-tauri/src/engine.rs`:
    engine://keystroke   — every key (or backspace) + dwell
    engine://decision    — at a word boundary
    engine://injection   — when a correction fired
-->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";

  type KeystrokePayload = { key: string; dwell_ms: number };
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

  type Row =
    | { id: number; kind: "key"; key: string; dwell_ms: number }
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

  let feedEl: HTMLDivElement;
  let unlistens: UnlistenFn[] = [];

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
  }

  onMount(async () => {
    unlistens.push(
      await listen<KeystrokePayload>("engine://keystroke", (e) => {
        keystrokeCount += 1;
        push({
          id: nextId++,
          kind: "key",
          key: e.payload.key,
          dwell_ms: e.payload.dwell_ms,
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
</script>

<section
  class="debug"
  aria-label="Engine debug panel"
>
  <header class="strip">
    <div class="stat"><span class="stat-label">KEYS</span><span class="stat-val">{keystrokeCount}</span></div>
    <div class="stat"><span class="stat-label">CORR</span><span class="stat-val">{correctionCount}</span></div>
    <div class="stat"><span class="stat-label">AVG</span><span class="stat-val">{fmtMs(latencyAvg)} ms</span></div>
    <div class="stat"><span class="stat-label">MAX</span><span class="stat-val">{fmtMs(latencyMax)} ms</span></div>
    <div class="spacer" />
    <button type="button" class="clear" on:click={clear}>Clear</button>
  </header>

  <div class="feed" bind:this={feedEl}>
    {#each rows as row (row.id)}
      {#if row.kind === "key"}
        <div class="row row-key">
          <span class="tag">KEY</span>
          <span class="col-key">{fmtKey(row.key)}</span>
          <span class="col-dwell">dwell {row.dwell_ms}ms</span>
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

<style>
  /* Utilitarian on purpose — this panel is a builder tool, not part of the
     calm user surface. Monospace, dense, dark background, terminal feel. */
  .debug {
    position: fixed;
    inset: auto 0 0 0;
    height: 45vh;
    min-height: 240px;
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
  .strip {
    display: flex;
    align-items: center;
    gap: 1.25rem;
    padding: 0.5rem 0.85rem;
    background: #15191f;
    border-bottom: 1px solid #2a2f36;
  }
  .stat {
    display: flex;
    align-items: baseline;
    gap: 0.4rem;
  }
  .stat-label {
    color: #7f8a96;
    letter-spacing: 0.06em;
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
    padding: 0.25rem 0.7rem;
    cursor: pointer;
  }
  .clear:hover { background: #353c45; }
  .clear:focus { outline: 2px solid var(--focus-ring); outline-offset: 1px; }

  .feed {
    flex: 1;
    overflow-y: auto;
    padding: 0.35rem 0.5rem 0.5rem 0.5rem;
  }
  .empty {
    color: #7f8a96;
    padding: 0.5rem;
    font-style: italic;
  }
  .row {
    display: grid;
    grid-template-columns: 70px auto 1fr auto;
    gap: 0.75rem;
    align-items: baseline;
    padding: 0.15rem 0.35rem;
    border-radius: 3px;
    white-space: nowrap;
  }
  /* Key rows have one fewer column — collapse the badge slot. */
  .row-key { grid-template-columns: 70px 1fr auto; }
  .row-injection { grid-template-columns: 70px 1fr auto; }
  .row:nth-child(even) { background: rgba(255, 255, 255, 0.02); }

  .tag {
    color: #7f8a96;
    letter-spacing: 0.06em;
  }
  /* Distinct background tints + text labels — never color alone. */
  .badge {
    padding: 0 0.4rem;
    border-radius: 3px;
    letter-spacing: 0.06em;
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
  .row-injection {
    border-left: 2px solid #2e5a2e;
    padding-left: 0.55rem;
  }
  .row-matched {
    border-left: 2px solid #2e5a2e;
    padding-left: 0.55rem;
  }
</style>
