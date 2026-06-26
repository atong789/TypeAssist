<!-- Reconnect panel — its own webview window (label "reconnect"), shown by the
     menu-bar "Reconnect…" recovery item when capture has stopped because a
     required permission was revoked (e.g. after an update). It reuses onboarding
     step 2's checkmark / "you're all set" VISUAL STYLING for its confirmation —
     NOT the onboarding component itself: no Back/Next, no dots, no full page.
     This is a small, self-contained window.

     TWO PERMISSIONS. Capture needs both Accessibility (the AX API: secure-field
     focus, correction injection) AND Input Monitoring (the keystroke-capture
     CGEventTap) — separate macOS grants that an update can revoke
     independently. Re-granting Accessibility alone does NOT restore capture, so
     the panel offers a button per pane and lets the user grant whatever's off.

     AUTO-RESUME: while open it polls by asking the engine to restart capture
     every few seconds; capture-health is the SOURCE OF TRUTH — only its
     `engine://capture-health` → `live` (both grants in effect + tap armed)
     confirms reconnection. On `live` we bring the panel back to the front
     (System Settings is frontmost then) and show a "Reconnected — Jordan is
     active again" confirmation that STAYS until the user dismisses it (Done /
     Esc / ✕) — no auto-dismiss; a too-fast vanish was unreadable. Rust shows +
     centres the window and does NOT hide it on blur (the user must leave to flip
     the switch), and the open-settings buttons drop the panel below Settings so
     the toggle is reachable — so the panel survives that round-trip. -->
<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  let rootEl: HTMLElement;
  let openBtn: HTMLButtonElement | undefined; // Accessibility row's "Open …" button
  let openImBtn: HTMLButtonElement | undefined; // Input Monitoring row's "Open …" button
  let doneBtn: HTMLButtonElement | undefined;

  // Land focus on the first STILL-PENDING permission's button, so a half-revoked
  // recovery points straight at the one left to grant.
  function focusFirstPending() {
    (axGranted ? openImBtn : openBtn)?.focus();
  }

  // We only react to capture coming back (and only keep respawning the sidecar)
  // while the panel is actually open — `polling` gates both, so a `live` event
  // during normal operation never hides an already-hidden window, and we don't
  // churn the sidecar in the background.
  let polling = false;
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  // Once capture is back we don't silently vanish — we surface a "Reconnected"
  // confirmation (mirrors onboarding step 2's "you're all set" styling) that
  // STAYS until the user dismisses it. `reconnected` swaps the panel body to it.
  let reconnected = false;

  // Per-grant live status (from the sidecar's read-only probes via
  // `engine://permission-status`, the SAME signal onboarding uses). Drives the
  // two permission rows so a user mid-recovery sees exactly WHICH grant is still
  // off — not just two undifferentiated buttons. Capture-health `live` (→
  // `reconnected`) remains the gate that means "both back + tap armed".
  let axGranted = false;
  let imGranted = false;

  // Re-check permissions by asking the engine to restart capture. If both grants
  // are back, the sidecar boots and its first heartbeat flips health to `live`
  // (handled in onHealth → confirm). If either is still revoked, the sidecar
  // exits again and nothing changes — we just try again on the next tick.
  const POLL_MS = 3000;
  function poll() {
    invoke("restart_capture").catch(() => {});
  }
  function startPolling() {
    if (pollTimer) clearInterval(pollTimer);
    reconnected = false;
    polling = true;
    poll(); // try immediately, don't wait a full interval
    pollTimer = setInterval(poll, POLL_MS);
  }
  function stopPolling() {
    polling = false;
    if (pollTimer) {
      clearInterval(pollTimer);
      pollTimer = null;
    }
  }

  // Each open-settings button drops this panel below System Settings (it's
  // always-on-top) so the user can actually reach the toggle, then opens the
  // matching pane. The panel stays open and surfaces again once capture returns.
  function openAccessibility() {
    invoke("reconnect_open_accessibility").catch(() => {});
  }
  function openInputMonitoring() {
    invoke("reconnect_open_input_monitoring").catch(() => {});
  }

  function dismiss() {
    stopPolling();
    reconnected = false;
    getCurrentWindow()
      .hide()
      .catch((e) => console.error("reconnect panel hide failed:", e));
  }

  function onHealth(state: string | undefined) {
    // Capture is back — both grants in effect and the tap armed (capture-health
    // is the source of truth). Don't vanish silently: bring the panel to the
    // front (System Settings is frontmost at this point) and show the
    // confirmation. It STAYS until the user dismisses it (Done / Esc / ✕) — no
    // auto-dismiss. The menu-bar status returns to "Jordan is active" on its own
    // (driven by capture-ui).
    if (!polling || state !== "live" || reconnected) return;
    stopPolling();
    reconnected = true;
    invoke("reconnect_surface").catch(() => {});
    tick().then(() => doneBtn?.focus());
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      dismiss();
      return;
    }
    // Minimal focus trap (separate window — App.svelte's trap doesn't cover it).
    if (event.key !== "Tab" || !rootEl) return;
    const t = Array.from(
      rootEl.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    ).filter((el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length));
    if (t.length === 0) return;
    const first = t[0];
    const last = t[t.length - 1];
    const active = document.activeElement as HTMLElement | null;
    if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    } else if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    }
  }

  let unlistenHealth: (() => void) | null = null;
  let unlistenPerms: (() => void) | null = null;
  let unlistenOpen: (() => void) | null = null;
  onMount(() => {
    listen<{ state: string }>("engine://capture-health", (e) => onHealth(e.payload?.state)).then(
      (un) => (unlistenHealth = un),
    );
    // Per-grant status for the two rows. While polling, each sidecar spawn emits
    // this even when it's about to exit for a missing grant, so each row reflects
    // its own permission within a poll cycle of the user flipping that switch.
    listen<{ accessibility: boolean; input_monitoring: boolean }>(
      "engine://permission-status",
      (e) => {
        axGranted = !!e.payload?.accessibility;
        imGranted = !!e.payload?.input_monitoring;
      },
    ).then((un) => (unlistenPerms = un));
    // Each time the panel is shown, (re)start the poll and land focus on the
    // primary action — the ring shows immediately (controls style :focus).
    listen("reconnect://open", () => {
      startPolling();
      tick().then(() => focusFirstPending());
    }).then((un) => (unlistenOpen = un));
  });
  onDestroy(() => {
    stopPolling();
    unlistenHealth?.();
    unlistenPerms?.();
    unlistenOpen?.();
  });
</script>

<svelte:window on:keydown={onKeydown} />

<div class="panel" bind:this={rootEl}>
  <button class="close" aria-label="Close" on:click={dismiss}>
    <svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round">
      <line x1="6" y1="6" x2="18" y2="18" /><line x1="18" y1="6" x2="6" y2="18" />
    </svg>
  </button>

  {#if reconnected}
    <div class="badge badge-ok" aria-hidden="true">
      <i class="ti ti-circle-check"></i>
    </div>
    <h1 class="title">Reconnected</h1>
    <p class="confirm">
      <i class="ti ti-circle-check confirm-icon" aria-hidden="true"></i>
      <span>Reconnected — Jordan is active again.</span>
    </p>
    <button class="btn-primary" bind:this={doneBtn} on:click={dismiss}>Done</button>
  {:else}
    <div class="badge" aria-hidden="true">
      <i class="ti ti-lock"></i>
    </div>
    <h1 class="title">Jordan needs permission again</h1>
    <p class="body">
      TypeAssist lost access to watch your typing — this can happen after an update. Turn the one
      that’s off back on and Jordan picks up right where it left off.
    </p>

    <!-- Per-grant rows so the user sees exactly which switch is still off — each
         reflects its OWN live status (blue check on grant, never green/red). -->
    <div class="perms">
      <div class="perm" class:granted={axGranted}>
        <span class="perm-status" aria-hidden="true">
          {#if axGranted}
            <i class="ti ti-circle-check"></i>
          {:else}
            <span class="perm-ring"></span>
          {/if}
        </span>
        <div class="perm-body">
          <span class="perm-label">Accessibility</span>
          <span class="perm-sub">Lets Jordan see which field you’re typing in, and stay out of password fields.</span>
          {#if axGranted}
            <span class="perm-allowed"><i class="ti ti-check" aria-hidden="true"></i>Allowed</span>
          {:else}
            <button class="perm-btn" bind:this={openBtn} on:click={openAccessibility}>
              Open Accessibility Settings
            </button>
          {/if}
        </div>
      </div>

      <div class="perm" class:granted={imGranted}>
        <span class="perm-status" aria-hidden="true">
          {#if imGranted}
            <i class="ti ti-circle-check"></i>
          {:else}
            <span class="perm-ring"></span>
          {/if}
        </span>
        <div class="perm-body">
          <span class="perm-label">Input Monitoring</span>
          <span class="perm-sub">Lets Jordan see the keys as you press them.</span>
          {#if imGranted}
            <span class="perm-allowed"><i class="ti ti-check" aria-hidden="true"></i>Allowed</span>
          {:else}
            <button class="perm-btn" bind:this={openImBtn} on:click={openInputMonitoring}>
              Open Input Monitoring Settings
            </button>
          {/if}
        </div>
      </div>
    </div>
    <p class="note">No need to come back here — it reconnects the moment both are on.</p>
  {/if}
</div>

<style>
  .panel {
    position: relative;
    box-sizing: border-box;
    height: 100vh;
    padding: 1.6rem 1.5rem 1.3rem;
    border-radius: 16px;
    background: canvas;
    border: 1px solid var(--hairline);
    color: canvastext;
    overflow: hidden;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
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

  .badge {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 2.4rem;
    height: 2.4rem;
    margin: 0.2rem 0 0.9rem;
    border-radius: 999px;
    background: color-mix(in srgb, var(--focus-ring) 14%, canvas);
    color: var(--focus-ring);
  }
  .badge .ti {
    font-size: 1.3rem;
  }
  /* Reconnected state — blue check, never green (mirrors onboarding step 2). */
  .badge-ok {
    background: color-mix(in srgb, var(--link) 14%, canvas);
    color: var(--link);
  }

  /* "you're all set" confirmation line — same shape as onboarding's .ob-granted. */
  .confirm {
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
    margin: 0 0 1.1rem;
    font-size: 0.95rem;
    line-height: 1.55;
    color: canvastext;
  }
  .confirm-icon {
    flex-shrink: 0;
    margin-top: 0.05rem;
    font-size: 1.15rem;
    color: var(--link);
  }

  .title {
    margin: 0 0 0.6rem;
    font-size: 1.3rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .body {
    margin: 0 0 1.1rem;
    font-size: 0.95rem;
    line-height: 1.55;
    color: var(--text-secondary);
  }

  /* Per-grant permission rows — one per required pane, each showing its own
     live status. Mirrors onboarding step 2's row layout. */
  .perms {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
    width: 100%;
    margin: 0 0 0.2rem;
  }
  .perm {
    display: flex;
    align-items: flex-start;
    gap: 0.7rem;
  }
  .perm-status {
    flex-shrink: 0;
    width: 1.5rem;
    height: 1.5rem;
    display: flex;
    align-items: center;
    justify-content: center;
    margin-top: 0.1rem;
  }
  /* Not granted: hollow grey ring. Granted: it fills to a blue check (the
     ti-circle-check) — never green, never red. */
  .perm-ring {
    width: 1.15rem;
    height: 1.15rem;
    border-radius: 999px;
    border: 2px solid color-mix(in srgb, canvastext 32%, canvas);
    box-sizing: border-box;
  }
  .perm-status .ti {
    font-size: 1.45rem;
    color: var(--link); /* app blue */
  }
  .perm-body {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    min-width: 0;
  }
  .perm-label {
    font-size: 0.98rem;
    font-weight: 600;
    color: canvastext;
  }
  .perm-sub {
    font-size: 0.88rem;
    line-height: 1.45;
    color: var(--text-secondary);
  }
  .perm-btn {
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
  .perm-btn:hover {
    background: color-mix(in srgb, var(--focus-ring) 88%, black);
  }
  .perm-btn:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  /* Granted: quiet, non-actionable "Allowed" with a small blue check. */
  .perm-allowed {
    align-self: flex-start;
    margin-top: 0.25rem;
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.9rem;
    font-weight: 500;
    color: var(--text-secondary);
  }
  .perm-allowed .ti {
    font-size: 1.05rem;
    color: var(--link); /* app blue */
  }

  .btn-primary {
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
  .btn-primary:hover {
    background: color-mix(in srgb, var(--focus-ring) 88%, black);
  }
  .btn-primary:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  .note {
    margin: 0.9rem 0 0;
    font-size: 0.85rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }
</style>
