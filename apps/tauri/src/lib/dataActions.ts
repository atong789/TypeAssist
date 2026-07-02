// On-device data actions — Back up / Restore / Delete. ONE place that wires the
// Settings flow to the Rust data layer. Everything stays on this Mac; there is no
// network path here (Principle #8). All disk I/O happens in Rust:
//   • Back up  → native SAVE dialog → `create_backup` reads every learned/history
//                store and writes one OS-portable, versioned `.typingbackup` file.
//   • Restore  → native OPEN dialog → `restore_preview` validates (read-only),
//                then `apply_restore` REPLACES current learning via the engine
//                (the sole writer of ~/.typeassist). Never merges.
//   • Delete   → `delete_all_data` erases everything via the engine → fresh start.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save, open } from "@tauri-apps/plugin-dialog";

// Brand-free, rename-clean extension (no "ta"/"tcd" initials baked in).
const BACKUP_EXT = "typingbackup";

export function backupFilename(): string {
  const d = new Date();
  const p = (n: number) => String(n).padStart(2, "0");
  // Local 24-hour HHMM time is ALWAYS appended so multiple same-day backups
  // get distinct suggested names and never collide / overwrite each other.
  const date = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
  const time = `${p(d.getHours())}${p(d.getMinutes())}`;
  return `TenCalmDigits-backup-${date}-${time}.${BACKUP_EXT}`;
}

const BACKUP_FILTER = [{ name: "TenCalmDigits backup", extensions: [BACKUP_EXT] }];

// Just the filename (not the full chosen path) for the success label.
const baseName = (path: string) => path.split(/[\\/]/).pop() ?? backupFilename();

// Export ALL learned data to one dated file via the macOS save dialog. The Rust
// `create_backup` command reads the on-device stores and writes the bundle.
// Returns the chosen filename, or null if the user cancels (no success UI).
export async function backUp(): Promise<string | null> {
  const path = await save({
    defaultPath: backupFilename(),
    title: "Back up your TenCalmDigits data",
    filters: BACKUP_FILTER,
  });
  if (!path) return null; // user dismissed the save panel
  await invoke("create_backup", { path });
  return baseName(path);
}

// What `restore_preview` returns — a read-only glance at a backup file so the
// guarded confirm can describe the ACTUAL file (its date + contents) before
// anything is replaced. Mirrors the Rust `RestoreSummary`.
export interface RestoreSummary {
  schema_version: number;
  created_ms: number;
  motor_keys: number;
  word_patterns: number;
  vocab_words: number;
  history_days: number;
}

export type PreviewResult =
  | { status: "ok"; path: string; summary: RestoreSummary }
  | { status: "cancelled" }
  | { status: "error"; message: string };

// Step 1 of restore: pick a file, then validate it read-only (version +
// structure). The engine never touches disk here — a bad / newer-schema file is
// refused BEFORE the guarded confirm shows, and the summary describes the file so
// the confirm isn't blind.
export async function pickAndPreviewBackup(): Promise<PreviewResult> {
  const picked = await open({
    multiple: false,
    title: "Choose a TenCalmDigits backup",
    filters: BACKUP_FILTER,
  });
  const path = Array.isArray(picked) ? picked[0] : picked;
  if (!path) return { status: "cancelled" };
  try {
    const summary = await invoke<RestoreSummary>("restore_preview", { path });
    return { status: "ok", path, summary };
  } catch (e) {
    return { status: "error", message: String(e) };
  }
}

// The engine emits `data://restored` { ok, message } once it has applied (or
// rejected) the file. We arm the listener BEFORE invoking so we never miss it.
function awaitRestoreOutcome(): Promise<{ ok: boolean; message: string }> {
  return new Promise((resolve) => {
    let stop: (() => void) | undefined;
    const timer = setTimeout(() => {
      stop?.();
      resolve({ ok: false, message: "Restore timed out." });
    }, 15000);
    listen<{ ok: boolean; message: string }>("data://restored", (e) => {
      clearTimeout(timer);
      stop?.();
      resolve(e.payload);
    }).then((s) => {
      stop = s;
    });
  });
}

// Step 2 of restore: REPLACE current learning with the already-previewed backup
// (never merge). The engine (sole writer) does the atomic replace and reports the
// outcome on `data://restored`.
export async function applyRestore(path: string): Promise<{ ok: boolean; message: string }> {
  const outcome = awaitRestoreOutcome();
  try {
    await invoke("apply_restore", { path });
  } catch (e) {
    return { ok: false, message: String(e) };
  }
  const r = await outcome;
  return r.ok ? { ok: true, message: r.message } : { ok: false, message: r.message || "Restore failed." };
}

// Erase ALL learned data on disk (and reset Corrections to off) so the app truly
// starts fresh — equivalent to first launch. Runs through the engine (sole
// writer); afterwards every screen reflects the now-empty store on its own.
export async function deleteEverything(): Promise<void> {
  await invoke("delete_all_data");
}
