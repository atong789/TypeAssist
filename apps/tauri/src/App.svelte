<script lang="ts">
  import Home from "./routes/Home.svelte";
  import Settings from "./routes/Settings.svelte";
  import Practice from "./routes/Practice.svelte";
  import WarmUp from "./routes/WarmUp.svelte";
  import Today from "./routes/Today.svelte";

  type Route = "home" | "today" | "warmup" | "practice" | "settings";
  let route: Route = "home";

  // Route changes requested by a child view (e.g. Home's "Start" → Warm-up).
  // The cast lives here in the script block — TS isn't valid in markup expressions.
  function handleNavigate(event: CustomEvent<string>) {
    route = event.detail as Route;
  }
</script>

<main>
  <nav aria-label="Primary">
    <button class:active={route === "home"} on:click={() => (route = "home")}>Home</button>
    <button class:active={route === "today"} on:click={() => (route = "today")}>Today</button>
    <button class:active={route === "warmup"} on:click={() => (route = "warmup")}>Warm-up</button>
    <button class:active={route === "practice"} on:click={() => (route = "practice")}>Practice</button>
    <button class:active={route === "settings"} on:click={() => (route = "settings")}>Settings</button>
  </nav>

  <section>
    {#if route === "home"}<Home on:navigate={handleNavigate} />
    {:else if route === "today"}<Today />
    {:else if route === "warmup"}<WarmUp />
    {:else if route === "practice"}<Practice />
    {:else if route === "settings"}<Settings />
    {/if}
  </section>
</main>

<style>
  main {
    display: grid;
    grid-template-columns: 220px 1fr;
    min-height: 100vh;
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    padding: 1rem;
    border-right: 1px solid color-mix(in srgb, canvastext 12%, transparent);
  }
  nav button {
    text-align: left;
    padding: 0.5rem 0.75rem;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 6px;
    font: inherit;
    color: inherit;
    cursor: pointer;
  }
  nav button.active {
    background: color-mix(in srgb, canvastext 8%, transparent);
    border-color: color-mix(in srgb, canvastext 14%, transparent);
  }
  nav button:focus-visible {
    outline: 2px solid Highlight;
    outline-offset: 2px;
  }
  section {
    padding: 1.5rem 2rem;
  }
</style>
