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
  }

  // 12 catches = consent (brief): if the user didn't want it fixed, it wouldn't
  // have been caught 12×. Mirrors TIER1_MIN_OBSERVATIONS engine-side.
  const READY_THRESHOLD = 12;

  let activeTab: Tab = "statistics";
  let patterns: ImpactPattern[] = [];

  // The command already returns patterns sorted by obs desc, so each group keeps
  // that order (the brief's "sorted by obs desc").
  $: ready = patterns.filter((p) => p.ready);
  $: observing = patterns.filter((p) => !p.ready);

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
      focusActiveTab();
    });
    loadPatterns();
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
      <!-- Built in step 4, on the verified engine numbers. Honest empty state
           until then — never a predicted or placeholder figure. -->
      <div class="empty">
        <p class="empty-lead">This view mirrors how you type.</p>
        <p class="empty-sub">
          It fills in once there's a day of typing to reflect.
        </p>
      </div>
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
