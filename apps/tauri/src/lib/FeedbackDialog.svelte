<!-- Feedback dialog — opened by the sidebar "Tell me what you think" CTA. Reuses
     the shared Modal (focus-in on open, Escape cancels, focus returns to the CTA
     on close). This is the ONE place anything leaves the Mac, and only on Send,
     and only the message + optional email — NEVER typing/motor data (Principle
     #8). The actual send is STUBBED (TODO(feedback-endpoint)) — no network call
     is made yet; sendFeedback resolves as if sent. -->
<script lang="ts">
  import { onDestroy, createEventDispatcher } from "svelte";
  import Modal from "./Modal.svelte";

  const dispatch = createEventDispatcher<{ close: void }>();

  let message = "";
  let email = "";
  let sent = false;
  let sending = false;

  // After Send the confirmation shows briefly, then the whole dialog auto-dismisses
  // with a quiet fade. Escape still closes it immediately (handled by the Modal).
  const AUTO_DISMISS_MS = 5000;
  let dismissTimer: ReturnType<typeof setTimeout> | undefined;
  onDestroy(() => clearTimeout(dismissTimer));

  // Send needs a message; an email is optional but, if given, must look like one
  // (a light sanity check — just an "@", not full RFC validation).
  $: emailLooksOk = email.trim() === "" || email.includes("@");
  $: emailInvalid = email.trim() !== "" && !email.includes("@");
  $: canSend = message.trim().length > 0 && emailLooksOk && !sending;

  // STUB — wire to an endpoint (Formspree or our own domain) in a later pass.
  async function sendFeedback(payload: { message: string; email: string }): Promise<void> {
    // TODO(feedback-endpoint): POST { message, email } to the chosen endpoint.
    // This is the only outbound request in the app and carries ONLY what the user
    // typed here — never keystrokes, motor map, or any typing data (Principle #8).
    // No network call yet — resolve as if sent.
    console.info("[stub] sendFeedback →", payload);
    return Promise.resolve();
  }

  async function onSend() {
    if (!canSend) return;
    sending = true;
    try {
      await sendFeedback({ message: message.trim(), email: email.trim() });
      sent = true;
      dismissTimer = setTimeout(() => dispatch("close"), AUTO_DISMISS_MS);
    } finally {
      sending = false;
    }
  }
</script>

<Modal titleId="feedback-title" fadeOut on:cancel={() => dispatch("close")}>
  {#if !sent}
    <h2 id="feedback-title" class="dlg-title">Tell me what you think</h2>
    <p class="dlg-sub">Anything at all — what’s working, what’s clunky, what you wish I did.</p>

    <textarea
      class="fb-message"
      bind:value={message}
      data-autofocus
      rows="4"
      placeholder="What’s on your mind?"
      aria-label="Your message"
    ></textarea>

    <input
      class="fb-email"
      class:invalid={emailInvalid}
      type="email"
      bind:value={email}
      placeholder="Your email (optional — only if you’d like a reply)"
      aria-label="Your email (optional)"
      aria-invalid={emailInvalid}
    />

    <p class="fb-privacy">
      <i class="ti ti-lock" aria-hidden="true"></i>
      <span>
        This is the only thing that leaves your Mac, and only when you tap Send — just your
        message, never your typing data.
      </span>
    </p>

    <!-- Send leads (primary, left); Cancel steps back as a quiet text link, far
         right with space between them so it's never an equal-weight target next to
         Send. Opposite emphasis from the destructive Delete/Restore dialogs. -->
    <div class="dlg-actions">
      <button class="btn-primary" disabled={!canSend} on:click={onSend}>Send</button>
      <button class="cancel-link" on:click={() => dispatch("close")}>Cancel</button>
    </div>
  {:else}
    <h2 id="feedback-title" class="sr-only">Feedback sent</h2>
    <div class="fb-confirm" aria-live="polite">
      <i class="ti ti-circle-check fb-check" aria-hidden="true"></i>
      <p class="fb-thanks">Thanks — that’s on its way.</p>
    </div>
  {/if}
</Modal>

<style>
  .dlg-title {
    margin: 0 0 0.4rem;
    font-size: 1.1rem;
    font-weight: 600;
  }
  .dlg-sub {
    margin: 0 0 1rem;
    font-size: 0.92rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }

  .fb-message,
  .fb-email {
    display: block;
    width: 100%;
    box-sizing: border-box;
    font: inherit;
    font-size: 0.95rem;
    color: canvastext;
    background: color-mix(in srgb, canvastext 4%, canvas);
    border: 1px solid var(--hairline);
    border-radius: 9px;
    padding: 0.6rem 0.7rem;
  }
  .fb-message {
    resize: vertical;
    min-height: 5.5rem;
    line-height: 1.5;
  }
  .fb-email {
    margin-top: 0.7rem;
  }
  .fb-message::placeholder,
  .fb-email::placeholder {
    color: var(--text-secondary);
  }
  .fb-message:focus,
  .fb-email:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
    border-color: transparent;
  }
  /* Gentle, non-red invalid hint (amber) — the app avoids red as a signal. */
  .fb-email.invalid {
    border-color: var(--warning);
  }

  .fb-privacy {
    display: flex;
    align-items: flex-start;
    gap: 0.45rem;
    margin: 0.9rem 0 0;
    font-size: 0.82rem;
    line-height: 1.5;
    color: var(--text-secondary);
  }
  .fb-privacy .ti {
    font-size: 0.95rem;
    margin-top: 0.1rem;
    flex-shrink: 0;
  }

  /* ---- confirmation ---- */
  .fb-confirm {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.7rem;
    padding: 1.4rem 0 1rem;
    text-align: center;
  }
  /* Blue accent, never green (colour-blind-safe). */
  .fb-check {
    font-size: 2.4rem;
    color: var(--link);
  }
  .fb-thanks {
    margin: 0;
    font-size: 1.02rem;
    font-weight: 500;
    color: canvastext;
  }

  /* ---- actions ---- */
  /* Send leads on the left; Cancel sits far right. space-between keeps the quiet
     Cancel link well clear of Send so it's never an accidental equal-weight tap. */
  .dlg-actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 1.3rem;
  }
  .btn-primary {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    min-height: 38px;
    padding: 0.5rem 1.15rem;
    font: inherit;
    font-size: 0.92rem;
    font-weight: 500;
    border-radius: 9px;
    cursor: pointer;
    border: none;
    background: var(--focus-ring);
    color: #fff;
  }
  .btn-primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--focus-ring) 88%, black);
  }
  .btn-primary:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .btn-primary:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
  }
  /* Cancel — a quiet text link that steps back, not an equal-weight button. */
  .cancel-link {
    padding: 0.3rem 0.2rem;
    font: inherit;
    font-size: 0.9rem;
    color: var(--text-secondary);
    background: transparent;
    border: none;
    cursor: pointer;
  }
  .cancel-link:hover {
    color: canvastext;
    text-decoration: underline;
  }
  .cancel-link:focus {
    outline: 3px solid var(--focus-ring);
    outline-offset: 2px;
    border-radius: 5px;
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    margin: -1px;
    padding: 0;
    border: 0;
    clip: rect(0 0 0 0);
    clip-path: inset(50%);
    overflow: hidden;
    white-space: nowrap;
  }
</style>
