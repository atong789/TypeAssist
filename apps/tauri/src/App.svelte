<script lang="ts">
  import { onMount, tick, type ComponentType } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import DebugPanel from "./routes/DebugPanel.svelte";
  import Today from "./routes/Today.svelte";
  import Progress from "./routes/Progress.svelte";
  import Settings from "./routes/Settings.svelte";
  import FeedbackDialog from "./lib/FeedbackDialog.svelte";
  import Onboarding from "./routes/Onboarding.svelte";
  import { modalOpen } from "./lib/modal";
  import { onboarded } from "./lib/previewSettings";

  // DEV-ONLY preview-state switcher. Loaded via a dynamic import GUARDED by
  // import.meta.env.DEV so a production build folds the branch to `false`, drops
  // the import, and never emits the component's JS *or* CSS chunk — it is
  // excluded from a release entirely, not merely hidden.
  let PreviewSwitcher: ComponentType | null = null;
  if (import.meta.env.DEV) {
    import("./lib/PreviewSwitcher.svelte").then((m) => (PreviewSwitcher = m.default));
  }

  // First-run onboarding ("DayOne") gates the shell. On "Get started" (or a
  // restore), it completes and drops the user onto Today — observe-only, NOT a
  // warm-up — so the first day breathes. Onboarding never recurs once done.
  //
  // `onboarded` is webview localStorage, which the app's bundle-id change reset —
  // so an EXISTING user could otherwise be shown onboarding again. The backend
  // `is_first_run` (learning data OR Accessibility grant) is the real signal: if
  // it says "not a first run", force `onboarded` true so an existing user skips
  // onboarding regardless of the reset. `firstRunResolved` gates the initial
  // render on this check so onboarding never flashes before it resolves. (Dev
  // "Replay onboarding" sets the flag false AFTER mount, within a session, so
  // this one-shot mount reconciliation doesn't undo it.)
  let firstRunResolved = false;
  onMount(async () => {
    try {
      // TWO-WAY reconciliation — the backend `is_first_run` is authoritative.
      // `ta.onboarded` lives in the WebKit data store, which survives app
      // deletion AND a `~/.typeassist` wipe, so a stale `true` would otherwise
      // suppress onboarding forever on a genuine first run. So first-run true ⇒
      // CLEAR the flag and show onboarding; false ⇒ set it and skip.
      onboarded.set(!(await invoke<boolean>("is_first_run")));
    } catch {
      // If the check fails, fall back to the stored flag as-is.
    }
    firstRunResolved = true;
    if ($onboarded) {
      // Existing-user shell just mounted — land the focus ring as the normal
      // launch path would (the sibling onMount's focus call ran before the gate).
      await tick();
      focusSelectedTab();
    }
  });

  async function completeOnboarding() {
    route = "today";
    onboarded.set(true);
    // Let the shell mount (tabEls + navRovingIndex update for "today"), then land
    // the focus ring on the Today nav item via the same deterministic nav focus
    // used on every tab switch.
    await tick();
    focusSelectedTab();
  }

  // Feedback opens as a modal (not a sidebar route). Focus the CTA before opening
  // so the modal records it and returns focus to it on close (WebKit doesn't
  // focus a button on click).
  let feedbackOpen = false;
  function openFeedback(e: MouseEvent) {
    (e.currentTarget as HTMLElement | null)?.focus();
    feedbackOpen = true;
  }

  // Builder's debug view — hidden behind Cmd+Shift+D. Not a user feature.
  let debugOpen = false;

  // DEV-ONLY state-preview overlay — hidden behind Cmd+Shift+P, dev builds only
  // (the toggle is guarded by import.meta.env.DEV, and the component itself is
  // dropped from release builds). Lets the builder force Today's empty/normal
  // state and replay onboarding without touching real data.
  let previewOpen = false;

  // ---- Shell navigation -------------------------------------------------
  // THREE primary tabs (Home retired — its only job, the Corrections invite,
  // moved to Today), then a three-tier utility group at the bottom — Feedback as
  // a soft accent invitation ("Tell me what you think"), then About + Privacy &
  // Terms of Service as tiny muted links.
  type Route =
    | "today"
    | "progress"
    | "settings"
    | "feedback"
    | "about"
    | "privacy";

  // Only these three are tabs (a WAI-ARIA roving tablist). Icons are Tabler,
  // bundled locally (see main.ts) — never a CDN.
  const tabs: { route: Route; label: string; icon: string }[] = [
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

  let route: Route = "today";
  let tabEls: HTMLButtonElement[] = [];

  // The nav's roving anchor: the index of the tab that holds tabindex="0" (the
  // single keyboard stop). It follows the selected tab, but when the active
  // route is a utility one (Feedback / About / Privacy — none of which are tabs)
  // it STAYS on the last selected tab, so the nav ALWAYS keeps exactly one tab
  // stop and never drops out of the Tab order (which is what let focus get
  // trapped in the utility group).
  let navRovingIndex = 0;
  $: {
    const idx = tabs.findIndex((t) => t.route === route);
    if (idx >= 0) navRovingIndex = idx;
  }

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
      const el = tabEls[navRovingIndex];
      if (!el) return;
      const active = document.activeElement as HTMLElement | null;
      if (!force && active === el) return; // already correct — don't disturb it
      if (active && active !== el) active.blur?.();
      el.focus({ preventScroll: true });
    };
    requestAnimationFrame(() => apply(true));
    setTimeout(() => apply(false), 120);
  }

  // Menu-bar entry: the tray's "Open TenCalmDigits" / "Settings" items show this
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
    if (
      import.meta.env.DEV &&
      event.metaKey &&
      event.shiftKey &&
      (event.key === "p" || event.key === "P")
    ) {
      event.preventDefault();
      previewOpen = !previewOpen;
      return;
    }
    // A modal owns Tab while it's open — stand down so the two traps don't fight.
    if ($modalOpen) return;
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

{#if !firstRunResolved}
  <!-- Deciding onboarding vs shell (a fast local IPC). Rendering nothing for this
       tick avoids flashing onboarding at an existing user whose webview
       localStorage was reset by the bundle-id change. -->
{:else if !$onboarded}
  <Onboarding on:done={completeOnboarding} />
{:else}
  <main bind:this={rootEl}>
  <nav class="sidebar" aria-label="Primary">
    <div class="tabs" role="tablist" aria-orientation="vertical">
      {#each tabs as item, i}
        <button
          role="tab"
          id={`tab-${item.route}`}
          aria-controls="screen-panel"
          aria-selected={route === item.route}
          tabindex={i === navRovingIndex ? 0 : -1}
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
    <!-- DEV-ONLY state-preview overlay: loaded only in dev (guarded dynamic
         import above) and shown only when toggled with Cmd+Shift+P. Absent from
         release builds entirely. -->
    {#if PreviewSwitcher && previewOpen}
      <svelte:component this={PreviewSwitcher} />
    {/if}
    {#if route === "today"}
      <Today on:navigate={handleNavigate} />
    {:else if route === "progress"}
      <Progress />
    {:else if route === "settings"}
      <Settings />
    {:else if route === "about"}
      <!-- About — copy verbatim from design doc Section 08 About panel. Reuses
           the Privacy wrapper/typography. Version is the app's actual 0.1.0. -->
      <header class="screen-header"><h1>{activeLabel}</h1></header>
      <div class="privacy about-stack">
        <!-- The TenCalmDigits hand mark (Noto 🖐, credited below). Decorative —
             the heading carries the name, so it's aria-hidden. -->
        <img class="about-mark" src="/hand.svg" alt="" aria-hidden="true" />
        <p class="about-lead">Hi, I’m Jordan.</p>
        <p class="privacy-p">
          I’m the quiet helper inside TenCalmDigits. I was made for hands that don’t always land where
          you mean — so I learn the small ways your fingers slip, and gently offer the word you
          meant. You’re always the one typing; I only ever suggest.
        </p>
        <p class="privacy-p">
          I keep a small dictionary, too. Most of it I came with — the everyday words. The rest is
          yours: your name, the people and places you write about, the words only you reach for. So
          when something is simply you, I know it’s right and leave it be.
        </p>
        <p class="privacy-p">
          If you ever want to see what I notice, that’s Progress. I keep a gentle record of how your
          hands are doing — the keys you land cleanly, and the moments letters arrive out of order.
          Not a score, not a test. Just a quiet picture, there if it helps.
        </p>
        <p class="privacy-p">
          The more we type together, the better I come to know your hands. Everything I learn stays
          right here on your Mac — no cloud, no account, no one else.
        </p>
        <p class="about-foot">TenCalmDigits · version 0.1.0 — everything stays on your Mac.</p>
      </div>
    {:else if route === "privacy"}
      <!-- Privacy & Terms — copy is verbatim from design doc Section 08 (honesty
           pass: the vocabulary count CAN include learned names/terms, and stays
           on-device). British "recognise" is intentional. -->
      <header class="screen-header"><h1>{activeLabel}</h1></header>
      <div class="privacy">
        <section class="privacy-block">
          <h2 class="privacy-h">What I learn</h2>
          <p class="privacy-p">
            The shape of how you type: which keys you slip on, the short words you fix (like “teh” →
            “the”), and your own everyday vocabulary — a private count of the words you use most,
            and how often. The words you reach for again and again become part of that count — your
            own names and terms included — so I recognise them instead of treating them as slips.
          </p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">What I never keep</h2>
          <p class="privacy-p">
            Your sentences, your documents, and your passwords. I skip secure fields entirely,
            ignore anything you paste or auto-fill, and keep no log of what you write. The count is
            only ever single words and how often you use them — never the sentences they sit in —
            and none of it ever leaves this Mac.
          </p>
        </section>

        <!-- Terms of Use — BETA PLACEHOLDER (lawyer-reviewed later). Copy is
             rendered verbatim; bracketed [PLACEHOLDERS] are intentional and must
             stay literal. Reuses the .privacy wrapper + .privacy-h/.privacy-p
             typography so Privacy and Terms read as one page. -->
        <section class="privacy-block terms-start">
          <h2 class="privacy-h">Terms of Use</h2>
          <p class="terms-note">DRAFT — beta placeholder.</p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">The short version</h2>
          <p class="privacy-p">
            TenCalmDigits is a small Mac app that watches how you type and gently offers corrections.
            It runs entirely on your Mac. By using it, you agree to the terms below.
          </p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">What it is</h2>
          <p class="privacy-p">
            TenCalmDigits ("the app") is a typing aid. The assistant inside it is called Jordan. The
            app learns the small ways your fingers slip and suggests the word you meant — you always
            decide whether to accept. It is a convenience tool, not a medical device or a treatment,
            and makes no health or recovery claims.
          </p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">On your Mac, and yours</h2>
          <p class="privacy-p">
            Everything the app learns stays on your Mac. There is no account, no cloud, and no
            tracking. The only thing that ever leaves your Mac is a feedback message, and only if you
            choose to write one and send it. The app does not read your passwords, secure fields, or
            the sentences you write — see Privacy above for the full detail.
          </p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">Your responsibilities</h2>
          <p class="privacy-p">
            Use the app on a Mac you control, signed in to your own macOS account. The app suggests;
            it never types for you — you are responsible for what you accept and for what you write.
            Don't try to misuse, reverse-engineer, or redistribute the app except as these terms
            allow.
          </p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">Permissions</h2>
          <p class="privacy-p">
            The app asks for macOS Accessibility and Input Monitoring so it can see your keystrokes.
            You can turn these off at any time in System Settings; the app simply stops watching when
            you do.
          </p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">No warranty</h2>
          <p class="privacy-p">
            The app is provided "as is," without warranty of any kind. It may make wrong suggestions
            or miss the word you meant. To the fullest extent allowed by law, [LEGAL ENTITY / YOUR
            NAME] is not liable for any loss arising from your use of the app. Always review your own
            text before it matters.
          </p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">Beta</h2>
          <p class="privacy-p">
            During the beta, the app is provided free for testing and feedback. Features may change
            or break. [Pricing / licensing terms for the public release — to be finalised.]
          </p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">Credits</h2>
          <p class="privacy-p">
            The hand icon is from Noto Emoji by Google, used under the SIL Open Font License 1.1. The
            full license text ships with the app.
          </p>
        </section>
        <section class="privacy-block">
          <h2 class="privacy-h">Changes &amp; contact</h2>
          <p class="privacy-p">
            These terms may be updated; the current version travels with the app. Questions: [CONTACT
            EMAIL]. These terms are governed by the laws of [JURISDICTION].
          </p>
        </section>
        <p class="terms-foot">Version 0.1 (beta) — [DATE]. Pending legal review.</p>
      </div>
    {:else}
      <header class="screen-header"><h1>{activeLabel}</h1></header>
      <p class="placeholder">Shell only — this screen’s content is coming next.</p>
    {/if}
  </div>

  <!-- Utility group — Feedback invite + About / Privacy links. Visually pinned
       to the BOTTOM of the sidebar (grid area), but placed AFTER the panel in
       SOURCE order so Tab reaches the active screen's content before it. -->
  <nav class="utility" aria-label="Secondary">
    <!-- Feedback — a soft accent invitation, not a tab; opens the modal. -->
    <button class="navcta" on:click={openFeedback}>
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
{/if}

{#if debugOpen}
  <DebugPanel />
{/if}

{#if feedbackOpen}
  <FeedbackDialog on:close={() => (feedbackOpen = false)} />
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

  /* ---- Privacy & Terms content (two blocks, doc Section 08) ---- */
  .privacy {
    max-width: 34rem;
  }
  .privacy-block {
    margin-top: 1.4rem;
  }
  .privacy-h {
    margin: 0 0 0.4rem;
    font-size: 1.02rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .privacy-p {
    margin: 0;
    font-size: 0.95rem;
    line-height: 1.65;
    color: canvastext;
  }

  /* ---- Terms of Use (beta placeholder; same wrapper + heading/body type) ---- */
  /* A hairline + extra space marks the Privacy → Terms break while keeping them
     one page. Headings reuse .privacy-h, bodies reuse .privacy-p. */
  .terms-start {
    margin-top: 2rem;
    padding-top: 1.6rem;
    border-top: 1px solid var(--hairline);
  }
  .terms-note {
    margin: 0;
    font-size: 0.85rem;
    color: var(--text-secondary);
  }
  .terms-foot {
    margin: 1.4rem 0 0;
    font-size: 0.85rem;
    color: var(--text-secondary);
  }

  /* ---- About panel (reuses .privacy wrapper + .privacy-p typography) ---- */
  .about-stack {
    display: flex;
    flex-direction: column;
    gap: 0.9rem;
    margin-top: 0.2rem;
  }
  .about-mark {
    width: 56px;
    height: 56px;
    margin: 0 0 0.1rem;
    /* SVG has its own transparent padding; no background/rounding needed. */
  }
  .about-lead {
    margin: 0;
    font-size: 1.05rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .about-foot {
    margin: 0.3rem 0 0;
    font-size: 0.85rem;
    color: var(--text-secondary);
  }
</style>
