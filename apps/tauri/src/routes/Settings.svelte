<!-- Settings — observing strip, warm-up language, state-aware Corrections, and the
     on-device data controls (Back up / Restore / Delete) with real confirm
     modals. Warm-up language reuses the existing locale pref (Practice reads it).
     The Corrections toggle is the SAME state as the menu-bar dropdown's — read +
     synced via corrections://state, written via set_correction_enabled (the
     engine is the sole writer). Everything stays on-device; no network.

     NOTE: "Back up" now opens the native save dialog and writes a real file
     (placeholder bundle contents — see TODO(data-layer) in lib/dataActions).
     Restore / Delete FILE I/O is still stubbed. The full screen + all three
     dialog flows are built and wired to clearly-named functions. -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import { fade } from "svelte/transition";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { loadSpellingPref, saveSpellingPref, type SpellingPref } from "../lib/locale";
  import Modal from "../lib/Modal.svelte";
  import RestoreDialog from "../lib/RestoreDialog.svelte";
  import {
    backUp,
    deleteEverything,
    pickAndPreviewBackup,
    type RestoreSummary,
  } from "../lib/dataActions";

  // ---- Warm-up language — reuses the existing locale pref (Practice reads it).
  let lang: SpellingPref = "system";
  let langLoaded = false;
  $: if (langLoaded) saveSpellingPref(lang);
  const LANGS: { value: SpellingPref; label: string }[] = [
    { value: "system", label: "Follow system" },
    { value: "en-US", label: "English (US)" },
    { value: "en-GB", label: "English (UK)" },
  ];
  // Grouped control → app-wide keyboard convention: ONE Tab stop (roving
  // tabindex); Left/Right (and Home/End) switch WITHIN it. Tab moves between
  // controls, not between the three options.
  let segEls: HTMLButtonElement[] = [];
  function onSegKeydown(event: KeyboardEvent, i: number) {
    let ni: number;
    switch (event.key) {
      case "ArrowRight":
        ni = (i + 1) % LANGS.length;
        break;
      case "ArrowLeft":
        ni = (i - 1 + LANGS.length) % LANGS.length;
        break;
      case "Home":
        ni = 0;
        break;
      case "End":
        ni = LANGS.length - 1;
        break;
      default:
        return;
    }
    event.preventDefault();
    lang = LANGS[ni].value;
    segEls[ni]?.focus();
  }

  // ---- Corrections master gate — the SAME on/off as the menu-bar toggle and
  // Today's offer card (one source of truth). Off by default; the engine never
  // flips it on. Always available — no unlock/readiness gate. Read on mount +
  // sync via corrections://state; write via set_correction_enabled.
  interface AllowListState {
    correction_enabled: boolean;
  }
  let correctionEnabled = false;
  function toggleCorrections() {
    const next = !correctionEnabled;
    correctionEnabled = next; // optimistic; the engine echo confirms
    invoke("set_correction_enabled", { enabled: next }).catch(() => {});
  }

  onMount(() => {
    lang = loadSpellingPref();
    langLoaded = true;

    invoke<AllowListState>("read_allow_list")
      .then((al) => (correctionEnabled = !!al?.correction_enabled))
      .catch(() => {});
    invoke("request_allow_list").catch(() => {});
    const off = listen<AllowListState>("corrections://state", (e) => {
      correctionEnabled = !!e.payload?.correction_enabled;
    });
    return () => {
      off.then((f) => f());
    };
  });

  // ---- Data actions — Back up / Restore / Delete, wired to the Rust data layer. ----
  let restoreOpen = false;
  let deleteOpen = false;
  let lastBackup: string | null = null;
  let deleteCancelBtn: HTMLButtonElement | undefined;

  // The picked-and-previewed backup, handed to the guarded confirm so it can
  // describe the ACTUAL file (date + contents) instead of warning blind.
  let restorePath = "";
  let restoreSummary: RestoreSummary | null = null;
  // A transient error if a picked file is invalid / a newer schema (shown inline
  // by the data actions, not in a dialog that never opened).
  let restoreError = "";

  // Quiet, self-dismissing confirmation — a blue check (NEVER green, per locked
  // design) that fades on its own after ~5s. Shared by Back up + Restore.
  let confirmMsg = "";
  let confirmTimer: ReturnType<typeof setTimeout> | undefined;
  function flashConfirm(msg: string) {
    confirmMsg = msg;
    clearTimeout(confirmTimer);
    confirmTimer = setTimeout(() => (confirmMsg = ""), 5000);
  }

  // The main "Back up" button + the delete-guardrail backup record the filename
  // locally so the delete dialog can show "Backed up — …".
  async function doBackup() {
    const name = await backUp();
    lastBackup = name;
    // Only confirm on a real write — a cancelled save dialog shows nothing.
    if (name) flashConfirm("Backed up");
  }

  // Restore, reordered: pick the file → preview (read-only) → THEN the guarded
  // confirm describes that file → apply on confirm. A cancelled pick shows
  // nothing; a bad/newer file surfaces inline without ever opening the confirm.
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
    restoreOpen = false;
    restoreSummary = null;
    flashConfirm("Restored — your data’s back");
  }
  function openDelete(e: MouseEvent) {
    (e.currentTarget as HTMLElement | null)?.focus();
    deleteOpen = true;
  }
  // "Back up now" inside the delete modal removes its own button (the guardrail
  // flips to "Backed up — …"), so move focus to a stable control in the dialog.
  async function backupNow() {
    await doBackup();
    tick().then(() => deleteCancelBtn?.focus());
  }

  async function confirmDelete() {
    // Erases all on-device learning + history and resets Corrections to off —
    // the engine echoes corrections://state (which flips the toggle) when done.
    await deleteEverything();
    lastBackup = null;
    deleteOpen = false;
  }
</script>

<header class="screen-header"><h1>Settings</h1></header>

<!-- 1) Observing strip -->
<div class="observe">
  <i class="ti ti-eye" aria-hidden="true"></i>
  <span>Observing your typing · Everything stays on this Mac</span>
</div>

<!-- 2) Warm-up language -->
<section class="group">
  <h2 class="group-label">Warm-up language</h2>
  <div class="segmented" role="radiogroup" aria-label="Warm-up language">
    {#each LANGS as o, i}
      <button
        class="seg-btn"
        class:on={lang === o.value}
        role="radio"
        aria-checked={lang === o.value}
        tabindex={lang === o.value ? 0 : -1}
        bind:this={segEls[i]}
        on:click={() => (lang = o.value)}
        on:keydown={(e) => onSegKeydown(e, i)}>{o.label}</button
      >
    {/each}
  </div>
</section>

<!-- 3) Corrections — one always-available On/Off (no readiness gate). Same state
     as the menu-bar toggle and Today's offer card. -->
<section class="group">
  <div class="row">
    <div class="row-text">
      <div class="row-title">Corrections</div>
      <p class="row-desc">
        When this is on, I’ll suggest a fix right after a word — tap Shift to accept, Esc to undo.
        The fixes I’m still learning will join in over time. You’re always the one typing.
      </p>
    </div>
    <button
      class="switch"
      class:on={correctionEnabled}
      role="switch"
      aria-checked={correctionEnabled}
      aria-label="Corrections"
      on:click={toggleCorrections}
    >
      <span class="knob" aria-hidden="true"></span>
    </button>
  </div>
</section>

<!-- 4) Your Typing Data — one caption above the button; Restore / Delete sit as
     two discreet links directly beneath it, on one row. -->
<section class="group">
  <h2 class="group-label">Your Typing Data</h2>
  <p class="group-sub">
    A copy of what TenCalmDigits has learned about your hands — kept only on this Mac. Saves a
    file you can keep anywhere: a drive, a cloud folder, or a new Mac.
  </p>
  <div class="backup-row">
    <button class="btn-primary" on:click={doBackup}>
      <i class="ti ti-download" aria-hidden="true"></i> Back up
    </button>
    <!-- Quiet, self-dismissing confirmation — blue check, NOT green; fades ~5s. -->
    {#if confirmMsg}
      <span class="data-confirm" out:fade={{ duration: 350 }}>
        <i class="ti ti-circle-check" aria-hidden="true"></i> {confirmMsg}
      </span>
    {/if}
  </div>
  {#if restoreError}
    <p class="data-error" role="alert" out:fade={{ duration: 350 }}>
      <i class="ti ti-alert-circle" aria-hidden="true"></i> {restoreError}
    </p>
  {/if}
  <div class="data-links">
    <button class="link" on:click={openRestore}>Restore from a backup</button>
    <button class="link danger" on:click={openDelete}>Delete everything</button>
  </div>
</section>

<!-- Restore — the guarded confirm, opened only AFTER a file is picked + previewed
     so it describes the actual backup (date + contents). -->
{#if restoreOpen && restoreSummary}
  <RestoreDialog
    path={restorePath}
    summary={restoreSummary}
    on:close={() => (restoreOpen = false)}
    on:restored={onRestored}
  />
{/if}

<!-- Delete — confirm modal -->
{#if deleteOpen}
  <Modal titleId="delete-title" on:cancel={() => (deleteOpen = false)}>
    <div class="dlg-head">
      <i class="ti ti-alert-triangle warn-icon" aria-hidden="true"></i>
      <h2 id="delete-title" class="dlg-title">Delete everything I’ve learned?</h2>
    </div>
    <p class="dlg-body">
      This permanently erases everything I’ve learned — every pattern and day of history — and
      turns Corrections back off. You’ll start fresh, like the first time you opened the app. This
      can’t be undone.
    </p>
    <div class="guardrail">
      {#if lastBackup}
        <span class="ok"><i class="ti ti-circle-check" aria-hidden="true"></i> Backed up — {lastBackup}</span>
      {:else}
        <span class="warn">Not backed up yet</span>
        <button class="link" on:click={backupNow}>Back up now</button>
      {/if}
    </div>
    <div class="dlg-actions">
      <button class="btn-danger" on:click={confirmDelete}>Delete everything</button>
      <div class="dlg-spacer"></div>
      <button
        class="btn-primary"
        data-autofocus
        bind:this={deleteCancelBtn}
        on:click={() => (deleteOpen = false)}>Cancel</button
      >
    </div>
  </Modal>
{/if}

<style>
  /* ---- observing strip ---- */
  .observe {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    max-width: 34rem;
    margin: 0.25rem 0 0.5rem;
    padding: 0.6rem 0.85rem;
    border-radius: 8px;
    border: 1px solid var(--hairline);
    background: color-mix(in srgb, canvastext 3%, canvas);
    color: var(--text-secondary);
    font-size: 0.9rem;
  }
  .observe .ti {
    font-size: 1.05rem;
  }

  /* ---- groups ---- */
  .group {
    max-width: 34rem;
    margin-top: 1.5rem;
  }
  .group-label {
    margin: 0 0 0.6rem;
    font-size: 0.82rem;
    font-weight: 600;
    color: var(--text-secondary);
  }
  .group-sub {
    margin: -0.3rem 0 0.85rem;
    font-size: 0.9rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }

  /* ---- segmented control (warm-up language) ---- */
  .segmented {
    display: inline-flex;
    gap: 0.3rem;
    padding: 0.25rem;
    border: 1px solid var(--hairline);
    border-radius: 10px;
  }
  .seg-btn {
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
  .seg-btn.on {
    background: color-mix(in srgb, var(--focus-ring) 16%, canvas);
    color: canvastext;
  }
  .seg-btn:hover:not(.on) {
    background: color-mix(in srgb, canvastext 5%, canvas);
  }
  .seg-btn:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }

  /* ---- corrections row ---- */
  .row {
    display: flex;
    align-items: flex-start;
    gap: 1rem;
    padding: 0.9rem 1rem;
    border: 1px solid var(--hairline);
    border-radius: 12px;
  }
  .row-title {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    font-weight: 600;
  }
  .row-desc {
    margin: 0.4rem 0 0;
    font-size: 0.9rem;
    line-height: 1.55;
    color: var(--text-secondary);
  }
  .row-text {
    flex: 1;
    min-width: 0;
  }

  /* ---- toggle switch ---- */
  .switch {
    position: relative;
    flex-shrink: 0;
    width: 42px;
    height: 24px;
    margin-top: 0.1rem;
    padding: 0;
    border: none;
    border-radius: 12px;
    background: color-mix(in srgb, canvastext 22%, canvas);
    cursor: pointer;
  }
  .switch .knob {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: #fff;
  }
  .switch.on {
    background: var(--focus-ring);
  }
  .switch.on .knob {
    left: 21px;
  }
  .switch:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  @media (prefers-reduced-motion: no-preference) {
    .switch,
    .switch .knob {
      transition:
        background-color 120ms ease,
        left 120ms ease;
    }
  }

  /* ---- buttons + links ---- */
  .btn-primary,
  .btn-danger {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    min-height: 38px;
    padding: 0.5rem 1.05rem;
    font: inherit;
    font-size: 0.92rem;
    font-weight: 500;
    border-radius: 9px;
    cursor: pointer;
  }
  .btn-primary {
    border: none;
    background: var(--focus-ring);
    color: #fff;
  }
  .btn-primary:hover {
    background: color-mix(in srgb, var(--focus-ring) 88%, black);
  }
  /* Destructive action — NEUTRAL muted outline (never red; the app avoids red/green
     for colour-blind safety). Destructiveness is carried by the amber alert icon,
     the copy, the confirm step, and Cancel being the highlighted blue default. */
  .btn-danger {
    background: transparent;
    border: 1px solid color-mix(in srgb, canvastext 28%, canvas);
    color: var(--text-secondary);
  }
  .btn-danger:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
    color: canvastext;
  }
  .btn-primary:focus,
  .btn-danger:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  /* Back up button + its self-dismissing confirmation, on one baseline row. */
  .backup-row {
    display: flex;
    align-items: center;
    gap: 0.7rem;
  }
  /* Quiet confirmation — the app BLUE accent + a check, never green (locked
     design: green is reserved out of the colour-blind-safe palette). */
  .data-confirm {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.9rem;
    font-weight: 500;
    color: var(--focus-ring);
  }
  .data-error {
    display: flex;
    align-items: flex-start;
    gap: 0.4rem;
    margin: 0.65rem 0 0;
    font-size: 0.88rem;
    line-height: 1.5;
    color: #d23f3f;
  }
  /* Discreet text links (Restore / Delete) — directly beneath the Back up
     button, on one row. */
  .data-links {
    display: flex;
    gap: 1.4rem;
    margin-top: 1rem;
  }
  .link {
    padding: 0.3rem 0.1rem;
    font: inherit;
    font-size: 0.9rem;
    color: var(--link);
    background: transparent;
    border: none;
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
  /* Neutral muted, not red — destructiveness is made clear by the confirm dialog,
     not by colour alone. */
  .link.danger {
    color: var(--text-secondary);
  }
  .link:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
    border-radius: 5px;
  }

  /* ---- dialog content ---- */
  .dlg-head {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    margin-bottom: 0.6rem;
  }
  /* Amber caution — never red (colour-blind-safe). */
  .warn-icon {
    font-size: 1.3rem;
    color: var(--warning);
  }
  .dlg-title {
    margin: 0 0 0.6rem;
    font-size: 1.1rem;
    font-weight: 600;
  }
  .dlg-head .dlg-title {
    margin: 0;
  }
  .dlg-body {
    margin: 0;
    font-size: 0.92rem;
    line-height: 1.55;
    color: var(--text-secondary);
  }
  .guardrail {
    display: flex;
    align-items: center;
    gap: 0.7rem;
    margin-top: 1rem;
    padding: 0.6rem 0.8rem;
    border-radius: 9px;
    background: color-mix(in srgb, canvastext 4%, canvas);
    font-size: 0.86rem;
  }
  .guardrail .warn {
    color: var(--text-secondary);
  }
  /* Confirmation reads in the blue accent, never green (colour-blind-safe). */
  .guardrail .ok {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--link);
  }
  .dlg-actions {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 1.3rem;
  }
  .dlg-spacer {
    flex: 1;
  }
</style>
