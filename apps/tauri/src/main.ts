import "./app.css";
import App from "./App.svelte";
import PracticePanel from "./routes/PracticePanel.svelte";
import ProgressPanel from "./routes/ProgressPanel.svelte";
import { getCurrentWindow } from "@tauri-apps/api/window";

// One build, several windows. The "practice" and "progress" menu-bar panels
// mount their own calm surfaces; every other window (the "main" app) mounts the
// full app. Branching here avoids a separate Vite entry point per window.
const label = getCurrentWindow().label;
const Component =
  label === "practice"
    ? PracticePanel
    : label === "progress"
      ? ProgressPanel
      : App;

const app = new Component({
  target: document.getElementById("app")!,
});

export default app;
