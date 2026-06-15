<!-- Reconnect panel — its own webview window (label "reconnect"), shown by the
     menu-bar "Reconnect…" recovery item when capture has stopped because the
     Accessibility permission was revoked (e.g. after an update). Reuses
     onboarding step 2's visual language (privacy/permission framing + the one
     "Open Accessibility settings" action) but WITHOUT the Back/Next flow.

     AUTO-RESUME: while open it polls the permission by asking the engine to
     restart capture every few seconds; a successful re-arm fires
     `engine://capture-health` → `live`, and we dismiss ourselves. The user
     never has to come back here. Rust shows + centres the window and does NOT
     hide it on blur (the user must leave to flip the switch in System
     Settings), so the panel survives that round-trip. -->
<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";

  let rootEl: HTMLElement;
  let openBtn: HTMLButtonElement | undefined;

  // We only react to capture coming back (and only keep respawning the sidecar)
  // while the panel is actually open — `polling` gates both, so a `live` event
  // during normal operation never hides an already-hidden window, and we don't
  // churn the sidecar in the background.
  let polling = false;
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  // Re-check the permission by asking the engine to restart capture. If the
  // grant is back, the sidecar boots and its first heartbeat flips health to
  // `live` (handled in onHealth → dismiss). If it's still revoked, the sidecar
  // exits again and nothing changes — we just try again on the next tick.
  const POLL_MS = 3000;
  function poll() {
    invoke("restart_capture").catch(() => {});
  }
  function startPolling() {
    if (pollTimer) clearInterval(pollTimer);
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

  function openAccessibility() {
    invoke("open_accessibility_settings").catch(() => {});
  }

  function dismiss() {
    stopPolling();
    getCurrentWindow()
      .hide()
      .catch((e) => console.error("reconnect panel hide failed:", e));
  }

  function onHealth(state: string | undefined) {
    // Capture is back — re-armed and live. Auto-resume: dismiss; the menu-bar
    // status returns to "Jordan is active" on its own (driven by capture-ui).
    if (polling && state === "live") dismiss();
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
  let unlistenOpen: (() => void) | null = null;
  onMount(() => {
    listen<{ state: string }>("engine://capture-health", (e) => onHealth(e.payload?.state)).then(
      (un) => (unlistenHealth = un),
    );
    // Each time the panel is shown, (re)start the poll and land focus on the
    // primary action — the ring shows immediately (controls style :focus).
    listen("reconnect://open", () => {
      startPolling();
      tick().then(() => openBtn?.focus());
    }).then((un) => (unlistenOpen = un));
  });
  onDestroy(() => {
    stopPolling();
    unlistenHealth?.();
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

  <div class="badge" aria-hidden="true">
    <i class="ti ti-lock"></i>
  </div>
  <h1 class="title">Jordan needs permission again</h1>
  <p class="body">
    TypeAssist lost access to watch your typing — this can happen after an update. Turn it back on
    under Accessibility, and Jordan picks up right where it left off.
  </p>
  <button class="btn-primary" bind:this={openBtn} on:click={openAccessibility}>
    Open Accessibility Settings
  </button>
  <p class="note">No need to come back here — it reconnects the moment you flip the switch.</p>
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
