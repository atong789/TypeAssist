<!-- Progress — ONE integrated screen (main-window tab): the per-key keyboard map
     shows WHERE you slip, and the detail panel below shows the WORDS behind the
     shading. The map reuses read_key_scores (per-key precision / coordination
     rates, well_sampled); the panel reads read_word_patterns (the corrections,
     each tagged coord/precis, with the soft-blue corrected letter and the keys
     it involves). Selecting a key links the two: it highlights AND filters the
     panel to corrections touching that key; "← Show all corrections" clears it.

     Heat = the blue accent, deeper = more slips for the active lens (never red),
     scaled relative to the user's own worst live key. State-aware (shared
     appState): Day one = all faint/dashed + lens toggle disabled; Building /
     Fluent = REAL coverage — sampled keys shaded, under-sampled keys stay faint
     (we never force a key to "seen"). Observe-only; reads nothing it shouldn't.
     Colours: app blue + soft blue (#7fb6ee) only — never green or red. -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { appState } from "../lib/previewSettings";
  import CorrectionPair from "../lib/CorrectionPair.svelte";

  type Lens = "precision" | "coordination";

  // RAW decayed rates in 0..1 (engine-side), per the live ProgressPanel.
  interface KeyScore {
    key: string;
    precision: number;
    coordination: number;
    productions: number;
    well_sampled: boolean;
  }

  // One learned correction for the detail panel. `obs` is the DECAYED weight
  // (what the engine actually sees); `class` groups it (coord/precis); `highlight`
  // are the target char indices the fix changed (soft-blue mark); `keys` are the
  // keyboard keys the correction involves (coordination: the transposed pair;
  // precision: the wrong + intended key). All derived on read, never persisted.
  interface ImpactPattern {
    typed: string;
    target: string;
    obs: number;
    ready: boolean;
    class: string | null;
    highlight: number[];
    keys: string[];
  }

  let keyScores: KeyScore[] = [];
  let patterns: ImpactPattern[] = [];
  let lens: Lens = "precision";
  // The selected key links the map and the panel: null → the panel shows the
  // FULL list; a key → that key highlights and the panel filters to the
  // corrections that involve it. Selection follows focus (arrow-navigation) and
  // click; the panel's "← Show all corrections" clears it.
  let selectedKey: string | null = null;

  // The command returns patterns sorted by obs desc, so the list stays ranked by
  // count. With a key selected, scope to the corrections that touch it.
  const scopeToKey = (list: ImpactPattern[], key: string | null) =>
    key ? list.filter((p) => p.keys.includes(key)) : list;
  $: scopedPatterns = scopeToKey(patterns, selectedKey);
  // The lens drives the LIST too, not just the map shading: show ONLY the active
  // group (never both stacked). Precision → precision corrections; Coordination →
  // coordination corrections. Applies to the full list and the per-key drill-down.
  $: activeClass = lens === "precision" ? "precis" : "coord";
  $: activeName = lens === "precision" ? "Precision" : "Coordination";
  $: activeCap = lens === "precision" ? "right key, clean hit" : "right keys, right order";
  $: activeRows = scopedPatterns.filter((p) => p.class === activeClass);
  $: hasShown = activeRows.length > 0;
  const obsCount = (o: number) => Math.round(o);

  const SPACE = "space";
  // The board: three letter rows + a real space-bar row.
  const ROWS: string[][] = [
    "qwertyuiop".split(""),
    "asdfghjkl".split(""),
    "zxcvbnm".split(""),
    [SPACE],
  ];

  // Space follows the SAME rules as every key: real read_key_scores data only.
  // read_key_scores has no space entry, so it is simply "not sampled" — it
  // renders faint/dashed like any key without confident data (and faint in Day
  // one). No synthesized value.
  $: scoreByKey = new Map(keyScores.map((k) => [k.key, k]));

  // Roving tabindex — the keyboard is ONE Tab stop (like the main nav). Exactly
  // one key has tabindex 0 (the roving key), the rest tabindex -1. Arrows move
  // focus between keys; focusing a key updates the detail line, just like
  // tapping. Tab reaches the board once, Tab again exits.
  let rovingKey = "q";
  let keyEls: Record<string, HTMLButtonElement> = {};

  function onKeyKeydown(event: KeyboardEvent, r: number, c: number) {
    let nr = r;
    let nc = c;
    switch (event.key) {
      case "ArrowLeft":
        nc = Math.max(0, c - 1);
        break;
      case "ArrowRight":
        nc = Math.min(ROWS[r].length - 1, c + 1);
        break;
      case "ArrowUp":
        if (r === 0) return;
        nr = r - 1;
        nc = Math.min(c, ROWS[nr].length - 1);
        break;
      case "ArrowDown":
        if (r === ROWS.length - 1) return;
        nr = r + 1;
        nc = Math.min(c, ROWS[nr].length - 1);
        break;
      default:
        return;
    }
    event.preventDefault();
    keyEls[ROWS[nr][nc]]?.focus();
  }

  const rawOf = (k: KeyScore | undefined, l: Lens) =>
    !k ? 0 : l === "precision" ? k.precision : k.coordination;

  // Day one shows no heat at all; Building / Fluent both show real coverage.
  $: dataLive = $appState !== "day1";
  function isLive(key: string): boolean {
    if (!dataLive) return false;
    const k = scoreByKey.get(key);
    return !!(k && k.well_sampled);
  }
  // Relative scaling — the brightest live key for the active lens tops the ramp.
  $: maxRaw = dataLive
    ? Math.max(
        0,
        ...[...scoreByKey.values()].filter((k) => k.well_sampled).map((k) => rawOf(k, lens)),
      )
    : 0;

  // Quiet → bright blue, relative to the user's own worst key; a key with too
  // little data is painted neutral, never falsely "good".
  function keyStyle(key: string): string {
    if (!isLive(key)) {
      return "background: color-mix(in srgb, canvastext 5%, canvas); color: var(--text-secondary);";
    }
    const i = maxRaw > 0 ? Math.min(1, rawOf(scoreByKey.get(key), lens) / maxRaw) : 0;
    const mix = 4 + i * 80; // 4%..84% of the blue accent over the background
    return `background: color-mix(in srgb, var(--focus-ring) ${mix.toFixed(0)}%, canvas); color: canvastext;`;
  }

  const labelOf = (key: string) => (key === SPACE ? "Space bar" : key);

  // Per-key detail wording — verbatim shape from the design's Progress text.
  function keyDetail(key: string): string {
    const label = labelOf(key);
    if (!isLive(key)) return `${label} — not enough yet. Keep typing and this fills in.`;
    const k = scoreByKey.get(key)!;
    if (lens === "precision") {
      const v = k.precision * 100;
      return v < 0.15
        ? `${label} — rock steady. Barely ever mistyped.`
        : `${label} — slips on about ${v.toFixed(1)}% of presses.`;
    }
    const c = k.coordination * 100;
    return c < 0.05
      ? `${label} — rarely swapped with its neighbours.`
      : `${label} — turns up in about ${c.toFixed(2)}% of letter-swaps.`;
  }

  const keyLabel = (key: string) => (key === SPACE ? "space" : key);

  $: caption =
    $appState === "day1"
      ? "I haven’t seen enough yet to show your pattern. Keep typing — this fills in on its own."
      : $appState === "building"
        ? "Filling in. The faint, dashed keys are ones I haven’t seen enough of yet — not “clean”, just unknown."
        : "Your full picture. Tap a key, or use the arrow keys, to see the corrections behind it.";

  $: lensDesc =
    lens === "precision"
      ? "How often each key gets mistyped."
      : "Keys that get swapped with the ones next to them.";

  // Selecting a key links the map to the panel. Selection follows FOCUS (so
  // arrow-navigation reveals each key's corrections as you go) and click; the
  // panel's "← Show all corrections" is the way back to the full list.
  function selectKey(key: string) {
    rovingKey = key; // the tab stop follows the selection, too
    selectedKey = key;
    revealResults();
  }
  // Focus-driven selection, with one guarded exception: after "Show all" we
  // return focus to the board (so the ring never drops to <body>) WITHOUT
  // re-selecting — otherwise the clear would instantly re-filter the panel.
  let suppressFocusSelect = false;
  function onKeyFocus(key: string) {
    rovingKey = key;
    if (suppressFocusSelect) {
      suppressFocusSelect = false;
      return;
    }
    selectedKey = key;
    revealResults();
  }
  function clearSelectionAndRefocus() {
    selectedKey = null;
    suppressFocusSelect = true;
    tick().then(() => keyEls[rovingKey]?.focus({ preventScroll: true }));
  }

  // QoL: when a key is selected, bring its results into view so the user rarely
  // scrolls manually. `block: "nearest"` is a no-op once they're already
  // visible, so arrowing key-to-key doesn't jump the panel around.
  let impactEl: HTMLElement;
  function revealResults() {
    tick().then(() => {
      const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      impactEl?.scrollIntoView({ behavior: reduce ? "auto" : "smooth", block: "nearest" });
    });
  }

  // Lens toggle keyboard model (app-wide convention): one Tab stop via roving
  // tabindex; Left/Right (and Home/End) switch WITHIN the control. Tab moves
  // BETWEEN controls (to the keyboard map), never lapping through this toggle.
  const LENSES: Lens[] = ["precision", "coordination"];
  let lensEls: Partial<Record<Lens, HTMLButtonElement>> = {};
  function onLensKeydown(event: KeyboardEvent) {
    const i = LENSES.indexOf(lens);
    let ni: number;
    switch (event.key) {
      case "ArrowRight":
        ni = (i + 1) % LENSES.length;
        break;
      case "ArrowLeft":
        ni = (i - 1 + LENSES.length) % LENSES.length;
        break;
      case "Home":
        ni = 0;
        break;
      case "End":
        ni = LENSES.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    lens = LENSES[ni];
    lensEls[lens]?.focus();
  }

  onMount(() => {
    invoke<KeyScore[]>("read_key_scores")
      .then((k) => (keyScores = k ?? []))
      .catch(() => {
        /* read-only; keep the last-known board rather than blanking */
      });
    invoke<ImpactPattern[]>("read_word_patterns")
      .then((p) => (patterns = p ?? []))
      .catch(() => {
        /* read-only; keep the last-known list rather than blanking */
      });
  });
</script>

<header class="screen-header"><h1>Progress</h1></header>

<!-- Lens toggle — ONE Tab stop (roving tabindex); Left/Right switch within it. -->
<div class="lens">
  <div class="lens-toggle" role="radiogroup" aria-label="Score lens">
    <button
      class="lens-btn"
      class:on={lens === "precision"}
      role="radio"
      aria-checked={lens === "precision"}
      tabindex={lens === "precision" ? 0 : -1}
      disabled={$appState === "day1"}
      bind:this={lensEls.precision}
      on:click={() => (lens = "precision")}
      on:keydown={onLensKeydown}>Precision</button
    >
    <button
      class="lens-btn"
      class:on={lens === "coordination"}
      role="radio"
      aria-checked={lens === "coordination"}
      tabindex={lens === "coordination" ? 0 : -1}
      disabled={$appState === "day1"}
      bind:this={lensEls.coordination}
      on:click={() => (lens = "coordination")}
      on:keydown={onLensKeydown}>Coordination</button
    >
  </div>
  <p class="lens-desc">{$appState === "day1" ? " " : lensDesc}</p>
</div>

<!-- Keyboard block — centred, with the caption beneath it. -->
<div class="kb">
  <div class="kb-board" role="group" aria-label="{lens === 'precision' ? 'Precision' : 'Coordination'} by key">
    {#each ROWS as row, r}
      <div class="kb-row">
        {#each row as key, c}
          <button
            class="kb-key"
            class:space={key === SPACE}
            class:muted={!isLive(key)}
            class:selected={selectedKey === key}
            style={keyStyle(key)}
            tabindex={rovingKey === key ? 0 : -1}
            disabled={$appState === "day1"}
            aria-label={keyDetail(key)}
            aria-pressed={selectedKey === key}
            bind:this={keyEls[key]}
            on:click={() => selectKey(key)}
            on:keydown={(e) => onKeyKeydown(e, r, c)}
            on:focus={() => onKeyFocus(key)}>{key === SPACE ? "space" : key}</button
          >
        {/each}
      </div>
    {/each}
  </div>

  <p class="kb-caption">{caption}</p>
</div>

<!-- Corrections detail panel — linked to the map. Default (no key selected):
     the FULL list, ranked by count. A key selected: filtered to the corrections
     that involve it, under a "The X key" header with a way back. Read-only: no
     pills, no thresholds; every row is a fix the user already made. -->
<section class="impact" aria-label="Corrections behind your slip rate" bind:this={impactEl}>
  {#if selectedKey}
    <div class="impact-head">
      <h2 class="impact-title">The {keyLabel(selectedKey)} key</h2>
      <button class="impact-clear" on:click={clearSelectionAndRefocus}>
        ← Show all corrections
      </button>
    </div>
  {:else}
    <h2 class="impact-title">The corrections behind your slip rate</h2>
    <p class="impact-sub">
      Each one is a fix you made as you typed. Noticing them is how your keyboard map and
      percentages take shape.
    </p>
  {/if}

  {#if !hasShown}
    <p class="impact-empty">
      {#if selectedKey}
        No {activeName} corrections recorded for the {keyLabel(selectedKey)} key yet.
      {:else}
        No {activeName} corrections yet — they’ll gather here as you type.
      {/if}
    </p>
  {:else}
    <!-- Only the active lens's group — never both stacked. -->
    <div class="grp">
      <div class="grp-head">
        <span class="grp-name">{activeName}</span>
        <span class="grp-cap">{activeCap}</span>
      </div>
      <ul class="rows">
        {#each activeRows as p}
          <li class="crow">
            <CorrectionPair typed={p.typed} target={p.target} highlight={p.highlight} />
            <span class="count">{obsCount(p.obs)}×</span>
          </li>
        {/each}
      </ul>
    </div>
  {/if}
</section>

<style>
  /* ---- Impact detail panel — linked to the map above ---- */
  .impact {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    max-width: 34rem;
    margin: 1.6rem auto 0;
    width: 100%;
  }
  /* "The X key" header + the way back to the full list. */
  .impact-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.75rem;
    flex-wrap: wrap;
  }
  .impact-clear {
    flex-shrink: 0;
    padding: 0.2rem 0.1rem;
    font: inherit;
    font-size: 0.88rem;
    font-weight: 500;
    color: var(--focus-ring);
    background: transparent;
    border: none;
    cursor: pointer;
  }
  .impact-clear:hover {
    text-decoration: underline;
  }
  .impact-clear:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
    border-radius: 5px;
  }
  .impact-title {
    margin: 0;
    font-size: 1.05rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .impact-sub {
    margin: 0 0 0.6rem;
    font-size: 0.9rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .impact-empty {
    margin: 0.4rem 0 0;
    font-size: 0.92rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .grp {
    margin-top: 0.9rem;
  }
  .grp-head {
    display: flex;
    align-items: baseline;
    gap: 0.55rem;
    padding-bottom: 0.35rem;
    border-bottom: 1px solid var(--hairline);
  }
  .grp-name {
    font-size: 0.95rem;
    font-weight: 600;
  }
  .grp-cap {
    font-size: 0.82rem;
    color: var(--text-secondary);
  }
  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .crow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.5rem 0;
    border-bottom: 1px solid color-mix(in srgb, var(--hairline) 60%, transparent);
  }
  .count {
    flex-shrink: 0;
    font-size: 0.86rem;
    font-variant-numeric: tabular-nums;
    color: var(--text-secondary);
  }

  /* ---- lens toggle (left-aligned) ---- */
  .lens {
    margin: 0.5rem 0 1.6rem;
  }
  .lens-toggle {
    display: inline-flex;
    gap: 0.3rem;
    padding: 0.25rem;
    border: 1px solid var(--hairline);
    border-radius: 10px;
  }
  .lens-btn {
    min-height: 36px;
    padding: 0.4rem 0.95rem;
    font: inherit;
    font-size: 0.9rem;
    font-weight: 600;
    color: var(--text-secondary);
    background: transparent;
    border: none;
    border-radius: 7px;
    cursor: pointer;
  }
  .lens-btn.on {
    background: color-mix(in srgb, var(--focus-ring) 16%, canvas);
    color: canvastext;
  }
  .lens-btn:hover:not(.on):not(:disabled) {
    background: color-mix(in srgb, canvastext 5%, canvas);
  }
  .lens-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .lens-btn:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .lens-desc {
    margin: 0.6rem 0 0;
    font-size: 0.88rem;
    color: var(--text-secondary);
  }

  /* ---- keyboard block (centred) ---- */
  .kb {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 1rem;
  }
  .kb-board {
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .kb-row {
    display: flex;
    gap: 7px;
    justify-content: center;
  }
  .kb-key {
    width: 40px;
    height: 40px;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    font: inherit;
    font-size: 0.95rem;
    font-weight: 600;
    /* background + colour come from the inline keyStyle() ramp */
    cursor: pointer;
  }
  .kb-key.space {
    width: 240px;
    font-size: 0.82rem;
    font-weight: 500;
  }
  /* Too little data: neutral + dashed edge — "no evidence yet", never "good". */
  .kb-key.muted {
    border-style: dashed;
  }
  .kb-key:disabled {
    cursor: default;
  }
  /* Selected key — the one whose corrections the panel is showing. A persistent
     app-blue ring (distinct from the transient focus outline below; the two
     stack on a focused-and-selected key). */
  .kb-key.selected {
    border-color: var(--focus-ring);
    box-shadow: 0 0 0 2px var(--focus-ring);
  }
  /* TRUE focus indicator only. The roving key is the tab anchor (tabindex 0)
     but shows NO ring unless it actually has keyboard focus. :focus-visible (not
     :focus) so a mouse click never leaves a ring, and nothing stays painted
     after focus moves away or the window is hidden and reshown. Exactly one key
     — the focused one — is ever ringed; arrowing moves it; tabbing out clears it. */
  .kb-key:focus-visible {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: no-preference) {
    .kb-key {
      transition: background-color 120ms ease;
    }
  }

  .kb-caption {
    margin: 0;
    max-width: 30rem;
    text-align: center;
    font-size: 0.9rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }
</style>
