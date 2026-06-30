<!-- Progress — ONE integrated screen (main-window tab): the per-key keyboard map
     shows WHERE you slip, and the detail panel below shows the WORDS behind the
     shading. The map reuses read_key_scores (per-key precision / coordination
     rates, gated only by `productions > 0` — NOT `well_sampled`; the menu-bar
     Progress popover uses the same `isSeen` gate so a key reads identically on
     both surfaces); the panel reads read_word_patterns (the corrections,
     each tagged coord/precis, with the soft-blue corrected letter and the keys
     it involves). Selecting a key links the two: it highlights AND filters the
     panel to corrections touching that key; "← Show all corrections" clears it.

     Heat = the blue accent, deeper = more slips for the active lens (never red),
     scaled relative to the user's own worst live key. The map fills CONTINUOUSLY
     as data accrues — seen keys shade in, never-typed keys stay faint/dashed
     (we never force a key to "seen"); no per-state variants. Observe-only; reads
     nothing it shouldn't. Colours: app blue + soft blue only — never green/red. -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import CorrectionPair from "../lib/CorrectionPair.svelte";
  import { devProgressState } from "../lib/previewSettings";

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
  // Coordination leads (matches the menu-bar Progress popover's order), so it's
  // the default-selected lens too.
  let lens: Lens = "coordination";

  // DEV-ONLY override (Cmd+Shift+P overlay): force the day-one / no-data state on
  // a Mac that already has data. Folds to false in release builds (the guard),
  // and blanks the EFFECTIVE views only — real data on disk is never touched.
  // Everything downstream (keyboard, caption, impact panel) reads the `eff*`
  // arrays, so forcing empty makes the whole screen render its no-data state.
  $: forceEmpty = import.meta.env.DEV && $devProgressState === "empty";
  $: effKeyScores = forceEmpty ? [] : keyScores;
  $: effPatterns = forceEmpty ? [] : patterns;
  // Entering the forced day-one view, drop any stale key selection so the panel
  // shows the clean no-data state, not "The q key".
  $: if (forceEmpty) selectedKey = null;
  // The selected key links the map and the panel: null → the panel shows the
  // FULL list; a key → that key highlights and the panel filters to the
  // corrections that involve it. Selection follows focus (arrow-navigation) and
  // click; the panel's "← Show all corrections" clears it.
  let selectedKey: string | null = null;

  // The command returns patterns sorted by obs desc, so the list stays ranked by
  // count. With a key selected, scope to the corrections that touch it.
  const scopeToKey = (list: ImpactPattern[], key: string | null) =>
    key ? list.filter((p) => p.keys.includes(key)) : list;
  $: scopedPatterns = scopeToKey(effPatterns, selectedKey);
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
  $: scoreByKey = new Map(effKeyScores.map((k) => [k.key, k]));

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

  // Whether a key has been TYPED at all (v41): faint/dashed = truly no data =
  // never pressed (`productions > 0`), the SAME rule on both lenses. The moment a
  // key has any presses we show its real value, however small — a key typed but
  // with a 0 mis-hit rate OR 0 letter-order slips sits at the lowest shade, never
  // dashed (e.g. q pressed 141× with no swaps is "seen", not "unknown"). We never
  // hold a key faint for a low count and never synthesize a shade. The space bar
  // follows the same rule (no entry → never pressed → faint).
  const isSeen = (k: KeyScore | undefined): k is KeyScore =>
    !!k && k.productions > 0;
  function isLive(key: string): boolean {
    return isSeen(scoreByKey.get(key));
  }
  // Relative scaling — the brightest seen key for the active lens tops the ramp.
  $: maxRaw = Math.max(0, ...[...scoreByKey.values()].filter(isSeen).map((k) => rawOf(k, lens)));

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
    if (!isLive(key)) {
      // Faint = never pressed (v41), on either lens — never "not enough".
      return `${label} — I haven’t seen you type this key yet.`;
    }
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

  // Per-key render data, computed REACTIVELY off the loaded scores. This is the
  // load-bearing fix: the keyboard mounts before the async `read_key_scores`
  // resolves, and the per-key `style`/`muted`/`aria-label` in the markup are
  // function calls that don't, on their own, tell Svelte they depend on
  // scoreByKey/maxRaw/lens — so the board painted once (empty → faint) and never
  // repainted when the data arrived or the lens changed. Referencing
  // scoreByKey/maxRaw/lens HERE makes Svelte recompute this map (and thus
  // re-render every key) the moment scores load or the lens flips.
  interface KeyViz {
    style: string;
    live: boolean;
    detail: string;
  }
  $: keyViz = ((_sbk, _max, _lens) => {
    const m = new Map<string, KeyViz>();
    for (const row of ROWS) {
      for (const key of row) {
        m.set(key, { style: keyStyle(key), live: isLive(key), detail: keyDetail(key) });
      }
    }
    return m;
  })(scoreByKey, maxRaw, lens);

  const keyLabel = (key: string) => (key === SPACE ? "space" : key);

  // Per-key summary HEADLINE for the detail panel — the SAME line the menu-bar
  // Progress popover shows on tap ("k · 3.9% of presses mis-hit · 51 presses
  // seen"), so the two surfaces match word-for-word and number-for-number. Same
  // wording, same source: rawOf(k, lens) for the rate, k.productions for the
  // count, the popover's exact `fmt1`/`<0.1` formatting. Sits above the
  // itemized corrections. Reactive (refs lens + scoreByKey) so it tracks the
  // lens flip and the async score load.
  const fmt1 = (x: number) => x.toFixed(1);
  $: keySummary = ((_l, _sbk) => {
    if (!selectedKey) return "";
    const label = keyLabel(selectedKey);
    const k = scoreByKey.get(selectedKey);
    if (!isSeen(k)) {
      return `${label} · too few presses yet to read`;
    }
    const pct = rawOf(k, lens) * 100;
    const pctStr = pct === 0 ? "0.0" : pct < 0.1 ? "<0.1" : fmt1(pct);
    const presses = Math.round(k.productions).toLocaleString();
    const what =
      lens === "precision"
        ? `${pctStr}% of presses mis-hit`
        : `letter-order slips on ${pctStr}% of presses`;
    return `${label} · ${what} · ${presses} presses seen`;
  })(lens, scoreByKey);

  // (The former `selectedEmpty` per-key "No … recorded" line was retired — the
  // per-key headline above the list now states the rate + press count, so a
  // second line saying the same thing was pure redundancy. The full, no-key
  // empty message stays inline in the markup.)

  // Has the user typed at all yet? Drives the Progress day-one / no-data state
  // (v41): before the first keystroke there's nothing on the map. The moment any
  // key has data, Progress leaves this state for good (parallels Today's no-data
  // welcome). `productions` is the master signal — coordination can't exist
  // without it — but we OR both for safety.
  $: hasAnyData = effKeyScores.some((k) => k.productions > 0 || k.coordination > 0);

  // Caption — no-data-aware + lens-aware lead (v41):
  //   • No data yet → a gentle line in Jordan's voice (the keyboard is all faint).
  //   • With data → "tap a key…" + the faint-key meaning. Faint = never pressed on
  //     BOTH lenses, so both say "haven't seen you type yet"; only the lead clause
  //     (corrections vs letter-order slips) differs by lens.
  $: caption = !hasAnyData
    ? lens === "precision"
      ? "I haven’t seen you type yet, so there’s nothing on the map. Keep going — the keys you lean on fill in as I learn your hands."
      : "I haven’t seen you type yet, so there’s nothing on the map. Keep going — letter-order slips show up here as I learn your hands."
    : lens === "precision"
      ? "Tap a key, or use the arrow keys, to see the corrections behind it. The faint, dashed keys are ones I haven’t seen you type yet."
      : "Tap a key, or use the arrow keys, to see the letter-order slips behind it. The faint, dashed keys are ones I haven’t seen you type yet.";

  // Lens descriptor — the short "right key, clean hit" / "right keys, right order"
  // tags, matching the menu-bar Progress glance (v41).
  $: lensDesc =
    lens === "precision" ? "right key, clean hit" : "right keys, right order";

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
  // Coordination leads, Precision second — same order as the menu-bar Progress
  // popover, so both surfaces read "Coordination | Precision". The arrow-nav
  // order matches the visual button order (Left/Right cycle).
  const LENSES: Lens[] = ["coordination", "precision"];
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
      class:on={lens === "coordination"}
      role="radio"
      aria-checked={lens === "coordination"}
      tabindex={lens === "coordination" ? 0 : -1}
      bind:this={lensEls.coordination}
      on:click={() => (lens = "coordination")}
      on:keydown={onLensKeydown}>Coordination</button
    >
    <button
      class="lens-btn"
      class:on={lens === "precision"}
      role="radio"
      aria-checked={lens === "precision"}
      tabindex={lens === "precision" ? 0 : -1}
      bind:this={lensEls.precision}
      on:click={() => (lens = "precision")}
      on:keydown={onLensKeydown}>Precision</button
    >
  </div>
  <p class="lens-desc">{lensDesc}</p>
</div>

<!-- Keyboard block — centred, with the caption beneath it. -->
<div class="kb">
  <div class="kb-board" role="group" aria-label="{lens === 'precision' ? 'Precision' : 'Coordination'} by key">
    {#each ROWS as row, r}
      <div class="kb-row">
        {#each row as key, c}
          {@const v = keyViz.get(key)}
          <button
            class="kb-key"
            class:space={key === SPACE}
            class:muted={!v?.live}
            class:selected={selectedKey === key}
            style={v?.style}
            tabindex={rovingKey === key ? 0 : -1}
            aria-label={v?.detail}
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
    <h2 class="impact-title">What’s behind your slip rate</h2>
    <p class="impact-sub">
      Each one is a fix you made as you typed. Noticing them is how your keyboard map and
      percentages take shape.
    </p>
  {/if}

  <!-- Per-key headline — mirrors the menu-bar popover's tap readout, above the
       itemized corrections. Only when a key is selected. -->
  {#if selectedKey}
    <p class="key-summary">{keySummary}</p>
  {/if}

  {#if !hasShown}
    <!-- When a key is selected the headline above is the single per-key summary
         (it already states the rate + press count), so no second "No … recorded"
         line. The empty message is only for the full, no-key-selected list. -->
    {#if !selectedKey}
      <p class="impact-empty">
        No {activeName} corrections yet — they’ll gather here as you type.
      </p>
    {/if}
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
  /* Per-key headline — the popover's tap readout, shown above the corrections.
     Slightly emphasised (primary text, medium weight) so it reads as a summary
     of the key, not body copy. */
  .key-summary {
    margin: 0.1rem 0 0.2rem;
    font-size: 0.95rem;
    line-height: 1.5;
    font-weight: 500;
    /* No explicit colour — inherits the panel's primary text, like .impact-title. */
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
