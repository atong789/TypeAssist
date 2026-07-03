<!-- First-run onboarding — shows once, before the main shell (no sidebar). The
     window's native traffic-light title bar sits above this; here we render a
     centred three-step flow: hello + name → privacy promise + the one permission
     → done (lands on Today in Day one, NOT a warm-up). Footer is Back · dots ·
     Next. Colour-blind-safe: blue for confirm/primary, amber only for caution,
     never red/green. Reduce Motion honoured (the dot only animates with motion).

     Carries its own focus trap (the shell's trap is inert until the shell mounts)
     and defers to the Restore dialog's trap while it's open. -->
<script lang="ts">
  import { onMount, onDestroy, tick, createEventDispatcher } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { userName } from "../lib/previewSettings";
  import { modalOpen } from "../lib/modal";
  import RestoreDialog from "../lib/RestoreDialog.svelte";
  import { pickAndPreviewBackup, type RestoreSummary } from "../lib/dataActions";

  const dispatch = createEventDispatcher<{ done: void }>();

  let step = 1;
  let name = ""; // starts empty on a true first run; kept across Back/Next
  let restoreOpen = false;
  let restorePath = "";
  let restoreSummary: RestoreSummary | null = null;
  let restoreError = "";
  // Capture needs TWO macOS grants — Accessibility (AX: focus + injection) AND
  // Input Monitoring (the keystroke CGEventTap). Two signals drive step 2:
  //  • `engine://permission-status` gives each grant's OWN read-only state
  //    (`axGranted` / `imGranted`), so each row ticks the moment ITS permission
  //    lands — even while the other is still pending (the whole point of the
  //    screen: people didn't realise BOTH were required).
  //  • `engine://capture-health` → `live` means both grants are in effect AND
  //    the tap is armed. That stays the gate for auto-advance + Next; the two
  //    booleans drive only the per-row visual status.
  let axGranted = false;
  let imGranted = false;
  let captureLive = false;
  // `live` implies both grants, but it can arrive a beat before a fresh
  // permission-status snapshot (or when we arrive already-on via Back/replay
  // before any snapshot lands) — so fold it in to avoid a row flashing "pending"
  // when capture is demonstrably running.
  $: axOn = axGranted || captureLive;
  $: imOn = imGranted || captureLive;

  let rootEl: HTMLElement;
  let inputEl: HTMLInputElement | undefined;
  let openBtn: HTMLButtonElement | undefined; // Accessibility row's "Open …" button
  let openImBtn: HTMLButtonElement | undefined; // Input Monitoring row's "Open …" button
  let nextBtn: HTMLButtonElement | undefined;
  let getStartedBtn: HTMLButtonElement | undefined;

  // Save the entered name as the display name (Jordan uses it elsewhere). Persist
  // on each navigation and at finish, so it survives Back/Next.
  function persistName() {
    userName.set(name.trim());
  }

  async function goTo(n: number) {
    persistName();
    step = Math.max(1, Math.min(3, n));
    // Poll for the grants only while sitting on step 2 with capture still off
    // (mirrors Reconnect's auto-resume). Arriving already-live (Back/replay) does
    // NOT poll — we hold still and just show the two confirmations.
    if (step === 2 && !captureLive) ensurePolling();
    else stopPolling();
    await tick();
    focusStep();
  }

  // Focus on arrival lands directly on the step's MAIN ACTION — no Tab needed.
  // Step 2 lands on the first STILL-PENDING permission's "Open … Settings"
  // button (so a half-granted return points at the one left to do); once both
  // are granted it lands on Next. The ring shows immediately (controls style
  // :focus, not only :focus-visible). A rAF re-apply corrects any late focus
  // shift — e.g. a just-removed button dumping focus.
  function step2Target() {
    if (captureLive) return nextBtn;
    if (!axOn) return openBtn;
    if (!imOn) return openImBtn;
    return nextBtn; // both granted, tap still arming — Next enables on `live`
  }
  function focusStep() {
    const target = () => (step === 1 ? inputEl : step === 2 ? step2Target() : getStartedBtn);
    const apply = () => target()?.focus();
    apply();
    requestAnimationFrame(apply);
  }

  // The two open-settings buttons reuse the existing per-pane Rust commands —
  // Privacy_Accessibility and Privacy_ListenEvent (Input Monitoring). Same two
  // panes the Reconnect panel opens.
  function openAccessibility() {
    invoke("open_accessibility_settings").catch(() => {});
  }
  function openInputMonitoring() {
    invoke("open_input_monitoring_settings").catch(() => {});
  }

  // ---- Step 2: capture-health, watched for the whole flow so step 2 always
  // reflects the truth on arrival. `engine://capture-health` reports `live` only
  // when BOTH grants are in effect and the tap is armed — BUT the watchdog also
  // RE-EMITS the current state every few seconds, so merely receiving `live` is
  // not a fresh grant. We auto-advance only on the off→on TRANSITION.
  //
  // Reconnect-style auto-resume: while on step 2 and not yet live, we re-probe by
  // asking the engine to restart capture every few seconds, so a dead sidecar
  // (it exits when a grant is missing) is respawned the moment both switches are
  // on and health flips to `live`.
  const POLL_MS = 3000;
  let pollTimer: ReturnType<typeof setInterval> | null = null;
  function ensurePolling() {
    if (pollTimer) return;
    // `start_capture` (not `restart_capture`): on a first run the engine is
    // deferred, so this is what lazily spawns the sidecar the moment the user
    // reaches the permission step — Accessibility is requested first, in context.
    // It's idempotent: the first call spawns; later calls (and existing users
    // replaying onboarding) just re-probe, so a just-granted permission flips
    // capture live within a poll cycle.
    invoke("start_capture").catch(() => {}); // spawn/probe immediately
    pollTimer = setInterval(() => invoke("start_capture").catch(() => {}), POLL_MS);
  }
  function stopPolling() {
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  let unlistenHealth: (() => void) | null = null;
  let unlistenPerms: (() => void) | null = null;
  function onHealth(state: string | undefined) {
    const live = state === "live";
    const wasLive = captureLive;
    captureLive = live;
    if (step === 2) {
      // Keep probing until both grants land; stop the moment capture is live.
      if (live) stopPolling();
      else ensurePolling();
    }
    // ONLY the moment capture flips off→on while the user is on step 2 (returning
    // from System Settings with the last switch flipped) do we auto-advance —
    // mirroring Reconnect's auto-resume. Arriving at step 2 already-live (Back
    // from step 3, or a replay) is NOT a transition, so nothing fires: the two
    // confirmations just show and Next is enabled as a manual fallback.
    if (!wasLive && live && step === 2) advanceFromStep2();
  }
  async function advanceFromStep2() {
    await invoke("focus_main_window").catch(() => {}); // bring our window to front
    goTo(3); // both grants are in — move on automatically
  }

  function finish() {
    persistName();
    dispatch("done");
  }

  // ---- Restore from a backup (step 3) — the SAME reordered flow as Settings:
  // pick the file → preview (read-only) → the guarded confirm describes it.
  async function openRestore(e: MouseEvent) {
    (e.currentTarget as HTMLElement | null)?.focus();
    restoreError = "";
    const r = await pickAndPreviewBackup();
    if (r.status === "cancelled") return;
    if (r.status === "error") {
      restoreError = r.message;
      return;
    }
    restorePath = r.path;
    restoreSummary = r.summary;
    restoreOpen = true;
  }
  function onRestored() {
    // A real restore replaces everything; the app then reflects whatever state the
    // backup holds. Finish onboarding and drop the user into the app.
    restoreOpen = false;
    dispatch("done");
  }

  // ---- Self-contained focus trap (keeps Tab inside the window on every step).
  function tabbables(): HTMLElement[] {
    if (!rootEl) return [];
    const sel =
      'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])';
    return Array.from(rootEl.querySelectorAll<HTMLElement>(sel)).filter(
      (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length),
    );
  }
  function onKeydown(event: KeyboardEvent) {
    if (event.key !== "Tab" || !rootEl) return;
    if ($modalOpen) return; // the Restore dialog owns Tab while open
    const t = tabbables();
    if (t.length === 0) return;
    const first = t[0];
    const last = t[t.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (active && !rootEl.contains(active)) {
      event.preventDefault();
      first.focus();
      return;
    }
    if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    } else if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    }
  }

  onMount(() => {
    // Aggregate capture-health drives the gate (Next + auto-advance). The
    // watchdog re-emits every few seconds, so this converges to truth without
    // waiting for a transition.
    listen<{ state: string }>("engine://capture-health", (e) => onHealth(e.payload?.state)).then(
      (un) => (unlistenHealth = un),
    );
    // Per-grant snapshot drives the two row statuses independently. The sidecar
    // emits it on every spawn (even when about to exit for a missing grant), so
    // while we poll on step 2 each row reflects ITS own permission within a poll
    // cycle of the user flipping that switch.
    listen<{ accessibility: boolean; input_monitoring: boolean }>(
      "engine://permission-status",
      (e) => {
        axGranted = !!e.payload?.accessibility;
        imGranted = !!e.payload?.input_monitoring;
      },
    ).then((un) => (unlistenPerms = un));
    tick().then(() => inputEl?.focus());
  });
  onDestroy(() => {
    stopPolling();
    unlistenHealth?.();
    unlistenPerms?.();
  });
</script>

<svelte:window on:keydown={onKeydown} />

<div class="ob" bind:this={rootEl}>
  <div class="ob-card">
    <div class="ob-main">
      {#if step === 1}
        <h1 class="ob-hi">Hi, I’m Jordan.</h1>
        <p class="ob-lede">
          I’ll quietly learn how your hands type, and in time help your words land the way you meant — all on
          this Mac. First, what should I call you?
        </p>
        <input
          class="ob-input"
          type="text"
          placeholder="Your name"
          bind:value={name}
          bind:this={inputEl}
          on:keydown={(e) => {
            if (e.key === "Enter") goTo(2);
          }}
          aria-label="Your name"
        />
      {:else if step === 2}
        <div class="ob-promise">
          <i class="ti ti-lock" aria-hidden="true"></i>
          <span>Everything stays on your Mac</span>
        </div>
        <p class="ob-lede">
          Jordan works entirely on this Mac — but macOS still needs your OK below. The moment it’s
          granted, Jordan starts learning.
        </p>

        <!-- Accessibility is the primary grant. On macOS 13–26 an Accessibility
             grant ALSO satisfies the keystroke tap's Input-Monitoring
             (ListenEvent) requirement — undocumented but consistent Catalina-era
             behaviour (Karabiner relies on it; see docs/macos-signing.md). So we
             DON'T list Input Monitoring as an upfront step: the row appears ONLY
             if, after Accessibility is granted, capture still isn't satisfied
             (`axOn && !imOn`) — i.e. only if a future macOS decouples the two, or
             a machine genuinely needs the separate grant. `imOn` folds capture
             health (`imGranted || captureLive`), so it flips true the instant the
             subsumption resolves — the row never flashes in the common case.
             Blue check on grant — never green/red. -->
        <div class="ob-perms">
          <div class="ob-perm" class:granted={axOn}>
            <span class="ob-perm-status" aria-hidden="true">
              {#if axOn}
                <i class="ti ti-circle-check"></i>
              {:else}
                <span class="ob-perm-ring"></span>
              {/if}
            </span>
            <div class="ob-perm-body">
              <span class="ob-perm-label">Accessibility</span>
              <span class="ob-perm-sub">Lets Jordan see which field you’re typing in, and stay out of password fields.</span>
              {#if axOn}
                <span class="ob-perm-allowed">
                  <i class="ti ti-check" aria-hidden="true"></i>Allowed
                </span>
              {:else}
                <button class="ob-perm-btn" bind:this={openBtn} on:click={openAccessibility}>
                  Open Accessibility Settings
                </button>
              {/if}
            </div>
          </div>

          <!-- Conditional: only when Accessibility is granted but capture still
               isn't satisfied (subsumption absent). Never rendered in the common
               case, so it's always the pending state — no "Allowed" branch. -->
          {#if axOn && !imOn}
            <div class="ob-perm">
              <span class="ob-perm-status" aria-hidden="true">
                <span class="ob-perm-ring"></span>
              </span>
              <div class="ob-perm-body">
                <span class="ob-perm-label">Input Monitoring</span>
                <span class="ob-perm-sub">This Mac also needs this one so Jordan can see the keys as you press them.</span>
                <button class="ob-perm-btn" bind:this={openImBtn} on:click={openInputMonitoring}>
                  Open Input Monitoring Settings
                </button>
              </div>
            </div>
          {/if}
        </div>

        <p class="ob-fine">Jordan moves on the moment capture starts.</p>
      {:else}
        <h1 class="ob-hi">{name.trim() ? `All set, ${name.trim()}.` : "All set."}</h1>
        <p class="ob-lede">
          Just type the way you always do — I’ll start learning straight away. Whenever you want a
          warm-up or a look at your progress, you’ll find me in the menu bar.
        </p>
        <button class="ob-btn-primary" bind:this={getStartedBtn} on:click={finish}>Get started</button>
      {/if}
    </div>

    <!-- Footer: Back (left) · step dots (centre) · Next (right). -->
    <footer class="ob-nav">
      <!-- No previous step on step 1 — omit Back entirely (a placeholder keeps the
           dots centred). -->
      {#if step > 1}
        <button class="ob-btn" on:click={() => goTo(step - 1)}>Back</button>
      {:else}
        <span class="ob-nav-end" aria-hidden="true"></span>
      {/if}

      <!-- Progress indicator only — NOT interactive, NOT in the tab order. -->
      <div class="ob-dots" role="img" aria-label={`Step ${step} of 3`}>
        {#each [1, 2, 3] as i}
          <span class="ob-dot" class:on={step === i} aria-hidden="true"></span>
        {/each}
      </div>

      {#if step < 3}
        <!-- Step 2 gates Next on both grants (manual fallback if detection lags). -->
        <button
          class="ob-btn next"
          bind:this={nextBtn}
          disabled={step === 2 && !captureLive}
          on:click={() => goTo(step + 1)}
        >
          Next
        </button>
      {:else}
        <span class="ob-nav-end" aria-hidden="true"></span>
      {/if}
    </footer>
  </div>

  {#if step === 3}
    <button class="ob-restore" on:click={openRestore}>Restore from a backup</button>
    {#if restoreError}
      <p class="ob-restore-error" role="alert">{restoreError}</p>
    {/if}
  {/if}
</div>

{#if restoreOpen && restoreSummary}
  <RestoreDialog
    path={restorePath}
    summary={restoreSummary}
    on:close={() => (restoreOpen = false)}
    on:restored={onRestored}
  />
{/if}

<style>
  .ob {
    position: relative;
    height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1.5rem;
    box-sizing: border-box;
  }
  .ob-card {
    width: 100%;
    max-width: 30rem;
    display: flex;
    flex-direction: column;
  }
  .ob-main {
    min-height: 16rem;
    outline: none; /* programmatic focus target; ring lives on the controls */
  }

  .ob-hi {
    margin: 0 0 0.7rem;
    font-size: 1.7rem;
    font-weight: 600;
    letter-spacing: -0.02em;
  }
  .ob-lede {
    margin: 0 0 1.1rem;
    font-size: 1rem;
    line-height: 1.6;
    color: var(--text-secondary);
  }
  /* ---- Step 2: the two permission rows ---- */
  .ob-perms {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
    margin: 0.3rem 0 1rem;
  }
  .ob-perm {
    display: flex;
    align-items: flex-start;
    gap: 0.7rem;
  }
  .ob-perm-status {
    flex-shrink: 0;
    width: 1.5rem;
    height: 1.5rem;
    display: flex;
    align-items: center;
    justify-content: center;
    margin-top: 0.1rem;
  }
  /* Not granted: hollow grey ring. Granted: it fills to a blue check (the
     ti-circle-check below) — never green, never red. */
  .ob-perm-ring {
    width: 1.15rem;
    height: 1.15rem;
    border-radius: 999px;
    border: 2px solid color-mix(in srgb, canvastext 32%, canvas);
    box-sizing: border-box;
  }
  .ob-perm-status .ti {
    font-size: 1.45rem;
    color: var(--link); /* app blue */
  }
  .ob-perm-body {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    min-width: 0;
  }
  .ob-perm-label {
    font-size: 0.98rem;
    font-weight: 600;
    color: canvastext;
  }
  .ob-perm-sub {
    font-size: 0.88rem;
    line-height: 1.45;
    color: var(--text-secondary);
  }
  /* Per-row action button — same blue primary, sized for a row. */
  .ob-perm-btn {
    align-self: flex-start;
    margin-top: 0.2rem;
    display: inline-flex;
    align-items: center;
    min-height: 36px;
    padding: 0.45rem 0.95rem;
    font: inherit;
    font-size: 0.92rem;
    font-weight: 500;
    border-radius: 9px;
    border: none;
    background: var(--focus-ring);
    color: #fff;
    cursor: pointer;
  }
  .ob-perm-btn:hover {
    background: color-mix(in srgb, var(--focus-ring) 88%, black);
  }
  .ob-perm-btn:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  /* Granted: quiet, non-actionable "Allowed" with a small blue check. */
  .ob-perm-allowed {
    align-self: flex-start;
    margin-top: 0.25rem;
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.9rem;
    font-weight: 500;
    color: var(--text-secondary);
  }
  .ob-perm-allowed .ti {
    font-size: 1.05rem;
    color: var(--link); /* app blue */
  }

  .ob-input {
    width: 100%;
    box-sizing: border-box;
    font: inherit;
    font-size: 0.98rem;
    padding: 0.7rem 0.8rem;
    border-radius: 9px;
    border: 1px solid var(--hairline);
    background: color-mix(in srgb, canvastext 4%, canvas);
    color: canvastext;
  }
  .ob-input::placeholder {
    color: var(--text-secondary);
  }
  .ob-input:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
    border-color: transparent;
  }

  /* Privacy promise pill. */
  .ob-promise {
    display: inline-flex;
    align-items: center;
    gap: 0.5rem;
    margin: 0 0 0.8rem;
    padding: 0.45rem 0.8rem;
    border-radius: 999px;
    background: color-mix(in srgb, var(--focus-ring) 14%, canvas);
    color: canvastext;
    font-size: 1rem;
    font-weight: 600;
  }
  .ob-promise .ti {
    font-size: 1.05rem;
  }
  .ob-btn-primary {
    display: inline-flex;
    align-items: center;
    min-height: 40px;
    padding: 0.55rem 1.15rem;
    font: inherit;
    font-size: 0.95rem;
    font-weight: 500;
    border-radius: 9px;
    border: none;
    background: var(--focus-ring);
    color: #fff;
    cursor: pointer;
  }
  .ob-btn-primary:hover {
    background: color-mix(in srgb, var(--focus-ring) 88%, black);
  }
  .ob-btn-primary:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .ob-fine {
    margin: 0.9rem 0 0;
    font-size: 0.85rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }

  /* ---- footer: Back · dots · Next ---- */
  .ob-nav {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 1.4rem;
  }
  .ob-btn {
    min-width: 5.4rem;
    min-height: 38px;
    padding: 0.5rem 1.05rem;
    font: inherit;
    font-size: 0.92rem;
    font-weight: 500;
    border-radius: 9px;
    border: 1px solid color-mix(in srgb, canvastext 28%, canvas);
    background: transparent;
    color: canvastext;
    cursor: pointer;
  }
  .ob-btn:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  .ob-btn.next {
    background: var(--focus-ring);
    border-color: var(--focus-ring);
    color: #fff;
  }
  .ob-btn.next:hover {
    background: color-mix(in srgb, var(--focus-ring) 88%, black);
  }
  /* Step 2: Next is disabled until both grants land (manual fallback). */
  .ob-btn.next:disabled {
    background: color-mix(in srgb, var(--focus-ring) 38%, canvas);
    border-color: transparent;
    color: color-mix(in srgb, #fff 75%, transparent);
    cursor: default;
  }
  .ob-btn.next:disabled:hover {
    background: color-mix(in srgb, var(--focus-ring) 38%, canvas);
  }
  .ob-btn:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  /* Keeps the dots centred when there's no Next button (step 3). */
  .ob-nav-end {
    min-width: 5.4rem;
  }

  .ob-dots {
    display: flex;
    gap: 0.5rem;
    align-items: center;
  }
  .ob-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: color-mix(in srgb, canvastext 22%, canvas);
  }
  .ob-dot.on {
    width: 22px;
    border-radius: 5px;
    background: var(--focus-ring);
  }
  @media (prefers-reduced-motion: no-preference) {
    .ob-dot {
      transition:
        width 160ms ease,
        background-color 160ms ease;
    }
  }

  /* Quiet restore link, pinned to the window's bottom-right (step 3 only). */
  .ob-restore {
    position: absolute;
    right: 1.5rem;
    bottom: 1.25rem;
    padding: 0.3rem 0.2rem;
    font: inherit;
    font-size: 0.85rem;
    color: var(--text-secondary);
    background: transparent;
    border: none;
    cursor: pointer;
  }
  .ob-restore:hover {
    color: canvastext;
    text-decoration: underline;
  }
  .ob-restore:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
    border-radius: 5px;
  }
  .ob-restore-error {
    position: absolute;
    right: 1.5rem;
    bottom: 0.4rem;
    margin: 0;
    max-width: 22rem;
    font-size: 0.8rem;
    text-align: right;
    color: #d23f3f;
  }
</style>
