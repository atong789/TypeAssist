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
  type HandStats = {
    hand: Hand;
    count: number;
    avg_dwell_ms: number;
    avg_interval_ms: number;
  };
  type AsymmetrySnapshot = {
    left: HandStats;
    right: HandStats;
    dwell_ratio: number;
    interval_ratio: number;
    overall_score: number;
    steadier_hand: Hand | null;
  };
  type KeyGhostRow = {
    key: string;
    hand: Hand | null;
    finger: Finger | null;
    events: number;
    short_dwell: number;
    rapid_repeat: number;
    self_corrected: number;
  };
  type FingerGhostRow = {
    hand: Hand;
    finger: Finger;
    events: number;
  };
  type HandGhostRow = {
    hand: Hand;
    events: number;
    short_dwell: number;
    rapid_repeat: number;
    self_corrected: number;
  };
  type GhostKeysSnapshot = {
    total_ghost_events: number;
    short_dwell_count: number;
    rapid_repeat_count: number;
    self_corrected_count: number;
    /// Always exactly two rows: [Left, Right]. Both present even with zero
    /// data, so the L vs R comparison is always visible.
    per_hand: HandGhostRow[];
    per_key: KeyGhostRow[];
    per_finger: FingerGhostRow[];
    /// Adaptive cutoff in ms; 0 while warming up.
    dwell_threshold_ms: number;
    /// Keystrokes left before the threshold goes live; 0 once active.
    warmup_remaining: number;
  };
  type PairSlipRow = {
    aimed_for: string;
    hit_instead: string;
    hand: Hand | null;
    finger: Finger | null;
    count: number;
    last_seen_ms: number;
  };
  type SlipsSnapshot = {
    total_slips: number;
    per_pair: PairSlipRow[];
    map_swap_pairs: number;
    map_key_confidence: number;
  };
  type SlipPayload = {
    aimed_for: string;
    hit_instead: string;
    hand: Hand | null;
    finger: Finger | null;
    timestamp_ms: number;
  };
  /// Mirrors `correction_engine::tokenizer::Token`. The L4 Observing brief,
  /// Component 1: shared boundary-of-truth tokenizer.
  type TokenKind = "word" | "number" | "url" | "email" | "code" | "acronym";
  type TokenPayload = {
    core: string;
    start: number;
    end: number;
    leading: string;
    trailing: string;
    terminator: string | null;
    kind: TokenKind;
    correctable: boolean;
    tokenizer_version: number;
  };
  /// Mirrors `correction_engine::anchor::SpanAnchor`. Component 2: span
  /// anchor tracking via edit deltas.
  type VoidReason = "split" | "merge" | "deleted";
  type AnchorState =
    | { kind: "tracking" }
    | { kind: "void"; reason: VoidReason };
  type SpanAnchorRow = {
    id: number;
    original_core: string;
    start: number;
    end: number;
    state: AnchorState;
  };
  type AnchorsSnapshot = {
    anchors: SpanAnchorRow[];
    void_count: number;
    /// Engine wraps the pure tracker snapshot with the current line buffer
    /// so the panel can show text-now-at-span without needing its own
    /// keystroke book-keeping.
    current_line: string;
  };
  /// Mirrors `LexiconPayload` in engine.rs. Emitted once per sealed Word
  /// token (Component 3a — read-only word source, no scoring yet).
  type LexiconPayload = {
    word: string;
    known: boolean;
    frequency: number;
    lexicon_version: number;
  };
  /// Mirrors `correction_engine::score::EditType`. Slip-perspective:
  /// the user added an extra key (insertion) or missed one (deletion).
  type EditType = "substitution" | "transposition" | "insertion" | "deletion";
  /// Mirrors `correction_engine::ConfidenceTier`.
  type ConfidenceTier = "gentle" | "balanced" | "bold";
  /// Mirrors `correction_engine::ScoredCandidate` — one row in the
  /// CANDIDATES panel under 3c-1 scoring.
  type ScoredCandidate = {
    word: string;
    frequency: number;
    edit_type: EditType;
    lexicon_evidence: number;
    motor_evidence: number;
    score: number;
  };
  /// Mirrors `CandidatesPayload` in engine.rs. Components 3b + 3c-1 —
  /// emitted once per UNKNOWN Word token. Empty `scored` is a real
  /// outcome ("no known candidates within edit-1"), not a missing event.
  /// `top_tier` is display-only this pass; the decision path is untouched.
  type CandidatesPayload = {
    word: string;
    scored: ScoredCandidate[];
    top_score: number | null;
    top_tier: ConfidenceTier | null;
    candidates_version: number;
    score_version: number;
  };
  type ModelSnapshot = {
    timing: { per_key: KeyTimingRow[]; per_finger: FingerTimingRow[] };
    asymmetry: AsymmetrySnapshot;
    ghost_keys: GhostKeysSnapshot;
    slips: SlipsSnapshot;
  };

  /// Cap on the per-key ghost list — keep the worst offenders visible,
  /// drop the long tail to keep the panel tidy.
  const GHOST_PER_KEY_LIMIT = 10;

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
      }
    | {
        id: number;
        kind: "slip";
        aimed_for: string;
        hit_instead: string;
        hand: Hand | null;
        finger: Finger | null;
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
  let asymmetry: AsymmetrySnapshot | null = null;
  let ghostKeys: GhostKeysSnapshot | null = null;
  let slips: SlipsSnapshot | null = null;
  /// Tokens sealed by the L4 tokenizer for the CURRENT line. Cleared by
  /// `engine://line-reset` events (newline, backspace rebuild, special key).
  let lineTokens: TokenPayload[] = [];
  /// Latest anchor snapshot from the engine. Replaced wholesale on each
  /// `engine://anchor-snapshot`. The panel doesn't infer anchor state from
  /// edits — engine is the source of truth.
  let anchorsSnap: AnchorsSnapshot | null = null;
  /// Lexicon lookups for words sealed on the CURRENT line. One row per
  /// Word token, appended in order. Cleared by `engine://line-reset`.
  let lineLexicon: LexiconPayload[] = [];
  /// Candidate sets for UNKNOWN Word tokens on the CURRENT line. One row
  /// per unknown word (known words never emit). Cleared by
  /// `engine://line-reset` alongside the other line-scoped state.
  let lineCandidates: CandidatesPayload[] = [];

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
        asymmetry = e.payload.asymmetry;
        ghostKeys = e.payload.ghost_keys;
        slips = e.payload.slips;
      }),
    );
    unlistens.push(
      await listen<SlipPayload>("engine://slip", (e) => {
        push({
          id: nextId++,
          kind: "slip",
          aimed_for: e.payload.aimed_for,
          hit_instead: e.payload.hit_instead,
          hand: e.payload.hand,
          finger: e.payload.finger,
        });
      }),
    );
    unlistens.push(
      await listen<TokenPayload>("engine://token", (e) => {
        lineTokens = [...lineTokens, e.payload];
      }),
    );
    unlistens.push(
      await listen("engine://line-reset", () => {
        lineTokens = [];
        lineLexicon = [];
        lineCandidates = [];
      }),
    );
    unlistens.push(
      await listen<AnchorsSnapshot>("engine://anchor-snapshot", (e) => {
        anchorsSnap = e.payload;
      }),
    );
    unlistens.push(
      await listen<LexiconPayload>("engine://lexicon", (e) => {
        lineLexicon = [...lineLexicon, e.payload];
      }),
    );
    unlistens.push(
      await listen<CandidatesPayload>("engine://candidates", (e) => {
        lineCandidates = [...lineCandidates, e.payload];
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
  function fmtKeyFinger(row: { hand: Hand | null; finger: Finger | null }): string {
    if (!row.hand || !row.finger) return "—";
    return fmtFinger(row.hand, row.finger);
  }
  /// Compose a token's raw form for display: leading + core + trailing,
  /// with core highlighted (the brief: span anchor tracks core only).
  function fmtTokenText(leading: string, core: string, trailing: string): string {
    return `${leading}${core}${trailing}`;
  }
  /// Char-indexed slice of `current_line` at `[start, end)` for an anchor.
  /// JS strings are UTF-16, so we go via Array.from to count chars properly.
  function sliceLine(line: string, start: number, end: number): string {
    const arr = Array.from(line);
    return arr.slice(start, end).join("");
  }
  /// Group separators for raw unigram counts so 23,135,851,162 is readable.
  function fmtFreq(n: number): string {
    if (n === 0) return "—";
    return n.toLocaleString("en-US");
  }
  /// Compact "23.1B" style for the CANDIDATES inline list — same numbers,
  /// just shorter so the per-word row fits e.g. `thge → the (23.1B), thee
  /// (8.6M), tage (695K)`.
  function fmtFreqCompact(n: number): string {
    if (n === 0) return "—";
    if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
    return String(n);
  }
  /// Two-decimal score formatter for the CANDIDATES table.
  function fmtScore(n: number | null): string {
    if (n === null) return "—";
    return n.toFixed(2);
  }
  function fmtAnchorState(state: AnchorState): string {
    if (state.kind === "tracking") return "Tracking";
    return `Void:${state.reason}`;
  }
  function fmtRatio(r: number): string {
    return `${r.toFixed(2)}×`;
  }
  /**
   * One-liner read of the asymmetry score. The aggregator now drives the
   * decision off **dwell** only — interval is shown for diagnostics but is
   * too easily contaminated by reading/thinking pauses to lead with, even
   * after gap filtering. So the readout always cites dwell.
   */
  function asymmetryReadout(a: AsymmetrySnapshot): string {
    if (a.left.count === 0 && a.right.count === 0) return "no keys observed yet";
    if (a.steadier_hand === null) {
      if (a.left.count === 0) return "only right hand observed so far";
      if (a.right.count === 0) return "only left hand observed so far";
      return "left and right have equal dwell";
    }
    const steadier = a.steadier_hand === "left" ? "Left" : "Right";
    return `${steadier} hand steadier — ${fmtRatio(a.overall_score)} on dwell`;
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
          {:else if row.kind === "slip"}
            <div class="row row-slip">
              <span class="tag">SLIP</span>
              <span class="badge badge-slip">CANDIDATE</span>
              <span class="col-word">aimed {fmtKey(row.aimed_for)} · hit {fmtKey(row.hit_instead)}</span>
              <span class="col-lat">{fmtKeyFinger(row)}</span>
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
        <div class="model-sub">ASYMMETRY · left vs right</div>
        <div class="asym-table">
          <div class="asym-row asym-head">
            <span class="col-ah">hand</span>
            <span class="col-an num">count</span>
            <span class="col-ad num">avg dwell</span>
            <span class="col-ai num">avg interval</span>
          </div>
          {#if asymmetry === null}
            <div class="empty">no data yet…</div>
          {:else}
            <div class="asym-row asym-left">
              <span class="col-ah">L</span>
              <span class="col-an num">{asymmetry.left.count}</span>
              <span class="col-ad num">{fmtMs(asymmetry.left.avg_dwell_ms)} ms</span>
              <span class="col-ai num">{fmtMs(asymmetry.left.avg_interval_ms)} ms</span>
            </div>
            <div class="asym-row asym-right">
              <span class="col-ah">R</span>
              <span class="col-an num">{asymmetry.right.count}</span>
              <span class="col-ad num">{fmtMs(asymmetry.right.avg_dwell_ms)} ms</span>
              <span class="col-ai num">{fmtMs(asymmetry.right.avg_interval_ms)} ms</span>
            </div>
            <div class="asym-score">
              <span class="asym-score-label">score</span>
              <span class="asym-score-val num">{fmtRatio(asymmetry.overall_score)}</span>
              <span class="asym-score-detail">
                (dwell {fmtRatio(asymmetry.dwell_ratio)} · interval {fmtRatio(asymmetry.interval_ratio)})
              </span>
            </div>
            <div class="asym-readout">{asymmetryReadout(asymmetry)}</div>
          {/if}
        </div>

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

      <!-- Scrolling: TOKENS (L4 Observing) → SLIPS → GHOST KEYS → PER KEY.
           Each section's sub-header is sticky so it stays visible as you
           scroll within the section. -->
      <div class="model-scroll">
        <div class="model-sub model-sub-sticky">TOKENS · current line · L4 boundary truth</div>
        <div class="token-block">
          {#if lineTokens.length === 0}
            <div class="empty">no tokens on this line yet…</div>
          {:else}
            <div class="token-table">
              <div class="token-row token-head">
                <span class="col-tc">core</span>
                <span class="col-tk">kind</span>
                <span class="col-tx">corr</span>
                <span class="col-ts num">[start,end)</span>
                <span class="col-tt">term</span>
              </div>
              {#each lineTokens as t, i (`${i}-${t.start}-${t.core}`)}
                <div class="token-row" class:token-correctable={t.correctable}>
                  <span class="col-tc">{fmtTokenText(t.leading, t.core, t.trailing)}</span>
                  <span class="col-tk">{t.kind}</span>
                  <span class="col-tx">{t.correctable ? "yes" : "—"}</span>
                  <span class="col-ts num">[{t.start},{t.end})</span>
                  <span class="col-tt">{t.terminator === null ? "—" : fmtKey(t.terminator)}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">LEXICON · current line · L4 word source</div>
        <div class="lex-block">
          {#if lineLexicon.length === 0}
            <div class="empty">no word lookups on this line yet…</div>
          {:else}
            <div class="lex-table">
              <div class="lex-row lex-head">
                <span class="col-lw">word</span>
                <span class="col-lk">known?</span>
                <span class="col-lf num">frequency</span>
              </div>
              {#each lineLexicon as l, i (`${i}-${l.word}`)}
                <div class="lex-row" class:lex-known={l.known} class:lex-unknown={!l.known}>
                  <span class="col-lw">{l.word}</span>
                  <span class="col-lk">{l.known ? "yes" : "no"}</span>
                  <!-- Frequency is a ranking-only signal under lexicon v2:
                       it's meaningless for unknown words (a Norvig typo
                       like `teh` has 1.7M occurrences but isn't a real
                       word). Suppress to "—" when known=NO so the panel
                       doesn't suggest the count is load-bearing. -->
                  <span class="col-lf num">{l.known ? fmtFreq(l.frequency) : "—"}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">CANDIDATES · unknown words · L4 score (display-only)</div>
        <div class="cand-block">
          {#if lineCandidates.length === 0}
            <div class="empty">no unknown words on this line yet…</div>
          {:else}
            {#each lineCandidates as c, i (`${i}-${c.word}`)}
              <div class="cand-group">
                <div class="cand-header">
                  <span class="cand-src">{c.word}</span>
                  <span class="cand-arrow">→</span>
                  {#if c.scored.length === 0}
                    <span class="cand-empty">(no known candidates within edit-1)</span>
                  {:else if c.top_tier !== null}
                    <span class="cand-tier cand-tier-{c.top_tier}">{c.top_tier}</span>
                    <span class="cand-top-score">top {fmtScore(c.top_score)}</span>
                  {:else}
                    <span class="cand-tier cand-tier-none">below tier floor</span>
                    <span class="cand-top-score">top {fmtScore(c.top_score)}</span>
                  {/if}
                </div>
                {#if c.scored.length > 0}
                  <div class="score-table">
                    <div class="score-row score-head">
                      <span class="col-sw">candidate</span>
                      <span class="col-se">edit</span>
                      <span class="col-sx num">freq</span>
                      <span class="col-sl num">lex_ev</span>
                      <span class="col-sm num">motor_ev</span>
                      <span class="col-ss num">score</span>
                    </div>
                    {#each c.scored as s, j (s.word)}
                      <div class="score-row" class:score-top={j === 0}>
                        <span class="col-sw">{s.word}</span>
                        <span class="col-se">{s.edit_type}</span>
                        <span class="col-sx num">{fmtFreqCompact(s.frequency)}</span>
                        <span class="col-sl num">{fmtScore(s.lexicon_evidence)}</span>
                        <span class="col-sm num">{fmtScore(s.motor_evidence)}</span>
                        <span class="col-ss num">{fmtScore(s.score)}</span>
                      </div>
                    {/each}
                  </div>
                {/if}
              </div>
            {/each}
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">ANCHORS · L4 spans · edit-delta tracked</div>
        <div class="anchor-block">
          {#if anchorsSnap === null || anchorsSnap.anchors.length === 0}
            <div class="empty">no anchors registered yet…</div>
          {:else}
            <div class="anchor-status">
              <span class="num">{anchorsSnap.anchors.length}</span> total ·
              <span class="num anchor-void-count">{anchorsSnap.void_count}</span> void
            </div>
            <div class="anchor-table">
              <div class="anchor-row anchor-head">
                <span class="col-ao">original</span>
                <span class="col-as num">[start,end)</span>
                <span class="col-an">text-now</span>
                <span class="col-ast">state</span>
              </div>
              {#each anchorsSnap.anchors as a (a.id)}
                {@const voided = a.state.kind === "void"}
                <div class="anchor-row" class:anchor-voided={voided}>
                  <span class="col-ao">{a.original_core}</span>
                  <span class="col-as num">[{a.start},{a.end})</span>
                  <span class="col-an">
                    {voided ? "—" : sliceLine(anchorsSnap.current_line, a.start, a.end) || "—"}
                  </span>
                  <span class="col-ast">{fmtAnchorState(a.state)}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">SLIPS · strict-filtered candidates · writes to L3 map</div>
        <div class="slip-block">
          {#if slips === null}
            <div class="empty">no data yet…</div>
          {:else}
            <div class="slip-status">
              total <span class="num slip-total">{slips.total_slips}</span>
              · L3 map: <span class="num">{slips.map_swap_pairs}</span> swap pairs,
              <span class="num">{slips.map_key_confidence}</span> key entries
            </div>
            {#if slips.per_pair.length === 0}
              <div class="empty">no slips confirmed yet…</div>
            {:else}
              <div class="slip-table">
                <div class="slip-row slip-head">
                  <span class="col-sa">aimed</span>
                  <span class="col-sh">hit</span>
                  <span class="col-sf">finger</span>
                  <span class="col-sn num">count</span>
                </div>
                {#each slips.per_pair as p (`${p.aimed_for}-${p.hit_instead}`)}
                  <div class="slip-row">
                    <span class="col-sa">{fmtKey(p.aimed_for)}</span>
                    <span class="col-sh">{fmtKey(p.hit_instead)}</span>
                    <span class="col-sf">{fmtKeyFinger(p)}</span>
                    <span class="col-sn num">{p.count}</span>
                  </div>
                {/each}
              </div>
            {/if}
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">GHOST KEYS · likely, not certain</div>
        <div class="ghost-block">
          {#if ghostKeys === null}
            <div class="empty">no data yet…</div>
          {:else}
            <div class="ghost-status">
              {#if ghostKeys.warmup_remaining > 0}
                calibrating — {ghostKeys.warmup_remaining} more keystrokes before short-dwell flagging
              {:else}
                flagging dwell &lt; <span class="num">{fmtMs(ghostKeys.dwell_threshold_ms)}</span> ms
              {/if}
            </div>
            <div class="ghost-totals">
              <span class="ghost-totals-label">total</span>
              <span class="num ghost-totals-val">{ghostKeys.total_ghost_events}</span>
              <span class="ghost-totals-detail">
                short-dwell <span class="num">{ghostKeys.short_dwell_count}</span>
                · rapid-repeat <span class="num">{ghostKeys.rapid_repeat_count}</span>
                · self-corrected <span class="num">{ghostKeys.self_corrected_count}</span>
              </span>
            </div>

            <div class="ghost-sub">per hand · signature breakdown</div>
            <div class="ghost-hand-table">
              <div class="ghost-hand-row ghost-hand-head">
                <span class="col-ghh">hand</span>
                <span class="col-ghe num">events</span>
                <span class="col-ghs num">short</span>
                <span class="col-ghr num">repeat</span>
                <span class="col-ghc num">self</span>
              </div>
              {#each ghostKeys.per_hand as h (h.hand)}
                <div
                  class="ghost-hand-row"
                  class:finger-left={h.hand === "left"}
                  class:finger-right={h.hand === "right"}
                >
                  <span class="col-ghh">{h.hand === "left" ? "L" : "R"}</span>
                  <span class="col-ghe num">{h.events}</span>
                  <span class="col-ghs num">{h.short_dwell}</span>
                  <span class="col-ghr num">{h.rapid_repeat}</span>
                  <span class="col-ghc num">{h.self_corrected}</span>
                </div>
              {/each}
            </div>

            <div class="ghost-sub">per finger · anatomical</div>
            {#if ghostKeys.per_finger.length === 0}
              <div class="empty">none yet…</div>
            {:else}
              <div class="ghost-finger-table">
                {#each ghostKeys.per_finger as f (`${f.hand}-${f.finger}`)}
                  <div
                    class="ghost-finger-row"
                    class:finger-left={f.hand === "left"}
                    class:finger-right={f.hand === "right"}
                  >
                    <span class="col-gff">{fmtFinger(f.hand, f.finger)}</span>
                    <span class="col-gfn num">{f.events}</span>
                  </div>
                {/each}
              </div>
            {/if}

            <div class="ghost-sub">per key · worst first</div>
            {#if ghostKeys.per_key.length === 0}
              <div class="empty">none yet…</div>
            {:else}
              <div class="ghost-key-table">
                <div class="ghost-key-row ghost-key-head">
                  <span class="col-gk">key</span>
                  <span class="col-gf">finger</span>
                  <span class="col-ge num">events</span>
                  <span class="col-gs num">short</span>
                  <span class="col-gr num">repeat</span>
                  <span class="col-gc num">self</span>
                </div>
                {#each ghostKeys.per_key.slice(0, GHOST_PER_KEY_LIMIT) as r (r.key)}
                  <div class="ghost-key-row">
                    <span class="col-gk">{fmtKey(r.key)}</span>
                    <span class="col-gf">{fmtKeyFinger(r)}</span>
                    <span class="col-ge num">{r.events}</span>
                    <span class="col-gs num">{r.short_dwell}</span>
                    <span class="col-gr num">{r.rapid_repeat}</span>
                    <span class="col-gc num">{r.self_corrected}</span>
                  </div>
                {/each}
                {#if ghostKeys.per_key.length > GHOST_PER_KEY_LIMIT}
                  <div class="ghost-more">
                    + {ghostKeys.per_key.length - GHOST_PER_KEY_LIMIT} more keys with ghost activity
                  </div>
                {/if}
              </div>
            {/if}
          {/if}
        </div>

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
  /* Slip candidate — distinct amber so it stands apart from CORRECTED green
     and LEFT-ALONE grey, but text label still carries the meaning. */
  .badge-slip {
    background: #3a2f1a;
    color: #e6c98a;
    border: 1px solid #6a5320;
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
  /* Same trick for SLIP rows — amber rail mirrors the badge colour but the
     text tag is still load-bearing. */
  .row-slip {
    grid-template-columns: 70px auto minmax(0, 1fr) auto;
    border-left: 2px solid #6a5320;
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

  /* Asymmetry block — left vs right rollup + single score + plain-language read.
     Sits above PER FINGER inside .model-pinned (always visible). */
  .asym-table {
    padding: 0 0 0.4rem;
    border-bottom: 1px solid #2a2f36;
  }
  .asym-row {
    display: grid;
    grid-template-columns: 32px 60px minmax(0, 1fr) minmax(0, 1fr);
    column-gap: 0.6rem;
    align-items: baseline;
    padding: 0.22rem 0.75rem;
    white-space: nowrap;
  }
  .asym-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.3rem;
    margin-bottom: 0.15rem;
  }
  /* Same hand tints as PER FINGER so left/right map visually between the
     two tables. Letter labels ("L"/"R") still carry the meaning so it's
     never colour-only. */
  .asym-left:not(.asym-head) { background: rgba(120, 160, 220, 0.045); }
  .asym-right:not(.asym-head) { background: rgba(220, 160, 120, 0.045); }
  .col-ah { color: #e6e6e6; font-weight: 600; }
  .col-an { color: #d5d5d5; }
  .col-ad, .col-ai { color: #8aa1b8; }

  .asym-score {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    padding: 0.35rem 0.75rem 0.1rem;
    border-top: 1px dashed #2a2f36;
    margin-top: 0.1rem;
  }
  .asym-score-label {
    color: #7f8a96;
    letter-spacing: 0.08em;
  }
  .asym-score-val {
    color: #e6e6e6;
    font-weight: 600;
  }
  .asym-score-detail {
    color: #6a747f;
    font-size: 11px;
  }
  .asym-readout {
    color: #cdd5de;
    padding: 0.1rem 0.75rem 0.3rem;
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

  /* TOKENS — L4 Observing brief, Component 1. Live tokens for the current
     line; cleared on engine://line-reset. */
  .token-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .token-table {
    padding: 0 0 0.2rem;
  }
  .token-row {
    display: grid;
    grid-template-columns: minmax(0, 1.4fr) 64px 40px 70px 36px;
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .token-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .token-row:not(.token-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  /* Correctable tokens get a subtle green rail — text label "yes" still
     carries the meaning so this is supplementary, not color-only. */
  .token-correctable {
    border-left: 2px solid #2e5a2e;
    padding-left: calc(0.75rem - 2px);
  }
  .col-tc {
    color: #e6e6e6;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-tk { color: #8aa1b8; }
  .col-tx { color: #d5d5d5; }
  .col-ts { color: #8aa1b8; }
  .col-tt { color: #d5d5d5; }

  /* LEXICON — L4 Component 3a, read-only word source. One row per sealed
     Word token on the current line. "known" gets a quiet green rail; "no"
     gets an amber rail — text labels still carry the meaning, the rails
     are just glanceability. */
  .lex-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .lex-table {
    padding: 0 0 0.2rem;
  }
  .lex-row {
    display: grid;
    grid-template-columns: minmax(0, 1.2fr) 60px minmax(0, 1fr);
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .lex-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .lex-row:not(.lex-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .lex-known {
    border-left: 2px solid #2e5a2e;
    padding-left: calc(0.75rem - 2px);
  }
  .lex-unknown {
    border-left: 2px solid #6a5320;
    padding-left: calc(0.75rem - 2px);
  }
  .col-lw {
    color: #e6e6e6;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-lk { color: #d5d5d5; }
  .col-lf { color: #8aa1b8; }

  /* CANDIDATES — L4 Components 3b + 3c-1. Per unknown word: a header
     line (typed word + tier badge + top score) and a small table with
     per-candidate edit type, frequency, lexicon evidence, motor evidence,
     and score. Tier badge is display-only this pass; the decision path
     is unchanged. */
  .cand-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .cand-group {
    padding: 0.3rem 0.75rem 0.45rem;
  }
  .cand-group:nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .cand-header {
    display: flex;
    align-items: baseline;
    gap: 0.45rem;
    margin-bottom: 0.2rem;
  }
  .cand-src {
    color: #e6c98a;
    font-weight: 600;
  }
  .cand-arrow {
    color: #7f8a96;
  }
  .cand-empty {
    color: #7f8a96;
    font-style: italic;
  }
  .cand-top-score {
    color: #8aa1b8;
    margin-left: auto;
    font-variant-numeric: tabular-nums;
  }
  /* Tier badges — colour-coded but the text label carries the meaning. */
  .cand-tier {
    padding: 0 0.45rem;
    border-radius: 3px;
    letter-spacing: 0.07em;
    font-weight: 600;
    text-transform: uppercase;
    font-size: 11px;
  }
  .cand-tier-gentle {
    background: #1f2a3a;
    color: #9bb4d6;
    border: 1px solid #2e4a6a;
  }
  .cand-tier-balanced {
    background: #1f3a1f;
    color: #b6e3b6;
    border: 1px solid #2e5a2e;
  }
  .cand-tier-bold {
    background: #3a2a1f;
    color: #e6c98a;
    border: 1px solid #6a5320;
  }
  .cand-tier-none {
    background: #1f1f1f;
    color: #7f8a96;
    border: 1px solid #3a3a3a;
    font-style: italic;
  }
  /* Per-candidate score table — fixed columns, tabular numerals so the
     numeric stacks line up. */
  .score-table {
    margin-top: 0.15rem;
  }
  .score-row {
    display: grid;
    grid-template-columns: minmax(0, 1.2fr) 88px 56px 56px 60px 56px;
    column-gap: 0.4rem;
    align-items: baseline;
    padding: 0.18rem 0.3rem;
    white-space: nowrap;
  }
  .score-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.2rem;
    margin-bottom: 0.1rem;
    font-size: 11px;
    letter-spacing: 0.06em;
  }
  /* Top candidate gets a subtle green rail — supplementary; the row
     position (first) and the tier badge above carry the meaning. */
  .score-top {
    border-left: 2px solid #2e5a2e;
    padding-left: calc(0.3rem - 2px);
    background: rgba(46, 90, 46, 0.08);
  }
  .col-sw { color: #b6e3b6; font-weight: 600; overflow: hidden; text-overflow: ellipsis; }
  .col-se { color: #8aa1b8; }
  .col-sx { color: #8aa1b8; }
  .col-sl, .col-sm { color: #d5d5d5; }
  .col-ss { color: #e6e6e6; font-weight: 600; }

  /* ANCHORS — L4 Observing, Component 2. Sibling of TOKENS; one row per
     live anchor on the current line. Voided rows get a faded look but
     stay in the list (the state label carries the meaning). */
  .anchor-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .anchor-status {
    color: #8893a0;
    padding: 0.15rem 0.75rem 0.3rem;
    font-size: 11px;
  }
  .anchor-void-count {
    color: #e6c98a;
    font-weight: 600;
  }
  .anchor-table {
    padding: 0 0 0.2rem;
  }
  .anchor-row {
    display: grid;
    grid-template-columns: minmax(0, 1.1fr) 70px minmax(0, 1.1fr) 96px;
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .anchor-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .anchor-row:not(.anchor-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .anchor-voided {
    color: #7f8a96;
  }
  .anchor-voided .col-ao,
  .anchor-voided .col-an,
  .anchor-voided .col-as {
    text-decoration: line-through;
    text-decoration-thickness: 1px;
  }
  .col-ao {
    color: #e6e6e6;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-as { color: #8aa1b8; }
  .col-an {
    color: #d5d5d5;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-ast {
    color: #d5d5d5;
    font-weight: 600;
  }
  .anchor-voided .col-ast { color: #e6c98a; }

  /* SLIPS — first L3 learning loop. Status + per-pair table.
     Sits at the very top of .model-scroll. */
  .slip-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .slip-status {
    color: #8893a0;
    padding: 0.15rem 0.75rem 0.4rem;
    font-size: 11px;
  }
  .slip-total {
    color: #e6c98a;
    font-weight: 600;
  }
  .slip-table {
    padding: 0 0 0.2rem;
  }
  .slip-row {
    display: grid;
    grid-template-columns: 44px 44px minmax(0, 1fr) 56px;
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .slip-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .slip-row:not(.slip-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .col-sa { color: #e6c98a; font-weight: 600; }
  .col-sh { color: #d5d5d5; }
  .col-sf { color: #8aa1b8; }
  .col-sn { color: #e6e6e6; font-weight: 600; }

  /* GHOST KEYS — totals header + per-finger rollup + per-key worst-first.
     Sits at the top of .model-scroll, above PER KEY. */
  .ghost-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .ghost-status {
    color: #8893a0;
    padding: 0.15rem 0.75rem 0.3rem;
    font-size: 11px;
  }
  .ghost-totals {
    display: grid;
    grid-template-columns: auto auto 1fr;
    column-gap: 0.6rem;
    align-items: baseline;
    padding: 0.1rem 0.75rem 0.4rem;
  }
  .ghost-totals-label {
    color: #7f8a96;
    letter-spacing: 0.08em;
  }
  .ghost-totals-val {
    color: #e6e6e6;
    font-weight: 600;
  }
  .ghost-totals-detail {
    color: #8aa1b8;
    font-size: 11px;
  }
  .ghost-sub {
    padding: 0.4rem 0.75rem 0.1rem;
    color: #6a747f;
    letter-spacing: 0.08em;
    font-size: 11px;
  }
  /* Per-hand × per-signature ghost breakdown. Two rows (L, R) reusing the
     same hand tints as PER FINGER / ASYMMETRY for visual continuity. */
  .ghost-hand-table {
    padding: 0 0 0.2rem;
  }
  .ghost-hand-row {
    display: grid;
    grid-template-columns: 32px 56px 48px 56px 44px;
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.18rem 0.75rem;
    white-space: nowrap;
  }
  .ghost-hand-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .col-ghh { color: #e6e6e6; font-weight: 600; }
  .col-ghe { color: #e6e6e6; font-weight: 600; }
  .col-ghs, .col-ghr, .col-ghc { color: #8aa1b8; }

  /* Per-finger ghost mini-table (one number per finger). */
  .ghost-finger-table {
    padding: 0 0 0.2rem;
  }
  .ghost-finger-row {
    display: grid;
    grid-template-columns: 90px minmax(0, 1fr);
    column-gap: 0.6rem;
    align-items: baseline;
    padding: 0.18rem 0.75rem;
    white-space: nowrap;
  }
  .col-gff { color: #e6e6e6; font-weight: 600; }
  .col-gfn { color: #d5d5d5; }
  /* Per-key ghost table (worst offenders). */
  .ghost-key-table {
    padding: 0 0 0.2rem;
  }
  .ghost-key-row {
    display: grid;
    grid-template-columns: 36px 80px 56px 44px 48px 44px;
    column-gap: 0.4rem;
    align-items: baseline;
    padding: 0.18rem 0.75rem;
    white-space: nowrap;
  }
  .ghost-key-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .ghost-key-row:not(.ghost-key-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .col-gk { color: #e6e6e6; font-weight: 600; }
  .col-gf { color: #8aa1b8; }
  .col-ge { color: #e6e6e6; font-weight: 600; }
  .col-gs, .col-gr, .col-gc { color: #8aa1b8; }
  .ghost-more {
    color: #6a747f;
    font-style: italic;
    padding: 0.25rem 0.75rem;
  }

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
