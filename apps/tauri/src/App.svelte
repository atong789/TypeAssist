<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import DebugPanel from "./routes/DebugPanel.svelte";
  import Home from "./routes/Home.svelte";
  import Today from "./routes/Today.svelte";
  import PreviewSwitcher from "./lib/PreviewSwitcher.svelte";

  // Builder's debug view — hidden behind Cmd+Shift+D. Not a user feature.
  let debugOpen = false;

  // ---- Shell navigation -------------------------------------------------
  // Front-end rebuild, shell only. The locked design: FOUR primary tabs, then a
  // three-tier utility group at the bottom — Feedback as a soft accent
  // invitation ("Tell me what you think"), then About + Privacy & Terms of
  // Service as tiny muted links. Each opens a PLACEHOLDER panel for now; real
  // screen content is built one at a time next.
  type Route =
    | "home"
    | "today"
    | "progress"
    | "settings"
    | "feedback"
    | "about"
    | "privacy";

  // Only these four are tabs (a WAI-ARIA roving tablist). Icons are Tabler,
  // bundled locally (see main.ts) — never a CDN.
  const tabs: { route: Route; label: string; icon: string }[] = [
    { route: "home", label: "Home", icon: "ti-home" },
    { route: "today", label: "Today", icon: "ti-sun" },
    { route: "progress", label: "Progress", icon: "ti-chart-bar" },
    { route: "settings", label: "Settings", icon: "ti-settings" },
  ];

  // The utility destinations are NOT tabs — Feedback is the accent invite,
  // About / Privacy are muted footer links. Titles for the placeholder panels.
  const utilityTitles: Record<string, string> = {
    feedback: "Feedback",
    about: "About",
    privacy: "Privacy & Terms of Service",
  };

  let route: Route = "home";
  let tabEls: HTMLButtonElement[] = [];

  $: activeLabel =
    tabs.find((t) => t.route === route)?.label ?? utilityTitles[route] ?? "";

  // Sidebar tablist with roving tabindex (the four primary tabs only):
  //  - one tab stop (the selected tab); Tab enters here, Tab again exits to the
  //    Feedback invite → footer links → panel content; Shift+Tab reverses.
  //  - Up/Down (wrapping) and Home/End move focus between the four tabs.
  //  - MANUAL activation: arrows only move focus; Enter/Space (or click) selects
  //    — so a stray arrow never changes screens (fewer accidental navigations).
  function selectTab(i: number) {
    route = tabs[i].route;
  }

  function onTabKeydown(event: KeyboardEvent, index: number) {
    const count = tabs.length;
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
        return; // Enter/Space activate natively; Tab exits the list.
    }
    event.preventDefault();
    tabEls[next]?.focus();
  }

  // A screen asked to switch to another tab (e.g. Today's "…in Progress" link →
  // Progress, a real tab now). Sets the route and lands focus on the
  // destination tab, the same as picking it in the sidebar.
  function handleNavigate(event: CustomEvent<string>) {
    const target = event.detail as Route;
    if (tabs.some((t) => t.route === target) || target in utilityTitles) {
      route = target;
      focusSelectedTab();
    }
  }

  // Blur whatever the webview currently has focused (→ document.body). Used on
  // hide so the last-focused element doesn't keep its :focus for WebKit to
  // restore on the next open. The ring is a plain :focus outline (no class), so
  // blur() removes it; with focus cleared while hidden, the show repaint is
  // clean.
  function clearFocus() {
    const active = document.activeElement as HTMLElement | null;
    if (active && active !== document.body) active.blur?.();
  }

  // Land keyboard focus on the currently-selected tab so Up/Down works
  // immediately, with EXACTLY ONE focus ring. Applied twice:
  //   - next animation frame (after the window is shown), and
  //   - again ~120ms later as a safety net, because WebKit can restore the
  //     previously-focused element a beat AFTER our rAF runs (that late restore
  //     is what left a second ring). The re-apply only corrects a genuine
  //     desync (active ≠ the selected tab), so it never yanks focus from a clean
  //     state. The ring shows because the tab style is on :focus (not
  //     :focus-visible). No-op for a non-tab route (the tray only routes to tabs).
  function focusSelectedTab() {
    const apply = (force: boolean) => {
      const el = tabEls[tabs.findIndex((t) => t.route === route)];
      if (!el) return;
      const active = document.activeElement as HTMLElement | null;
      if (!force && active === el) return; // already correct — don't disturb it
      if (active && active !== el) active.blur?.();
      el.focus({ preventScroll: true });
    };
    requestAnimationFrame(() => apply(true));
    setTimeout(() => apply(false), 120);
  }

  // Menu-bar entry: the tray's "Open TypeAssist" / "Settings" items show this
  // (otherwise hidden) window and ask it to land on a route. The webview stays
  // loaded across hide/show, so this listener is registered once; `app://route`
  // fires on EVERY re-show, so re-focusing here covers each menu-bar open.
  onMount(() => {
    const unlistenRoute = listen<string>("app://route", (e) => {
      const target = e.payload as Route;
      if (tabs.some((t) => t.route === target) || target in utilityTitles) {
        route = target;
      }
      focusSelectedTab();
    });

    // Deterministic hide signal from Rust (the main window only hides; emitted
    // in its CloseRequested handler). visibilitychange does NOT fire on a native
    // menu-bar hide, so this is the reliable trigger to clear focus while
    // hidden — nothing stale then survives to be restored on the next open.
    const unlistenHidden = listen("app://main-hidden", () => clearFocus());

    // Belt-and-suspenders for platforms/cases where visibilitychange DOES fire
    // (and a plain app-switch keeps the window visible, so it won't fire then —
    // Cmd-Tabbing away never disturbs focus).
    function onVisibilityChange() {
      if (document.hidden) clearFocus();
      else focusSelectedTab();
    }
    document.addEventListener("visibilitychange", onVisibilityChange);

    // First open (and any case where the window is already visible at mount).
    focusSelectedTab();
    return () => {
      unlistenRoute.then((off) => off());
      unlistenHidden.then((off) => off());
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  });

  // Focus trap: keep keyboard focus inside the app's controls. In a WebView,
  // Tab past the last focusable element hands focus to the host window — a
  // ringless, non-DOM location — before it wraps. So we wrap it ourselves.
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
    if (event.metaKey && event.shiftKey && (event.key === "d" || event.key === "D")) {
      event.preventDefault();
      debugOpen = !debugOpen;
      return;
    }
    if (event.key !== "Tab" || !rootEl) return;
    const tabbables = realTabbables();
    if (tabbables.length === 0) return;
    const first = tabbables[0];
    const last = tabbables[tabbables.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (!active || !rootEl.contains(active)) return;

    if (!event.shiftKey) {
      const atOrPastLast =
        active === last ||
        (active.compareDocumentPosition(last) & Node.DOCUMENT_POSITION_PRECEDING) !== 0;
      if (atOrPastLast) {
        event.preventDefault();
        first.focus();
      }
    } else {
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

<!-- Focus trap: keeps keyboard focus within the app's interactive controls. -->
<svelte:window on:keydown={onWindowKeydown} />

<main bind:this={rootEl}>
  <nav class="sidebar" aria-label="Primary">
    <div class="tabs" role="tablist" aria-orientation="vertical">
      {#each tabs as item, i}
        <button
          role="tab"
          id={`tab-${item.route}`}
          aria-controls="screen-panel"
          aria-selected={route === item.route}
          tabindex={route === item.route ? 0 : -1}
          class:active={route === item.route}
          bind:this={tabEls[i]}
          on:click={() => selectTab(i)}
          on:keydown={(e) => onTabKeydown(e, i)}
        >
          <i class={`ti ${item.icon}`} aria-hidden="true"></i>
          <span>{item.label}</span>
        </button>
      {/each}
    </div>
  </nav>

  <div class="panel" id="screen-panel" role="tabpanel" aria-labelledby={`tab-${route}`}>
    <!-- TEMPORARY: dev preview-state switcher, shared across every state-aware
         screen. Remove with automatic state detection. -->
    <PreviewSwitcher />
    {#if route === "home"}
      <Home />
    {:else if route === "today"}
      <Today on:navigate={handleNavigate} />
    {:else}
      <header class="screen-header"><h1>{activeLabel}</h1></header>
      <p class="placeholder">Shell only — this screen’s content is coming next.</p>
    {/if}
  </div>

  <!-- Utility group — Feedback invite + About / Privacy links. Visually pinned
       to the BOTTOM of the sidebar (grid area), but placed AFTER the panel in
       SOURCE order so Tab reaches the active screen's content before it. -->
  <nav class="utility" aria-label="Secondary">
    <!-- Feedback — a soft accent invitation, not a tab. -->
    <button
      class="navcta"
      class:active={route === "feedback"}
      on:click={() => (route = "feedback")}
    >
      <i class="ti ti-message-dots" aria-hidden="true"></i>
      <span>Tell me what you think</span>
    </button>

    <!-- About / Privacy — tiny muted links. -->
    <div class="navfoot">
      <button
        class="navfootlink"
        class:active={route === "about"}
        on:click={() => (route = "about")}>About</button
      >
      <button
        class="navfootlink"
        class:active={route === "privacy"}
        on:click={() => (route = "privacy")}>Privacy &amp; Terms of Service</button
      >
    </div>
  </nav>
</main>

{#if debugOpen}
  <DebugPanel />
{/if}

<style>
  main {
    display: grid;
    grid-template-columns: 206px 1fr;
    grid-template-rows: 1fr auto;
    grid-template-areas:
      "sidebar panel"
      "utility panel";
    height: 100vh;
  }
  /* The sidebar holds ONLY the primary tablist now; the utility group is a
     separate grid area below it. That lets the utility group come later in
     source order (so Tab reaches screen content first) while staying visually
     pinned to the sidebar bottom. Both carry the right border so the two read as
     one continuous sidebar. */
  .sidebar {
    grid-area: sidebar;
    padding: 1rem 0.85rem 0.5rem;
    border-right: 1px solid var(--hairline);
  }
  .utility {
    grid-area: utility;
    display: flex;
    flex-direction: column;
    padding: 0.5rem 0.85rem 1rem;
    border-right: 1px solid var(--hairline);
  }
  .tabs {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }

  /* ---- Primary tabs ---------------------------------------------------- */
  nav button[role="tab"] {
    /* Large, mouse-forgiving target: >= 44px tall, full sidebar width. */
    display: flex;
    align-items: center;
    gap: 0.65rem;
    min-height: 44px;
    text-align: left;
    padding: 0.5rem 0.8rem;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 8px;
    font: inherit;
    color: inherit;
    cursor: pointer;
  }
  nav button[role="tab"] .ti {
    font-size: 1.2rem;
    color: var(--text-secondary);
    flex-shrink: 0;
  }
  nav button[role="tab"]:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  /* Current screen: accent-tinted fill + bold label + left accent bar. Tinted
     (not grey) so it can't read as hover; filled (not outline) so it can't read
     as the focus ring. */
  nav button[role="tab"].active {
    background: color-mix(in srgb, var(--focus-ring) 16%, canvas);
    font-weight: 600;
    box-shadow: inset 4px 0 0 0 var(--focus-ring);
  }
  nav button[role="tab"].active .ti {
    color: var(--focus-ring);
  }
  nav button[role="tab"].active:hover {
    background: color-mix(in srgb, var(--focus-ring) 22%, canvas);
  }

  /* ---- Feedback: soft accent invitation (not a tab) -------------------- */
  .navcta {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    width: 100%;
    min-height: 42px;
    text-align: left;
    padding: 0.55rem 0.8rem;
    margin-bottom: 0.6rem;
    font: inherit;
    font-size: 0.92rem;
    color: color-mix(in srgb, var(--focus-ring) 85%, canvastext);
    background: color-mix(in srgb, var(--focus-ring) 10%, canvas);
    border: 1px solid color-mix(in srgb, var(--focus-ring) 30%, canvas);
    border-radius: 9px;
    cursor: pointer;
  }
  .navcta .ti {
    font-size: 1.1rem;
    flex-shrink: 0;
  }
  .navcta:hover,
  .navcta.active {
    background: color-mix(in srgb, var(--focus-ring) 16%, canvas);
  }

  /* ---- About / Privacy: tiny muted links ------------------------------- */
  .navfoot {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.15rem;
  }
  .navfootlink {
    padding: 0.3rem 0.2rem;
    background: transparent;
    border: none;
    border-radius: 5px;
    font: inherit;
    font-size: 0.78rem;
    color: var(--text-secondary);
    text-align: left;
    cursor: pointer;
  }
  .navfootlink:hover,
  .navfootlink.active {
    color: canvastext;
    text-decoration: underline;
  }

  /* Shared focus ring for every sidebar control (on :focus, not only
     :focus-visible, so programmatic focus shows it too). */
  nav button:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }

  /* ---- Panel ----------------------------------------------------------- */
  .panel {
    grid-area: panel;
    overflow-y: auto;
    overflow-x: clip;
    min-width: 0;
    padding: 1.5rem 2rem;
  }
  .panel:focus,
  .panel:focus-visible {
    outline: 3px solid var(--focus-ring);
    outline-offset: -3px;
  }
  .placeholder {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.95rem;
  }
</style>
