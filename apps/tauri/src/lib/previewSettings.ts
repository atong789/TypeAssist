// Small persisted UI settings, backed by localStorage (per the main window).
// These are intentionally lightweight stand-ins during the front-end rebuild:
//
//  - `userName`  — a saved setting; defaults to blank (no hardcoded name)
//    until onboarding writes the real one. TODO: move to a Rust-backed
//    setting when onboarding lands.
//
// NOTE: the former app-wide three-state model (Day one / Building back / Fluent)
// has been RETIRED. There are now only two phases — onboarding (internal name
// "DayOne", ends the moment the user reaches the menu and never recurs) and the
// running app, where each screen simply reflects current data (sparse early,
// fuller over time). No state gates what screens show.

import { writable, type Writable } from "svelte/store";

/** A writable store mirrored to localStorage (JSON), safe if storage is absent. */
function persisted<T>(key: string, fallback: T): Writable<T> {
  let initial = fallback;
  try {
    const raw = localStorage.getItem(key);
    if (raw !== null) initial = JSON.parse(raw) as T;
  } catch {
    /* storage unavailable — fall back to the default */
  }
  const store = writable<T>(initial);
  store.subscribe((v) => {
    try {
      localStorage.setItem(key, JSON.stringify(v));
    } catch {
      /* ignore write failures (private mode, etc.) */
    }
  });
  return store;
}

export const userName = persisted<string>("ta.userName", "");

// DEV-ONLY override for Today's data-driven state, so a Mac with lots of real
// data can still preview the empty / welcome screen without touching that data.
// "auto" = follow real data (the only value a release ever sees — Today reads
// this behind an `import.meta.env.DEV` guard, and the only writer is the dev
// state-preview overlay, which is excluded from release builds). Persisted so it
// survives Vite HMR reloads mid-testing.
export type DevTodayState = "auto" | "empty" | "normal";
export const devTodayState = persisted<DevTodayState>("ta.devTodayState", "auto");

// DEV-ONLY override for the Progress map's day-one / no-data state, so a Mac with
// real typing data can still preview it. "empty" forces an empty key-score +
// corrections view (whole keyboard faint + the no-data Jordan caption) WITHOUT
// touching real data. Read in Progress.svelte behind an `import.meta.env.DEV`
// guard; only the dev overlay writes it, so a release always sees "auto".
export type DevProgressState = "auto" | "empty";
export const devProgressState = persisted<DevProgressState>("ta.devProgressState", "auto");

// One-shot flag: the user has declined Today's Corrections offer card. Once set,
// the offer line never reappears (Corrections stays available in the menu and
// Settings). Cleared only if the user resets via "Replay onboarding" / delete.
export const correctionsOfferDismissed = persisted<boolean>(
  "ta.correctionsOfferDismissed",
  false,
);

// First-run onboarding shows once, before the shell, until completed. Persisted so
// it never reappears after the user finishes (or restores from a backup). The dev
// Preview switcher exposes a "Replay onboarding" reset to review it again.
export const onboarded = persisted<boolean>("ta.onboarded", false);
