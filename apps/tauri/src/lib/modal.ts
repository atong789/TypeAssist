import { writable } from "svelte/store";

// True while a modal dialog is open. App.svelte's global focus trap defers to
// the modal's own focus trap while this is set, so the two don't fight over Tab.
export const modalOpen = writable(false);
