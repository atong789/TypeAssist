// On-device data actions — Back up / Restore / Delete. ONE place for the stubs so
// the data-layer pass wires a single source. Everything stays on this Mac; there
// is no network path here (Principle #8). The real file I/O is TODO(data-layer).

export function backupFilename(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  return `TenCalmDigits-backup-${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}.tabackup`;
}

// Export ALL learned data to one dated file via the macOS save dialog. Returns the
// chosen filename. Async so callers can AWAIT the save before chaining the next
// step (e.g. the restore picker) once the real I/O lands.
export async function backUp(): Promise<string> {
  // TODO(data-layer): await tauri-plugin-dialog save() → write a bundle of the
  // on-device store (motor_map.json, word_patterns.json, key scores, …) to the
  // chosen path. No cloud — the save dialog already exposes iCloud / USB / folders.
  const name = backupFilename();
  console.info("[stub] back up →", name);
  return name;
}

// Pick a backup and REPLACE the current data with it (never merge — merging would
// double-count the statistics). After a real restore the app reflects whatever
// state the backup holds.
export async function restoreFromBackup(): Promise<void> {
  // TODO(data-layer): open dialog → read the bundle → atomically replace the live
  // store, then have the engine reload so every screen reflects the backup.
  console.info("[stub] restore (replace, never merge)");
}

// Erase ALL learned data on disk so the app truly starts fresh. Afterwards every
// screen reflects the now-empty store on its own (sparse), so there is no state
// flag to reset.
export function deleteEverything(): void {
  // TODO(data-layer): erase motor map, word patterns, snapshots, key scores, and
  // allow-list patterns on disk so nothing learned survives.
  console.info("[stub] delete everything");
}
