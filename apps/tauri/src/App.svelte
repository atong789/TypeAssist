<script lang="ts">
  import Home from "./routes/Home.svelte";
  import Settings from "./routes/Settings.svelte";
  import Practice from "./routes/Practice.svelte";
  import WarmUp from "./routes/WarmUp.svelte";
  import Today from "./routes/Today.svelte";
  import Progress from "./routes/Progress.svelte";

  type Route = "home" | "today" | "warmup" | "practice" | "settings" | "progress";

  const items: { route: Route; label: string }[] = [
    { route: "home", label: "Home" },
    { route: "today", label: "Today" },
    { route: "warmup", label: "Warm-up" },
    { route: "practice", label: "Practice" },
    { route: "settings", label: "Settings" },
  ];

  let route: Route = "home";
  let tabEls: HTMLButtonElement[] = [];

  // "progress" is a sub-view reached from Today, not a sidebar tab. While it's
  // open, keep Today lit and tabbable so the sidebar stays keyboard-reachable.
  $: activeTab = route === "progress" ? "today" : route;

  // Sidebar is a WAI-ARIA vertical tablist with roving tabindex:
  //  - one tab stop (the selected tab); Tab enters here, Tab again exits to the
  //    panel's interactive content, Shift+Tab reverses.
  //  - Up/Down (wrapping) and Home/End move focus between tabs.
  //  - MANUAL activation: arrows only move focus; Enter/Space (or click) selects.
  //    Chosen over auto-activation so a stray arrow press never changes screens
  //    — fewer accidental navigations for motor-impaired users.
  function selectIndex(i: number) {
    route = items[i].route;
  }

  function onTabKeydown(event: KeyboardEvent, index: number) {
    const count = items.length;
    let next: number;
    switch (event.key) {
      case "ArrowDown":
        next = (index + 1) % count;
        break;
      case "ArrowUp":
        next = (index - 1 + count) % count;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = count - 1;
        break;
      default:
        // Enter/Space activate the focused tab natively; Tab exits the list.
        return;
    }
    event.preventDefault();
    tabEls[next]?.focus();
  }

  // Route changes requested by a child view (e.g. Home's "Start" → Warm-up).
  function handleNavigate(event: CustomEvent<string>) {
    route = event.detail as Route;
  }

  // Focus trap: keep keyboard focus inside the app's controls. In a WebView,
  // Tab past the last focusable element hands focus to the host window — a
  // ringless, non-DOM location — before it wraps. So we wrap it ourselves:
  // Tab on the last control → first; Shift+Tab on the first → last.
  let rootEl: HTMLElement;

  function realTabbables(): HTMLElement[] {
    if (!rootEl) return [];
    const sel =
      'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]';
    return Array.from(rootEl.querySelectorAll<HTMLElement>(sel)).filter((el) => {
      const ti = el.getAttribute("tabindex");
      if (ti !== null && Number(ti) < 0) return false; // focusable but not tabbable (roving)
      return !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length); // visible
    });
  }

  function onWindowKeydown(event: KeyboardEvent) {
    if (event.key !== "Tab" || !rootEl) return;
    const tabbables = realTabbables();
    if (tabbables.length === 0) return;
    const first = tabbables[0];
    const last = tabbables[tabbables.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (!active || !rootEl.contains(active)) return;

    if (!event.shiftKey) {
      // Forward: wrap to first if focus is at or past the last tabbable.
      const atOrPastLast =
        active === last ||
        (active.compareDocumentPosition(last) & Node.DOCUMENT_POSITION_PRECEDING) !== 0;
      if (atOrPastLast) {
        event.preventDefault();
        first.focus();
      }
    } else {
      // Backward: wrap to last if focus is at or before the first tabbable.
      const atOrBeforeFirst =
        active === first ||
        (active.compareDocumentPosition(first) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
      if (atOrBeforeFirst) {
        event.preventDefault();
        last.focus();
      }
    }
  }

</script>

<!-- Focus trap: keeps keyboard focus within the app's interactive controls (see script). -->
<svelte:window on:keydown={onWindowKeydown} />

<main bind:this={rootEl}>
  <nav aria-label="Primary">
    <div class="tabs" role="tablist" aria-orientation="vertical">
      {#each items as item, i}
        <button
          role="tab"
          id={`tab-${item.route}`}
          aria-controls="screen-panel"
          aria-selected={activeTab === item.route}
          tabindex={activeTab === item.route ? 0 : -1}
          class:active={activeTab === item.route}
          bind:this={tabEls[i]}
          on:click={() => selectIndex(i)}
          on:keydown={(e) => onTabKeydown(e, i)}
        >{item.label}</button>
      {/each}
    </div>
  </nav>

  <div class="panel" id="screen-panel" role="tabpanel" aria-labelledby={`tab-${activeTab}`}>
    {#if route === "home"}<Home on:navigate={handleNavigate} />
    {:else if route === "today"}<Today on:navigate={handleNavigate} />
    {:else if route === "warmup"}<WarmUp />
    {:else if route === "practice"}<Practice />
    {:else if route === "settings"}<Settings />
    {:else if route === "progress"}<Progress />
    {/if}
  </div>
</main>

<style>
  main {
    display: grid;
    grid-template-columns: 220px 1fr;
    height: 100vh;
  }
  nav {
    padding: 1rem;
    border-right: 1px solid var(--hairline);
  }
  .tabs {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  nav button {
    /* Large, mouse-forgiving target: >= 44px tall, full sidebar width. */
    display: flex;
    align-items: center;
    min-height: 44px;
    text-align: left;
    padding: 0.5rem 0.85rem;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 8px;
    font: inherit;
    color: inherit;
    cursor: pointer;
  }
  /* "Hover" — a faint NEUTRAL-GREY wash, nothing more. Clearly secondary:
     "you could click this". Distinct in hue from the accent-tinted active
     state, so a hovered item never looks selected. */
  nav button:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  /* "Current screen" — a persistent ACCENT-TINTED fill + bold label + a solid
     accent bar down the left edge. Tinted (blue), not grey, so it can't be
     mistaken for hover; filled, not an outline, so it can't be mistaken for the
     keyboard-focus ring. State is conveyed by weight + bar + tint, not colour
     alone. */
  nav button.active {
    background: color-mix(in srgb, var(--focus-ring) 16%, canvas);
    font-weight: 600;
    box-shadow: inset 4px 0 0 0 var(--focus-ring);
  }
  /* Hovering the current screen keeps the active look (this rule's source order
     wins over :hover at equal specificity). */
  nav button.active:hover {
    background: color-mix(in srgb, var(--focus-ring) 22%, canvas);
  }
  /* "Keyboard focus" — a blue RING. Shape is clearly distinct from the filled
     active state above. On :focus (not just :focus-visible) so it's always shown. */
  nav button:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .panel {
    /* Vertical scroll only when content can't fit. `overflow-x: clip` stops the
       spurious HORIZONTAL scroll container WebKit would otherwise create (CSS
       computes overflow-x to `auto` when overflow-y is `auto` and overflow-x is
       `visible`). That container is keyboard-focusable but never matches
       :focus-visible — i.e. a ringless tab stop. Clip removes it entirely. */
    overflow-y: auto;
    overflow-x: clip;
    min-width: 0;
    padding: 1.5rem 2rem;
  }
  /* If the panel ever IS a scroll stop (tall content + short window), it must
     show a ring like every other focusable element. :focus (not just
     :focus-visible) because WebKit focuses scroll regions without the latter.
     Inset offset so the ring isn't clipped by the panel's own overflow. */
  .panel:focus,
  .panel:focus-visible {
    outline: 3px solid var(--focus-ring);
    outline-offset: -3px;
  }
</style>
