// Small persisted UI settings, backed by localStorage (per the main window).
// These are intentionally lightweight stand-ins during the front-end rebuild:
//
//  - `userName`  — a saved setting; defaults to "Soumyo" until onboarding
//    writes the real name. Move to a Rust-backed setting when onboarding lands.
//  - `appState`  — the app-wide state (Day one / Building back / Fluent) that
//    every screen reflects. For now it's a value you flip BY HAND (the Home
//    preview switcher writes it, and it persists); automatic detection from
//    engine data is wired later. Nothing here touches the engine or captured
//    data — it's UI preview state only.

import { writable, type Writable } from "svelte/store";

export type AppState = "day1" | "building" | "fluent";

export const APP_STATES: { value: AppState; label: string }[] = [
  { value: "day1", label: "Day one" },
  { value: "building", label: "Building back" },
  { value: "fluent", label: "Fluent" },
];

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

export const userName = persisted<string>("ta.userName", "Soumyo");
export const appState = persisted<AppState>("ta.appState", "fluent");
