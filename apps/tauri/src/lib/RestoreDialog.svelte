<!-- Restore-from-backup dialog — the SINGLE guarded restore flow, shared by
     Settings and first-run Onboarding. Restoring REPLACES current learning, so it
     warns first and offers to back up the current state before proceeding. Cancel
     is the safe, highlighted blue default (this is a data-replacing action).

     Dispatches `restored` after a (stubbed) restore completes, and `close` on
     cancel. The real file I/O lives in lib/dataActions.ts (TODO(data-layer)). -->
<script lang="ts">
  import { createEventDispatcher } from "svelte";
  import Modal from "./Modal.svelte";
  import { backUp, restoreFromBackup } from "./dataActions";

  const dispatch = createEventDispatcher<{ close: void; restored: void }>();

  async function justRestore() {
    await restoreFromBackup();
    dispatch("restored");
  }
  // Wait for the backup to finish, THEN flow straight into the restore — never
  // stop after the backup.
  async function backupThenRestore() {
    await backUp();
    await restoreFromBackup();
    dispatch("restored");
  }
</script>

<Modal titleId="restore-title" on:cancel={() => dispatch("close")}>
  <h2 id="restore-title" class="dlg-title">Restore from a backup?</h2>
  <p class="dlg-body">
    Restoring replaces your current learning with the backup. Anything learned since the backup
    will be lost.
  </p>
  <div class="dlg-actions">
    <button class="btn-ghost" on:click={backupThenRestore}>Back up current first</button>
    <div class="dlg-spacer"></div>
    <button class="btn-ghost" on:click={justRestore}>Restore</button>
    <button class="btn-primary" data-autofocus on:click={() => dispatch("close")}>Cancel</button>
  </div>
</Modal>

<style>
  .dlg-title {
    margin: 0 0 0.6rem;
    font-size: 1.1rem;
    font-weight: 600;
  }
  .dlg-body {
    margin: 0;
    font-size: 0.92rem;
    line-height: 1.55;
    color: var(--text-secondary);
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
  .btn-primary:focus,
  .btn-ghost:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
</style>
