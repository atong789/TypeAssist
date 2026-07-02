<!-- Restore-from-backup confirm — shown AFTER a file is picked and validated
     (read-only preview), so it describes the ACTUAL backup (its date + contents)
     rather than warning blind. Restoring REPLACES current learning, so it warns
     and offers to back up the current state first. Cancel is the safe, highlighted
     blue default. Dispatches `restored` on success, `close` on cancel/failure to
     proceed. The atomic replace runs in the engine (the sole writer). -->
<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import Modal from "./Modal.svelte";
  import { applyRestore, backUp, type RestoreSummary } from "./dataActions";

  // The picked file + its read-only preview (set by the caller before opening).
  export let path: string;
  export let summary: RestoreSummary;

  const dispatch = createEventDispatcher<{ close: void; restored: void }>();

  let busy = false;
  let errorMsg = "";

  // Describe the actual file: "June 29, 2026" when the backup carries a
  // timestamp, else an honest fallback for an older backup that predates it.
  $: dateStr =
    summary.created_ms > 0
      ? new Date(summary.created_ms).toLocaleDateString(undefined, {
          year: "numeric",
          month: "long",
          day: "numeric",
        })
      : "an earlier session";

  // A short, plain contents line — only the parts that have something to say.
  $: contents = (() => {
    const parts: string[] = [];
    if (summary.word_patterns > 0)
      parts.push(`${summary.word_patterns} correction${summary.word_patterns === 1 ? "" : "s"}`);
    if (summary.history_days > 0)
      parts.push(`${summary.history_days} day${summary.history_days === 1 ? "" : "s"} of history`);
    return parts.join(" · ");
  })();

  async function proceed(backupFirst: boolean) {
    busy = true;
    errorMsg = "";
    try {
      if (backupFirst) await backUp();
      const r = await applyRestore(path);
      if (r.ok) dispatch("restored");
      else errorMsg = r.message;
    } finally {
      busy = false;
    }
  }
</script>

<Modal titleId="restore-title" on:cancel={() => dispatch("close")}>
  <h2 id="restore-title" class="dlg-title">Restore this backup?</h2>

  <div class="summary">
    <div class="summary-date"><i class="ti ti-clock-hour-4" aria-hidden="true"></i> Backup from {dateStr}</div>
    {#if contents}
      <div class="summary-contents">{contents}</div>
    {/if}
  </div>

  <p class="dlg-body">
    This replaces your current learning with this backup. Anything learned since it was made will
    be lost.
  </p>

  {#if errorMsg}
    <p class="dlg-error" role="alert">
      <i class="ti ti-alert-circle" aria-hidden="true"></i>
      {errorMsg}
    </p>
  {/if}

  <div class="dlg-actions">
    <button class="btn-ghost" on:click={() => proceed(true)} disabled={busy}>
      Back up current first
    </button>
    <div class="dlg-spacer"></div>
    <button class="btn-ghost" on:click={() => proceed(false)} disabled={busy}>
      {busy ? "Restoring…" : "Restore"}
    </button>
    <button class="btn-primary" data-autofocus on:click={() => dispatch("close")}>Cancel</button>
  </div>
</Modal>

<style>
  .dlg-title {
    margin: 0 0 0.6rem;
    font-size: 1.1rem;
    font-weight: 600;
  }
  /* The picked file's date + contents — what's actually being restored. */
  .summary {
    margin: 0 0 0.75rem;
    padding: 0.6rem 0.8rem;
    border-radius: 9px;
    border: 1px solid var(--hairline);
    background: color-mix(in srgb, canvastext 3%, canvas);
  }
  .summary-date {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.95rem;
    font-weight: 600;
  }
  .summary-contents {
    margin-top: 0.2rem;
    font-size: 0.88rem;
    color: var(--text-secondary);
  }
  .dlg-body {
    margin: 0;
    font-size: 0.92rem;
    line-height: 1.55;
    color: var(--text-secondary);
  }
  /* Inline error (bad / newer-schema file, or a failed apply) — a genuine error
     surface, distinct from the never-red slip palette. */
  .dlg-error {
    display: flex;
    align-items: flex-start;
    gap: 0.4rem;
    margin: 0.75rem 0 0;
    font-size: 0.9rem;
    line-height: 1.5;
    color: #d23f3f;
  }
  .dlg-error .ti {
    margin-top: 0.1rem;
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
  .btn-primary,
  .btn-ghost {
    display: inline-flex;
    align-items: center;
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
  .btn-ghost {
    background: transparent;
    border: 1px solid color-mix(in srgb, canvastext 28%, canvas);
    color: canvastext;
  }
  .btn-ghost:hover {
    background: color-mix(in srgb, canvastext 6%, canvas);
  }
  .btn-ghost:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .btn-primary:focus,
  .btn-ghost:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
</style>
