<!-- Home — Jordan's greeting + a calm, state-aware read, with a passive invite to
     turn on Corrections once Fluent. Home INVITES (the warm-up CTA lives on
     Today); it carries no per-key detail (that's Progress). Renders all three
     app states (Day one / Building back / Fluent), driven by the `appState`
     store. Copy is verbatim from the locked front-end design. -->
<script lang="ts">
  import { userName, appState, type AppState } from "../lib/previewSettings";

  // Time-of-day greeting (the design shows "Good afternoon, …").
  const hour = new Date().getHours();
  const partOfDay = hour < 12 ? "morning" : hour < 18 ? "afternoon" : "evening";
  $: greeting = `Good ${partOfDay}, ${$userName}.`;

  type Copy = { sub: string; body: string; today: string; status: string; invite?: string };
  const COPY: Record<AppState, Copy> = {
    day1: {
      sub: "I’m Jordan, and I’m quietly learning as you type — everywhere on your Mac.",
      body: "We’ve only just met. Right now I’m watching how your hands move across the keys — nothing more. The more you type, the more I’ll understand.",
      today: "Glad to meet you — let’s start typing.",
      status: "Jordan is here, quietly learning.",
    },
    building: {
      sub: "Jordan is quietly learning as you type, everywhere on your Mac.",
      body: "I’m starting to see your pattern. A few keys trip you up more than others — the detail’s in Progress whenever you want it.",
      today: "A steady start. Your rhythm is finding itself.",
      status: "Jordan is active.",
    },
    fluent: {
      sub: "Jordan is quietly learning as you type, everywhere on your Mac.",
      body: "I’ve got a clear picture of your hands now — your trouble spots and your gains are both in Progress.",
      invite:
        "I can help as you type now, if you’d like — turn on Corrections. You’ll always be the one typing.",
      today: "Smooth so far. You’re in a good groove.",
      status: "Jordan is active.",
    },
  };
  $: copy = COPY[$appState];
</script>

<section class="home">
  <h1 class="greeting">{greeting}</h1>
  <p class="sub">{copy.sub}</p>

  <hr class="rule" />

  <p class="body">{copy.body}</p>

  {#if copy.invite}
    <div class="invite">
      <i class="ti ti-bulb" aria-hidden="true"></i>
      <span>{copy.invite}</span>
    </div>
  {/if}

  <hr class="rule" />

  <div class="todayrow">
    <span class="today-label">Today</span>
    <span class="today-line">{copy.today}</span>
  </div>

  <div class="status">
    <span class="greendot" aria-hidden="true"></span>
    <span>{copy.status}</span>
  </div>
</section>

<style>
  .home {
    max-width: 38rem;
  }
  .greeting {
    margin: 0 0 0.5rem;
    font-size: 1.7rem;
    font-weight: 600;
    letter-spacing: -0.02em;
    color: canvastext;
  }
  .sub {
    margin: 0;
    font-size: 1rem;
    line-height: 1.6;
    color: var(--text-secondary);
  }
  .rule {
    border: none;
    border-top: 1px solid var(--hairline);
    margin: 1.1rem 0;
  }
  .body {
    margin: 0;
    font-size: 1rem;
    line-height: 1.6;
    color: canvastext;
  }

  /* Passive Corrections invite (Fluent only) — accent-tinted, never a button.
     "turn on Corrections" is copy; Corrections is turned on from the menu or
     Settings. */
  .invite {
    display: flex;
    align-items: flex-start;
    gap: 0.6rem;
    margin: 1rem 0 0;
    padding: 0.75rem 0.9rem;
    border-radius: 10px;
    font-size: 0.95rem;
    line-height: 1.5;
    color: canvastext;
    background: color-mix(in srgb, var(--focus-ring) 10%, canvas);
    border: 1px solid color-mix(in srgb, var(--focus-ring) 28%, canvas);
  }
  .invite .ti {
    font-size: 1.1rem;
    color: var(--focus-ring);
    flex-shrink: 0;
    margin-top: 0.05rem;
  }

  .todayrow {
    display: flex;
    align-items: baseline;
    gap: 0.85rem;
  }
  .today-label {
    font-weight: 600;
    color: canvastext;
  }
  .today-line {
    color: var(--text-secondary);
  }

  .status {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 1.6rem;
    font-size: 0.95rem;
    color: var(--text-secondary);
  }
  /* "Jordan is active" indicator. Green reads as live in both light and dark. */
  .greendot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: #28c840;
    flex-shrink: 0;
  }
</style>
