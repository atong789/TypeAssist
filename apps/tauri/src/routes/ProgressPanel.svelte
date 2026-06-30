<!-- Progress dashboard — the menu-bar window that lets the user read their own
     typing data without Console.app. Lives in its OWN small webview window
     (label "progress"), shown from the tray, anchored under the menu-bar icon,
     hidden on click-away (see lib.rs `on_window_event`). Two tabs:

       Statistics — how I type   (built in step 4, on verified engine numbers)
       Impact     — the glance: the corrections behind your slip rate, top 5 in
                    each of Coordination / Precision, ranked by count. The full
                    list lives in the app (the integrated main-window Progress
                    tab), reached via "See the full list in the app →".

     Navigation is one-hand, no chords (brief): switch by click, or `1`/`2`, or
     `←`/`→`. Never a modifier combo. Scroll = wheel/trackpad or `↑`/`↓`/space.
     Reduce Motion is honoured by having NO tab-switch animation — the highlight
     just jumps. The window is a fixed height, sized so the Impact glance (its
     ten rows + the footer link) fits WITHOUT internal scroll (~600px); longer
     content (e.g. Statistics) still scrolls within the panel region.

     Design principles (do not violate): mirror not coach, report observed
     (keystrokes) never assumed (fingers), facts not commentary, mirror not
     scoreboard (neutral single colour, never red), observational + warm voice. -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import CorrectionPair from "../lib/CorrectionPair.svelte";

  type Tab = "statistics" | "impact";

  // One learned correction, as the engine sees it. `obs` is the DECAYED weight
  // (matches the kill-switch's view); `class` groups it (coord/precis); `highlight`
  // are the target char indices the fix changed (soft-blue mark). Derived on read.
  interface ImpactPattern {
    typed: string;
    target: string;
    obs: number;
    ready: boolean;
    // "coord" | "precis", derived engine-side from the pair; null for the rare
    // pair that isn't a clean motor slip.
    class: string | null;
    highlight: number[];
  }

  // The Impact tab is a glance: the top few corrections in each group, by count.
  const TOP_N = 5;

  // One calendar day's typing rollup, as the engine persists it (UTC-dated, to
  // match the dated motor snapshots). coord + precis === slips always.
  interface ProgressDay {
    date: string;
    words: number;
    slips: number;
    coord: number;
    precis: number;
  }

  // ---- Keyboard sub-view (per-key expansion of the two scores) -----------
  //
  // Tapping the Coordination or Precision row opens a QWERTY map coloured by
  // that score per key. `precision`/`coordination` are RAW decayed rates in
  // 0..1 (engine-side); colour intensity is scaled *relative to the user's own
  // worst key* here, so a board of small rates still reads. A key we've never
  // seen typed (`productions === 0`) is rendered neutral, never "good" — the
  // same `isSeen` gate the main-window Progress map uses (NOT `well_sampled`).
  type Score = "precision" | "coordination";
  interface KeyScore {
    key: string;
    precision: number;
    coordination: number;
    productions: number;
    well_sampled: boolean;
  }

  // "main" = the Statistics/Impact tabs; "keyboard" = the per-key drill-down.
  let view: "main" | "keyboard" = "main";
  let kbScore: Score = "precision";
  let keyScores: KeyScore[] = [];
  // The figure is revealed only on tap/focus/hover (brief: no numbers on keys).
  let selectedKey: string | null = null;
  let hoveredKey: string | null = null;
  // Roving tabindex over the QWERTY board: it's a spatial grid, so it's ONE Tab
  // stop and the arrow keys move between keys (Tab → arrows). Exactly one key
  // has tabindex 0 (the roving key); the rest are -1.
  let rovingKey = "q";
  let keyEls: Record<string, HTMLButtonElement> = {};
  // The score whose row opened the keyboard, so focus is restored to that exact
  // row on back. We track the score (not the DOM node) because the overview is
  // torn down while the keyboard is shown — the old button is detached, so
  // focusing it would silently drop the ring to <body>. `compRowEls` holds the
  // freshly-rendered row buttons by score so we can re-find the live one.
  let openerScore: Score | null = null;
  let compRowEls: Partial<Record<Score, HTMLButtonElement>> = {};

  let activeTab: Tab = "statistics";
  let patterns: ImpactPattern[] = [];
  let progressDays: ProgressDay[] = [];

  // The command returns patterns sorted by obs desc, so each group is already
  // ranked by count; the glance shows the top few in each.
  $: topCoord = patterns.filter((p) => p.class === "coord").slice(0, TOP_N);
  $: topPrecis = patterns.filter((p) => p.class === "precis").slice(0, TOP_N);
  $: hasImpact = topCoord.length > 0 || topPrecis.length > 0;
  const obsCount = (o: number) => Math.round(o);

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

  // The two composition rows, each a tap target into its per-key keyboard map.
  // Built in the script (not the template) so the `Score` typing is clean.
  $: compRows = [
    {
      name: "Coordination",
      caption: "right keys, right order",
      pct: coordPct,
      series: coordSeries,
      score: "coordination" as Score,
    },
    {
      name: "Precision",
      caption: "right key, clean hit",
      pct: precisPct,
      series: precisSeries,
      score: "precision" as Score,
    },
  ];

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

  function loadKeyScores() {
    invoke<KeyScore[]>("read_key_scores")
      .then((k) => {
        keyScores = k ?? [];
      })
      .catch(() => {
        // Read-only; keep the last-known board rather than blanking.
      });
  }

  // ---- Keyboard derivations ---------------------------------------------
  //
  // QWERTY home rows, lowercase (the app is content-blind and all-lowercase).
  // Row stagger mirrors a real keyboard so the map is recognisable at a glance.
  const KB_ROWS = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
  const KB_STAGGER = [0, 0.5, 1.5]; // half-key offsets per row

  $: scoreByKey = new Map(keyScores.map((k) => [k.key, k]));
  const rawOf = (k: KeyScore | undefined, s: Score) =>
    !k ? 0 : s === "precision" ? k.precision : k.coordination;

  // A key "has data" the moment it's been typed at all (`productions > 0`) —
  // the SAME rule the main-window Progress map uses (`isSeen`). We deliberately
  // do NOT gate per-key state on `well_sampled` (productions ≥ MIN_SAMPLES):
  // that higher bar made this popover disagree with the main window about which
  // state a low-but-nonzero-press key (e.g. `j`) is in — the main window showed
  // its real corrections while this surface said "too few presses". Precision
  // reflects the full motor-map per-key signal, not a corrections-only subset,
  // so any key the map has actually seen reads here, however few its presses.
  const isSeen = (k: KeyScore | undefined): k is KeyScore =>
    !!k && k.productions > 0;

  // The brightest key sets the top of the ramp (relative scaling). Only keys
  // we've actually seen can set it, so an unseen key can't blow out the scale.
  $: maxRaw = Math.max(
    0,
    ...keyScores.filter(isSeen).map((k) => rawOf(k, kbScore)),
  );

  // 0 (quiet, ~background) → 1 (bright blue, needs attention). Unseen keys
  // return null → painted neutral, not on the ramp.
  function intensity(letter: string): number | null {
    const k = scoreByKey.get(letter);
    if (!isSeen(k)) return null;
    if (maxRaw <= 0) return 0;
    return Math.min(1, rawOf(k, kbScore) / maxRaw);
  }

  // Background per key. Quiet keys sit just above the background; the worst key
  // reaches bright blue. The letter stays `canvastext` (the system foreground):
  // it contrasts cleanly over the whole ramp in BOTH light and dark mode
  // (dark-on-blue in light mode, light-on-blue in dark), where a fixed white
  // would fail on light-blue mid-tones.
  function keyStyle(letter: string): string {
    const i = intensity(letter);
    if (i === null) {
      // Neutral: we have too little data to say anything (never "good").
      return "background: color-mix(in srgb, canvastext 5%, canvas); color: var(--text-secondary);";
    }
    const mix = 4 + i * 80; // 4%..84% of the blue accent over the background
    return `background: color-mix(in srgb, var(--focus-ring) ${mix.toFixed(0)}%, canvas); color: canvastext;`;
  }

  const scoreLabel = (s: Score) => (s === "precision" ? "Precision" : "Coordination");
  const scoreCaption = (s: Score) =>
    s === "precision" ? "right key, clean hit" : "right keys, right order";

  // The figure revealed on tap (and announced to screen readers, so the map
  // never relies on colour alone). Coordination/precision share one phrasing
  // shape; an unsampled key says so plainly.
  function keyReadout(letter: string): string {
    const k = scoreByKey.get(letter);
    if (!isSeen(k)) {
      return `${letter} · too few presses yet to read`;
    }
    const pct = rawOf(k, kbScore) * 100;
    const pctStr = pct === 0 ? "0.0" : pct < 0.1 ? "<0.1" : fmt1(pct);
    const presses = Math.round(k.productions).toLocaleString();
    const what =
      kbScore === "precision"
        ? `${pctStr}% of presses mis-hit`
        : `letter-order slips on ${pctStr}% of presses`;
    return `${letter} · ${what} · ${presses} presses seen`;
  }

  // One plain-language line: where the trouble clusters for the active score.
  // Names only keys that are a real part of the cluster (≥25% of the worst),
  // so a lone hot key reads as "on x", not a misleading list.
  $: clusterSummary = (() => {
    const noun = kbScore === "precision" ? "mis-hits" : "letter-order slips";
    const sampled = keyScores.filter(isSeen);
    if (sampled.length === 0) {
      return `Not enough typing yet to show where your ${noun} cluster.`;
    }
    const ranked = sampled
      .map((k) => ({ key: k.key, raw: rawOf(k, kbScore) }))
      .filter((k) => k.raw > 0)
      .sort((a, b) => b.raw - a.raw);
    if (ranked.length === 0) {
      return `Your ${noun} are steady across every key — nothing stands out.`;
    }
    const top = ranked
      .filter((k) => k.raw >= ranked[0].raw * 0.25)
      .slice(0, 3)
      .map((k) => k.key);
    const list =
      top.length === 1
        ? top[0]
        : `${top.slice(0, -1).join(", ")} and ${top[top.length - 1]}`;
    return `Your ${noun} cluster on ${list}.`;
  })();

  // What the detail line shows: the hovered/focused key's figure, else the
  // cluster summary (so the line is never empty).
  $: shownKey = hoveredKey ?? selectedKey;
  $: detailLine = shownKey ? keyReadout(shownKey) : clusterSummary;

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

  // "See the full list in the app →" — hand off to the main window's Progress
  // tab (the integrated keyboard + corrections view, default all-corrections
  // state) and dismiss this glance. Rust shows + routes the main window.
  function openFullList() {
    invoke("open_main_progress").catch(() => {});
    close();
  }

  // Open the per-key keyboard for a score. Remembers which score's row opened it
  // so focus can be restored to that exact row on the way back (in-app sub-view
  // rule), and lands the ring on the back chevron (the sub-view's primary
  // anchor).
  let backEl: HTMLButtonElement;
  function openKeyboard(score: Score) {
    kbScore = score;
    openerScore = score;
    selectedKey = null;
    hoveredKey = null;
    rovingKey = "q"; // predictable: Tab into the board lands on the top-left key
    view = "keyboard";
    tick().then(() => backEl?.focus());
  }

  function goBack() {
    const score = openerScore;
    openerScore = null;
    view = "main";
    // Restore the ring to the row drilled in from. The overview re-renders on
    // this transition, so we focus the freshly-mounted button (looked up by
    // score), never a stale node — the ring must never fall to <body> here.
    // Fall back to the Statistics tab if the row isn't present, so focus is
    // still visible. scrollIntoView in case the row sits below the fold.
    tick().then(() => {
      const el = (score && compRowEls[score]) || statTabEl;
      el?.focus();
      el?.scrollIntoView({ block: "nearest" });
    });
  }

  // Switch which score the keyboard shows (one-hand: 1/2, no chord).
  function setScore(score: Score) {
    kbScore = score;
    selectedKey = null;
    hoveredKey = null;
  }

  // Arrow navigation across the QWERTY board (the board is a spatial grid, so
  // keys move with arrows, not Tab). Left/Right step within the row; Up/Down
  // land on the nearest key in the row above/below (clamped to that row's
  // length). Row ends are predictable: Left at column 0 / Right at the last
  // column stays put, and Up from the top row / Down from the bottom row is a
  // no-op (left to bubble — the window handler ignores ↑/↓ in this view).
  // stopPropagation keeps the window-level ←/→ handler from also firing.
  function onKeyKeydown(event: KeyboardEvent, r: number, c: number) {
    let nr = r;
    let nc = c;
    switch (event.key) {
      case "ArrowLeft":
        nc = Math.max(0, c - 1);
        break;
      case "ArrowRight":
        nc = Math.min(KB_ROWS[r].length - 1, c + 1);
        break;
      case "ArrowUp":
        if (r === 0) return;
        nr = r - 1;
        nc = Math.min(c, KB_ROWS[nr].length - 1);
        break;
      case "ArrowDown":
        if (r === KB_ROWS.length - 1) return;
        nr = r + 1;
        nc = Math.min(c, KB_ROWS[nr].length - 1);
        break;
      default:
        return;
    }
    event.preventDefault();
    event.stopPropagation();
    keyEls[KB_ROWS[nr][nc]]?.focus();
  }

  // Minimal focus trap — a separate window, so App.svelte's trap doesn't cover
  // it (same pattern as PracticePanel). Also owns the one-hand tab switching.
  function tabbables(): HTMLElement[] {
    if (!rootEl) return [];
    // Exclude tabindex="-1" everywhere — the QWERTY board is a roving-tabindex
    // grid, so only its one roving key is a real Tab stop (the rest are -1 and
    // must not count as trap boundaries).
    const sel =
      'a[href]:not([tabindex="-1"]), button:not([disabled]):not([tabindex="-1"]), [tabindex]:not([tabindex="-1"])';
    return Array.from(rootEl.querySelectorAll<HTMLElement>(sel)).filter(
      (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length),
    );
  }

  function onRootKeydown(event: KeyboardEvent) {
    // Never react to a modifier combo — the brief bans chords everywhere.
    if (event.metaKey || event.ctrlKey || event.altKey) return;

    if (event.key === "Escape") {
      event.preventDefault();
      // In the keyboard sub-view, Escape steps back to the tabs first (a second
      // Escape then closes the panel); on the tabs it closes.
      if (view === "keyboard") goBack();
      else close();
      return;
    }

    // 1/2 commit immediately, no chord. In the keyboard sub-view they switch
    // which score is shown; on the tabs they switch tab. ↑/↓/space are left
    // alone so they scroll natively.
    if (event.key === "1") {
      event.preventDefault();
      if (view === "keyboard") setScore("precision");
      else selectTab("statistics", true);
      return;
    }
    if (event.key === "2") {
      event.preventDefault();
      if (view === "keyboard") setScore("coordination");
      else selectTab("impact", true);
      return;
    }
    if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
      // On the tabs, ←/→ switch tab. In the keyboard sub-view the arrows move
      // between keys (handled on the focused key button via onKeyKeydown), so
      // the window leaves them alone here.
      if (view === "keyboard") return;
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
      view = "main";
      loadPatterns();
      loadProgress();
      loadKeyScores();
      focusActiveTab();
    });
    loadPatterns();
    loadProgress();
    loadKeyScores();
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

  {#if view === "main"}
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
  {:else}
    <!-- Keyboard sub-view header: a back chevron + the score name. The chevron
         is the sub-view's primary anchor (focused on arrival). -->
    <div class="subhead">
      <button
        class="screen-back"
        aria-label="Back to statistics"
        bind:this={backEl}
        on:click={goBack}
      >
        <svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
          <polyline points="15 18 9 12 15 6" />
        </svg>
      </button>
      <div class="subhead-titles">
        <div class="subhead-title">{scoreLabel(kbScore)}</div>
        <div class="subhead-caption">{scoreCaption(kbScore)} · by key</div>
      </div>
    </div>
  {/if}

  <div
    class="scroll"
    role="tabpanel"
    id="panel-progress"
    tabindex={view === "keyboard" ? -1 : 0}
    aria-labelledby={activeTab === "statistics" ? "tab-statistics" : "tab-impact"}
    bind:this={scrollEl}
  >
    {#if view === "keyboard"}
      <!-- Per-key keyboard map. NO numbers on the keys (brief) — the figure is
           revealed in the detail line on tap/focus/hover. Colour ramps deep→
           bright blue, relative to the user's own worst key; keys with too
           little data are neutral, never falsely "good". -->
      <div class="kb">
        <div class="kb-board" role="group" aria-label="{scoreLabel(kbScore)} by key">
          {#each KB_ROWS as rowKeys, r}
            <div class="kb-row" style="padding-left: {KB_STAGGER[r] * 2.5}rem">
              {#each rowKeys.split("") as letter, c}
                {@const k = scoreByKey.get(letter)}
                <button
                  class="kb-key"
                  class:muted={!isSeen(k)}
                  class:selected={selectedKey === letter}
                  style={keyStyle(letter)}
                  tabindex={rovingKey === letter ? 0 : -1}
                  aria-label={keyReadout(letter)}
                  aria-pressed={selectedKey === letter}
                  bind:this={keyEls[letter]}
                  on:click={() => (selectedKey = selectedKey === letter ? null : letter)}
                  on:keydown={(e) => onKeyKeydown(e, r, c)}
                  on:focus={() => {
                    rovingKey = letter;
                    hoveredKey = letter;
                  }}
                  on:blur={() => (hoveredKey = null)}
                  on:mouseenter={() => (hoveredKey = letter)}
                  on:mouseleave={() => (hoveredKey = null)}
                >
                  {letter}
                </button>
              {/each}
            </div>
          {/each}
        </div>

        <!-- Legend: quiet → needs attention, plus the neutral "too few" swatch.
             Pairs with the tap-to-reveal number so colour is never the only
             signal. -->
        <div class="kb-legend" aria-hidden="true">
          <div class="legend-ramp">
            <span class="legend-label">quiet</span>
            <span class="legend-bar"></span>
            <span class="legend-label">needs attention</span>
          </div>
          <div class="legend-muted">
            <span class="legend-swatch"></span>
            <span class="legend-label">too few presses</span>
          </div>
        </div>

        <!-- Detail line: the focused/hovered key's real figure, else the
             plain-language cluster summary. aria-live so it's announced. -->
        <p class="kb-detail" aria-live="polite">{detailLine}</p>
      </div>
    {:else if activeTab === "statistics"}
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
              {#each compRows as row}
                {@const pts = trendPoints(row.series)}
                {@const end = trendEnd(row.series)}
                <!-- Whole row is the tap target (brief: large, mouse-forgiving)
                     — opens the per-key keyboard map for this score. -->
                <button
                  class="comp-row"
                  bind:this={compRowEls[row.score]}
                  on:click={() => openKeyboard(row.score)}
                  aria-label="{row.name}, {fmt1(row.pct ?? 0)} percent. Open the per-key keyboard map."
                >
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
                  <svg class="comp-chevron" viewBox="0 0 24 24" width="16" height="16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">
                    <polyline points="9 6 15 12 9 18" />
                  </svg>
                </button>
              {/each}
              <p class="comp-hint">Tap a row to see your keyboard, key by key.</p>
              <p class="comp-sum">
                {fmt1(coordPct ?? 0)}% coordination + {fmt1(precisPct ?? 0)}% precision =
                your {fmt1(slipPct)}% slip rate.
              </p>
            {/if}
          </section>
        </div>
      {/if}
    {:else}
      <!-- Impact — the glance: the top few corrections in each group, by count.
           Read-only (no pills, no thresholds); the full list lives in the app. -->
      <div class="impact">
        <p class="impact-lead">What’s behind your slip rate</p>

        {#if !hasImpact}
          <p class="impact-empty">
            Nothing to show yet — the corrections you make as you type will gather here.
          </p>
        {:else}
          <!-- Tell the user this is a subset before they read the rows. -->
          <p class="t5">Your top 5 of each.</p>
          {#if topCoord.length > 0}
            <div class="grp">
              <div class="grp-head">
                <span class="grp-name">Coordination</span>
                <span class="grp-cap">right keys, right order</span>
              </div>
              <ul class="rows">
                {#each topCoord as p}
                  <li class="crow">
                    <CorrectionPair typed={p.typed} target={p.target} highlight={p.highlight} />
                    <span class="count">{obsCount(p.obs)}×</span>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}

          {#if topPrecis.length > 0}
            <div class="grp">
              <div class="grp-head">
                <span class="grp-name">Precision</span>
                <span class="grp-cap">right key, clean hit</span>
              </div>
              <ul class="rows">
                {#each topPrecis as p}
                  <li class="crow">
                    <CorrectionPair typed={p.typed} target={p.target} highlight={p.highlight} />
                    <span class="count">{obsCount(p.obs)}×</span>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}

          <p class="impact-note">The five you correct most often, in each.</p>
          <button class="impact-link" on:click={openFullList}>
            See the full list in the app →
          </button>
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
  /* Subtle focus cue only — a thin, faint inset hint so a keyboard user can tell
     the list has focus (↑/↓/space scroll it), WITHOUT the bold blue box that
     crowded the rows against the border. */
  .scroll:focus-visible {
    outline: 2px solid color-mix(in srgb, var(--focus-ring) 30%, transparent);
    outline-offset: -2px;
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

  /* Composition — each row is a button (whole row tappable, opens the per-key
     keyboard): name + caption, a neutral trend line, the %, a chevron. */
  .comp-row {
    width: 100%;
    display: grid;
    grid-template-columns: 1fr auto auto auto;
    align-items: center;
    gap: 0.9rem;
    padding: 0.6rem 0.4rem;
    margin: 0 -0.4rem;
    border: none;
    border-bottom: 1px solid var(--hairline);
    border-radius: 8px;
    background: transparent;
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .comp-row:hover {
    background: color-mix(in srgb, canvastext 5%, canvas);
  }
  .comp-row:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: -1px;
  }
  .comp-chevron {
    color: var(--text-secondary);
    flex-shrink: 0;
  }
  .comp-hint {
    margin: 0.6rem 0 0;
    color: var(--text-secondary);
    font-size: 0.82rem;
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

  /* ---- Keyboard sub-view header (replaces the tabs while drilling in) ---- */
  .subhead {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 1rem;
    min-height: 40px;
  }
  .subhead-titles {
    display: flex;
    flex-direction: column;
  }
  .subhead-title {
    font-size: 1.05rem;
    font-weight: 700;
    letter-spacing: -0.01em;
  }
  .subhead-caption {
    color: var(--text-secondary);
    font-size: 0.82rem;
  }

  /* ---- Keyboard map ---- */
  .kb {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    /* The focused-key ring is outset (outline-offset: 2px + 3px outline = 5px
       beyond the key). The board is the scroll region's content and `.scroll`
       clips overflow (overflow-y: auto makes overflow-x compute to auto too),
       so without room the top-row and edge rings get clipped. This padding
       gives the ring space to draw fully on every edge. */
    padding: 6px;
  }
  .kb-board {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
  }
  .kb-row {
    display: flex;
    gap: 0.35rem;
  }
  .kb-key {
    flex: 1;
    min-width: 0;
    height: 40px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--hairline);
    border-radius: 8px;
    font: inherit;
    font-size: 0.95rem;
    font-weight: 600;
    /* background + color come from the inline keyStyle() ramp */
    cursor: pointer;
  }
  /* Keys with too little data: neutral, dashed edge so they read as "no
     evidence yet", never as a good (quiet) score. */
  .kb-key.muted {
    border-style: dashed;
  }
  .kb-key:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .kb-key.selected {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: no-preference) {
    .kb-key {
      transition: background-color 120ms ease;
    }
  }

  /* Legend — the ramp the keys use, plus the neutral "too few" swatch. */
  .kb-legend {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem 1rem;
  }
  .legend-ramp {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex: 1;
    min-width: 0;
  }
  .legend-bar {
    flex: 1;
    min-width: 0;
    height: 10px;
    border-radius: 5px;
    /* the 4%..84% blue ramp the keys span */
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--focus-ring) 4%, canvas),
      color-mix(in srgb, var(--focus-ring) 84%, canvas)
    );
    border: 1px solid var(--hairline);
  }
  .legend-muted {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .legend-swatch {
    width: 14px;
    height: 14px;
    border-radius: 4px;
    border: 1px dashed var(--hairline);
    background: color-mix(in srgb, canvastext 5%, canvas);
  }
  .legend-label {
    color: var(--text-secondary);
    font-size: 0.78rem;
    white-space: nowrap;
  }

  /* Detail line — the focused/hovered key's real figure, else the cluster
     summary. The mono key reads clearly against the warm prose. */
  .kb-detail {
    margin: 0;
    padding: 0.7rem 0.85rem;
    border-radius: 10px;
    background: color-mix(in srgb, canvastext 4%, canvas);
    border: 1px solid var(--hairline);
    color: canvastext;
    font-size: 0.9rem;
    line-height: 1.45;
    min-height: 2.6rem;
    display: flex;
    align-items: center;
  }

  /* ---- Impact ---- */
  .impact {
    display: flex;
    flex-direction: column;
  }
  .impact-lead {
    margin: 0 0 0.25rem;
    font-size: 0.98rem;
    font-weight: 600;
    letter-spacing: -0.01em;
    color: canvastext;
  }
  /* Quiet secondary sub-line: "this is a subset" before the rows. */
  .t5 {
    margin: 0 0 0.2rem;
    font-size: 0.8rem;
    color: var(--text-secondary);
  }
  .impact-empty {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.92rem;
    line-height: 1.5;
  }
  .grp {
    margin-top: 0.55rem;
  }
  .grp-head {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    padding-bottom: 0.25rem;
    border-bottom: 1px solid var(--hairline);
  }
  .grp-name {
    font-size: 0.92rem;
    font-weight: 600;
  }
  .grp-cap {
    font-size: 0.78rem;
    color: var(--text-secondary);
  }
  .rows {
    margin: 0;
    padding: 0;
    list-style: none;
  }
  /* Compact rows so all ten (5 + 5) fit the fixed window with no inner scroll. */
  .crow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.3rem 0;
    border-bottom: 1px solid color-mix(in srgb, var(--hairline) 60%, transparent);
  }
  .count {
    flex-shrink: 0;
    font-size: 0.84rem;
    font-variant-numeric: tabular-nums;
    color: var(--text-secondary);
  }
  .impact-note {
    margin: 0.7rem 0 0;
    font-size: 0.82rem;
    color: var(--text-secondary);
  }
  /* Quiet link to the full list — app blue, never a heavy button. */
  .impact-link {
    align-self: flex-start;
    margin-top: 0.4rem;
    padding: 0.2rem 0.1rem;
    font: inherit;
    font-size: 0.85rem;
    font-weight: 500;
    color: var(--focus-ring);
    background: transparent;
    border: none;
    cursor: pointer;
  }
  .impact-link:hover {
    text-decoration: underline;
  }
  .impact-link:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
    border-radius: 5px;
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
