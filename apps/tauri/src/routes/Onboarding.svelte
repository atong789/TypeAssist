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

  const dispatch = createEventDispatcher<{ done: void }>();

  let step = 1;
  let name = ""; // starts empty on a true first run; kept across Back/Next
  let restoreOpen = false;
  let axOn = false; // is Accessibility (capture) currently on? (drives step 2)

  let rootEl: HTMLElement;
  let inputEl: HTMLInputElement | undefined;
  let openBtn: HTMLButtonElement | undefined;
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
    await tick();
    focusStep();
  }

  // Focus on arrival lands directly on the step's MAIN ACTION — no Tab needed.
  // Step 2's main action depends on whether Accessibility is already on: the
  // "Open Accessibility settings" button when off, otherwise Next. The ring shows
  // immediately (controls style :focus, not only :focus-visible). A rAF re-apply
  // corrects any late focus shift — e.g. a just-removed button dumping focus.
  function focusStep() {
    const target = () =>
      step === 1 ? inputEl : step === 2 ? (axOn ? nextBtn : openBtn) : getStartedBtn;
    const apply = () => target()?.focus();
    apply();
    requestAnimationFrame(apply);
  }

  function openAccessibility() {
    // Opens System Settings ▸ Privacy & Security ▸ Accessibility (Rust command).
    invoke("open_accessibility_settings").catch(() => {});
  }

  // ---- Step 2: Accessibility state, watched for the whole flow so step 2 always
  // reflects the truth on arrival. `engine://capture-health` reports `live` when
  // the sidecar's tap is enabled (the grant in effect) — BUT the watchdog also
  // RE-EMITS the current state every few seconds, so merely receiving `live` is
  // not a grant. We act only on the off→on TRANSITION (otherwise a re-emit would
  // bounce the screen).
  let unlistenHealth: (() => void) | null = null;
  function onHealth(state: string | undefined) {
    const live = state === "live";
    const wasOn = axOn;
    axOn = live;
    // ONLY the moment Accessibility flips off→on while the user is on step 2
    // (returning from System Settings) do we re-activate our window and move focus
    // to Next. We never auto-advance — the user continues at their own pace.
    // Arriving at step 2 already-on (Back from step 3, or a replay) is NOT a
    // transition, so nothing fires: the "on" state just shows, ring on Next.
    if (!wasOn && live && step === 2) onGrantedOnStep2();
  }
  async function onGrantedOnStep2() {
    await invoke("focus_main_window").catch(() => {}); // bring our window to front
    await tick(); // step 2 re-rendered to its "on" state — land focus on Next
    nextBtn?.focus();
  }

  function finish() {
    persistName();
    dispatch("done");
  }

  // ---- Restore from a backup (step 3) — the SAME guarded dialog as Settings.
  function openRestore(e: MouseEvent) {
    (e.currentTarget as HTMLElement | null)?.focus();
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
    // Track Accessibility state for the whole flow (the watchdog re-emits every
    // few seconds, so this converges to truth without waiting for a transition).
    listen<{ state: string }>("engine://capture-health", (e) => onHealth(e.payload?.state)).then(
      (un) => (unlistenHealth = un),
    );
    tick().then(() => inputEl?.focus());
  });
  onDestroy(() => unlistenHealth?.());
</script>

<svelte:window on:keydown={onKeydown} />

<div class="ob" bind:this={rootEl}>
  <div class="ob-card">
    <div class="ob-main">
      {#if step === 1}
        <h1 class="ob-hi">Hi, I’m Jordan.</h1>
        <p class="ob-lede">
          I’ll quietly learn how your hands type, and in time help you type more smoothly — all on
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
          <span>Everything stays on your Mac.</span>
        </div>
        <p class="ob-lede">
          Every keystroke you make stays right here — no cloud, no account, nobody else can see it.
        </p>
        <div class="ob-rule" role="presentation"></div>
        {#if axOn}
          <p class="ob-granted">
            <i class="ti ti-circle-check ob-granted-icon" aria-hidden="true"></i>
            <span>Accessibility is on — I can read your keystrokes now. You’re all set for this step.</span>
          </p>
          <p class="ob-fine">
            I skip password fields, and everything I keep stays on this Mac — just how your keys land, the little typo-and-fix habits your hands repeat, and the everyday words you use — never the sentences you write.
          </p>
        {:else}
          <p class="ob-body-t">
            To learn your hands, I need your permission to read your keystrokes. macOS will ask you
            to switch on Accessibility for TypeAssist.
          </p>
          <button class="ob-btn-primary" bind:this={openBtn} on:click={openAccessibility}>
            Open Accessibility settings
          </button>
          <p class="ob-fine">
            I skip password fields, and everything I keep stays on this Mac — just how your keys land, the little typo-and-fix habits your hands repeat, and the everyday words you use — never the sentences you write.
          </p>
        {/if}
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
        <button class="ob-btn next" bind:this={nextBtn} on:click={() => goTo(step + 1)}>Next</button>
      {:else}
        <span class="ob-nav-end" aria-hidden="true"></span>
      {/if}
    </footer>
  </div>

  {#if step === 3}
    <button class="ob-restore" on:click={openRestore}>Restore from a backup</button>
  {/if}
</div>

{#if restoreOpen}
  <RestoreDialog on:close={() => (restoreOpen = false)} on:restored={onRestored} />
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
  .ob-body-t {
    margin: 0 0 0.9rem;
    font-size: 0.95rem;
    line-height: 1.55;
    color: var(--text-secondary);
  }

  /* Step 2 "Accessibility is on" confirmation — blue check, never green. */
  .ob-granted {
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
    margin: 0 0 0.9rem;
    font-size: 0.95rem;
    line-height: 1.55;
    color: canvastext;
  }
  .ob-granted-icon {
    flex-shrink: 0;
    margin-top: 0.05rem;
    font-size: 1.15rem;
    color: var(--link);
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
  .ob-rule {
    height: 1px;
    background: var(--hairline);
    margin: 1.1rem 0;
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
</style>
