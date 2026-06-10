import "./app.css";
// Tabler icon font — bundled locally from node_modules (NOT a CDN), so the
// app makes no network request for it (Principle #8). Vite bundles the woff2
// from the package at build time.
import "@tabler/icons-webfont/dist/tabler-icons.min.css";
import App from "./App.svelte";
import PracticePanel from "./routes/PracticePanel.svelte";
import ProgressPanel from "./routes/ProgressPanel.svelte";
import AllowlistPanel from "./routes/AllowlistPanel.svelte";
import Cue from "./routes/Cue.svelte";
import { getCurrentWindow } from "@tauri-apps/api/window";

// One build, several windows. The menu-bar panels and the correction cue mount
// their own small surfaces; every other window (the "main" app) mounts the full
// app. Branching here avoids a separate Vite entry point per window.
const label = getCurrentWindow().label;
const Component =
  label === "practice"
    ? PracticePanel
    : label === "progress"
      ? ProgressPanel
      : label === "allowlist"
        ? AllowlistPanel
        : label === "cue"
          ? Cue
          : App;

const app = new Component({
  target: document.getElementById("app")!,
});

export default app;
