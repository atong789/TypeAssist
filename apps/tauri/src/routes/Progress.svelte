<!-- Progress — the per-key keyboard map (main-window tab). Reuses the keyboard
     DATA + heat logic from the menu-bar ProgressPanel (read_key_scores: per-key
     precision / coordination rates, well_sampled), built to the locked design
     shape: fixed 40×40 keys, centred rows, a real space-bar row.

     Heat = the blue accent, deeper = more slips for the active lens (never red),
     scaled relative to the user's own worst live key. State-aware (shared
     appState): Day one = all faint/dashed + lens toggle disabled; Building /
     Fluent = REAL coverage — sampled keys shaded, under-sampled keys stay faint
     (we never force a key to "seen"). Captions + per-key detail wording are
     verbatim from the design doc. Observe-only; reads nothing it shouldn't. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { appState } from "../lib/previewSettings";

  type Lens = "precision" | "coordination";

  // RAW decayed rates in 0..1 (engine-side), per the live ProgressPanel.
  interface KeyScore {
    key: string;
    precision: number;
    coordination: number;
    productions: number;
    well_sampled: boolean;
  }

  let keyScores: KeyScore[] = [];
  let lens: Lens = "precision";
  let selectedKey: string | null = null;
  let hoveredKey: string | null = null;

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

  // Detail line: the focused/hovered key's figure, else the prompt; nothing on
  // Day one.
  $: shownKey = hoveredKey ?? selectedKey;
  $: detailLine =
    $appState === "day1"
      ? "Nothing here yet."
      : shownKey
        ? keyDetail(shownKey)
        : "Tap a key, or use the arrow keys, to see its detail.";

  $: caption =
    $appState === "day1"
      ? "I haven’t seen enough yet to show your pattern. Keep typing — this fills in on its own."
      : $appState === "building"
        ? "Filling in. The faint, dashed keys are ones I haven’t seen enough of yet — not “clean”, just unknown."
        : "Your full picture. Tap a key, or use the arrow keys, to see its detail.";

  $: lensDesc =
    lens === "precision"
      ? "How often each key gets mistyped."
      : "Keys that get swapped with the ones next to them.";

  function selectKey(key: string) {
    rovingKey = key; // tab stop follows the tap, too
    selectedKey = selectedKey === key ? null : key;
  }

  onMount(() => {
    invoke<KeyScore[]>("read_key_scores")
      .then((k) => (keyScores = k ?? []))
      .catch(() => {
        /* read-only; keep the last-known board rather than blanking */
      });
  });
</script>

<header class="screen-header"><h1>Progress</h1></header>

<!-- Lens toggle — left-aligned, with its one-line description under it. -->
<div class="lens">
  <div class="lens-toggle" role="group" aria-label="Score lens">
    <button
      class="lens-btn"
      class:on={lens === "precision"}
      aria-pressed={lens === "precision"}
      disabled={$appState === "day1"}
      on:click={() => (lens = "precision")}>Precision</button
    >
    <button
      class="lens-btn"
      class:on={lens === "coordination"}
      aria-pressed={lens === "coordination"}
      disabled={$appState === "day1"}
      on:click={() => (lens = "coordination")}>Coordination</button
    >
  </div>
  <p class="lens-desc">{$appState === "day1" ? " " : lensDesc}</p>
</div>

<!-- Keyboard block — centred, with the caption + detail beneath it. -->
<div class="kb">
  <div class="kb-board" role="group" aria-label="{lens === 'precision' ? 'Precision' : 'Coordination'} by key">
    {#each ROWS as row, r}
      <div class="kb-row">
        {#each row as key, c}
          <button
            class="kb-key"
            class:space={key === SPACE}
            class:muted={!isLive(key)}
            style={keyStyle(key)}
            tabindex={rovingKey === key ? 0 : -1}
            disabled={$appState === "day1"}
            aria-label={keyDetail(key)}
            aria-pressed={selectedKey === key}
            bind:this={keyEls[key]}
            on:click={() => selectKey(key)}
            on:keydown={(e) => onKeyKeydown(e, r, c)}
            on:focus={() => {
              rovingKey = key;
              hoveredKey = key;
            }}
            on:blur={() => (hoveredKey = null)}
            on:mouseenter={() => (hoveredKey = key)}
            on:mouseleave={() => (hoveredKey = null)}>{key === SPACE ? "space" : key}</button
          >
        {/each}
      </div>
    {/each}
  </div>

  <p class="kb-caption">{caption}</p>
  <div class="kb-detail" aria-live="polite">{detailLine}</div>
</div>

<style>
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
  .kb-detail {
    width: 100%;
    max-width: 32rem;
    margin: 0;
    padding: 0.7rem 0.9rem;
    border: 1px solid var(--hairline);
    border-radius: 10px;
    background: color-mix(in srgb, canvastext 4%, canvas);
    text-align: center;
    font-size: 0.9rem;
    line-height: 1.45;
    color: canvastext;
    min-height: 1.3rem;
  }
</style>
