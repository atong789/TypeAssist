import "./app.css";
import App from "./App.svelte";
import PracticePanel from "./routes/PracticePanel.svelte";
import { getCurrentWindow } from "@tauri-apps/api/window";

// One build, two windows. The "practice" menu-bar panel mounts the calm
// Practice surface; every other window (the "main" app) mounts the full app.
// Branching here avoids a second Vite entry point.
const Component = getCurrentWindow().label === "practice" ? PracticePanel : App;

const app = new Component({
  target: document.getElementById("app")!,
});

export default app;
