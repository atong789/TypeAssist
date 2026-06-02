<!--
  Builder's debug view — hidden behind Cmd+Shift+D.

  Not a user feature. Utilitarian, monospace, exempt from the calm app voice.
  Lives on top of the regular UI so you can keep using the app while watching
  what the engine is doing.

  Listens to engine events emitted from `apps/tauri/src-tauri/src/engine.rs`:
    engine://keystroke       — every key (or backspace) + dwell + ingest cost
    engine://decision        — per sealed Word token: would-correct OR leave-alone (reason)
    engine://model-snapshot  — L2 BehaviouralModel state (per-key + per-finger)
    engine://slip            — confirmed L2 slip
    engine://token           — per L4 token (Observing Component 1)
    engine://line-reset      — tokenizer line reset (newline / backspace rebuild)
    engine://anchor-snapshot — per L4 anchor change (Observing Component 2)
    engine://lexicon         — per Word token: known? + frequency (Component 3a)
    engine://candidates      — per unknown Word token: scored edit-1 candidates (3b + 3c-1)
    engine://log-record      — per appended decision-ledger record (Component 4)
    engine://log-record-updated — per outcome transition on an existing record (Component 5a, revisable)

  Layout:
    [resize handle — drag to resize, height persisted in sessionStorage]
    [strip: totals + Clear]
    [body: feed column | model column]
      feed:  scrolls independently
      model: title (fixed) + per-finger (pinned) + per-key (scrolls independently)
-->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";

  type KeystrokePayload = {
    key: string;
    dwell_ms: number;
    ingest_latency_ms: number;
  };
  /// Mirrors `correction_engine::decision::LeaveAloneReason`.
  type LeaveAloneReason =
    | "known"
    | "no_candidates"
    | "below_floor"
    | "below_active_tier"
    | "ambiguous";
  /// Mirrors `correction_engine::DecisionOutcome`. Tag-internal `kind`
  /// matches the serde tag on the Rust enum. 3c-3: `WouldCorrect` carries
  /// the candidate's `confidence`, not a mode name.
  type DecisionOutcome =
    | {
        kind: "would_correct";
        original: string;
        suggested: string;
        confidence: Confidence;
        score: number;
        runner_up_score: number | null;
      }
    | {
        kind: "leave_alone";
        original: string;
        reason: LeaveAloneReason;
      };
  /// Mirrors `DecisionPayload` in engine.rs (Components 3c-2 + 3c-3).
  /// Observe-only — `would_correct` is a proposal, not an injection.
  /// `active_tier` is the engine's mode (Cautious / Balanced / Eager).
  type DecisionPayload = {
    outcome: DecisionOutcome;
    active_tier: ConfidenceTier;
    decide_time_ms: number;
    decision_version: number;
  };

  // Mirrors `behavioural_model::ModelSnapshot`.
  type Hand = "left" | "right";
  type Finger = "thumb" | "index" | "middle" | "ring" | "pinky";
  type KeyTimingRow = {
    key: string;
    count: number;
    avg_dwell_ms: number;
    avg_interval_ms: number;
    hand: Hand | null;
    finger: Finger | null;
  };
  type FingerTimingRow = {
    hand: Hand;
    finger: Finger;
    total_count: number;
    avg_dwell_ms: number;
    avg_interval_ms: number;
  };
  type HandStats = {
    hand: Hand;
    count: number;
    avg_dwell_ms: number;
    avg_interval_ms: number;
  };
  type AsymmetrySnapshot = {
    left: HandStats;
    right: HandStats;
    dwell_ratio: number;
    interval_ratio: number;
    overall_score: number;
    steadier_hand: Hand | null;
  };
  type KeyGhostRow = {
    key: string;
    hand: Hand | null;
    finger: Finger | null;
    events: number;
    short_dwell: number;
    rapid_repeat: number;
    self_corrected: number;
  };
  type FingerGhostRow = {
    hand: Hand;
    finger: Finger;
    events: number;
  };
  type HandGhostRow = {
    hand: Hand;
    events: number;
    short_dwell: number;
    rapid_repeat: number;
    self_corrected: number;
  };
  type GhostKeysSnapshot = {
    total_ghost_events: number;
    short_dwell_count: number;
    rapid_repeat_count: number;
    self_corrected_count: number;
    /// Always exactly two rows: [Left, Right]. Both present even with zero
    /// data, so the L vs R comparison is always visible.
    per_hand: HandGhostRow[];
    per_key: KeyGhostRow[];
    per_finger: FingerGhostRow[];
    /// Adaptive cutoff in ms; 0 while warming up.
    dwell_threshold_ms: number;
    /// Keystrokes left before the threshold goes live; 0 once active.
    warmup_remaining: number;
  };
  type PairSlipRow = {
    aimed_for: string;
    hit_instead: string;
    hand: Hand | null;
    finger: Finger | null;
    count: number;
    last_seen_ms: number;
  };
  type SlipsSnapshot = {
    total_slips: number;
    per_pair: PairSlipRow[];
    map_swap_pairs: number;
    map_key_confidence: number;
    /// C5b kill-switch state — when false, confirmed slips tally but
    /// don't write to the L3 volatility map. Defaulted off pending
    /// motor-verdict validation; the SLIPS section reflects the live
    /// state so the builder always knows whether learning is active.
    map_writes_enabled: boolean;
  };
  type SlipPayload = {
    aimed_for: string;
    hit_instead: string;
    hand: Hand | null;
    finger: Finger | null;
    timestamp_ms: number;
  };
  /// Mirrors `correction_engine::tokenizer::Token`. The L4 Observing brief,
  /// Component 1: shared boundary-of-truth tokenizer.
  type TokenKind = "word" | "number" | "url" | "email" | "code" | "acronym";
  type TokenPayload = {
    core: string;
    start: number;
    end: number;
    leading: string;
    trailing: string;
    terminator: string | null;
    kind: TokenKind;
    correctable: boolean;
    tokenizer_version: number;
  };
  /// Mirrors `correction_engine::anchor::SpanAnchor`. Component 2: span
  /// anchor tracking via edit deltas.
  type VoidReason = "split" | "merge" | "deleted";
  type AnchorState =
    | { kind: "tracking" }
    | { kind: "void"; reason: VoidReason };
  type SpanAnchorRow = {
    id: number;
    original_core: string;
    start: number;
    end: number;
    state: AnchorState;
  };
  type AnchorsSnapshot = {
    anchors: SpanAnchorRow[];
    void_count: number;
    /// Engine wraps the pure tracker snapshot with the current line buffer
    /// so the panel can show text-now-at-span without needing its own
    /// keystroke book-keeping.
    current_line: string;
  };
  /// Mirrors `LexiconPayload` in engine.rs. Emitted once per sealed Word
  /// token (Component 3a — read-only word source, no scoring yet).
  type LexiconPayload = {
    word: string;
    known: boolean;
    frequency: number;
    lexicon_version: number;
  };
  /// Mirrors `correction_engine::score::EditType`. Slip-perspective:
  /// the user added an extra key (insertion) or missed one (deletion).
  type EditType = "substitution" | "transposition" | "insertion" | "deletion";
  /// Mirrors `correction_engine::ConfidenceTier` — the engine **mode**.
  /// Names how aggressive the engine should be; never used as a per-
  /// candidate label.
  type ConfidenceTier = "cautious" | "balanced" | "eager";
  /// Mirrors `correction_engine::Confidence` — the per-candidate
  /// **confidence label** the badge renders. Separate vocabulary from
  /// the mode (3c-3 contract).
  type Confidence = "high" | "medium" | "low";
  /// Mirrors `correction_engine::ScoredCandidate` — one row in the
  /// CANDIDATES panel under 3c-1 scoring.
  type ScoredCandidate = {
    word: string;
    frequency: number;
    edit_type: EditType;
    lexicon_evidence: number;
    motor_evidence: number;
    score: number;
  };
  /// Mirrors `CandidatesPayload` in engine.rs (Components 3b + 3c-1 +
  /// 3c-3). Empty `scored` is a real outcome ("no known candidates
  /// within edit-1"). `top_confidence` is what the badge renders.
  type CandidatesPayload = {
    word: string;
    scored: ScoredCandidate[];
    top_score: number | null;
    top_confidence: Confidence | null;
    candidates_version: number;
    score_version: number;
  };
  /// Mirrors `correction_engine::log::LogConfidence` — the 4-state band
  /// recorded on each ledger entry. `below_floor` covers both "top
  /// candidate below floor" and "no candidates at all".
  type LogConfidence = "high" | "medium" | "low" | "below_floor";
  /// Mirrors `correction_engine::log::Outcome`. Component 5a resolves
  /// records from `pending` to one of the four terminal states; further
  /// edits on the same span can re-resolve (revisable).
  type Outcome =
    | { kind: "pending" }
    | { kind: "kept" }
    | { kind: "corrected_to_suggestion" }
    | { kind: "corrected_to_other" }
    | { kind: "abandoned" };
  /// Mirrors `correction_engine::motor_signal::TokenMotorSignal` —
  /// the candidate-INDEPENDENT per-token motor signal C5b reads.
  /// Present on every loggable record (fast lane included).
  type TokenMotorVerdict = "clean" | "slip" | "insufficient";
  type TokenMotorSignal = {
    verdict: TokenMotorVerdict;
    slip_score: number;
    graze_count: number;
    char_count: number;
  };
  /// Mirrors `correction_engine::log::LogRecord` — one decision ledger
  /// row. The full `decision` is the canonical outcome; `top_candidate`
  /// / `top_score` / `top_motor_evidence` are diagnostic projections;
  /// `token_motor` is what the C5b proposer actually gates on.
  type LogRecord = {
    id: number;
    timestamp_ms: number;
    original_text: string;
    decision: DecisionOutcome;
    top_candidate: string | null;
    top_score: number | null;
    top_motor_evidence: number | null;
    token_motor: TokenMotorSignal | null;
    confidence: LogConfidence;
    anchor_id: number;
    active_tier: ConfidenceTier;
    outcome: Outcome;
    /// LOG_VERSION v5 — was this Kept record actually credited to
    /// the lexicon proposer, or did "Pause learning" suppress it?
    ///   * null         — Pending OR resolved to a non-Kept outcome.
    ///   * true         — Kept while learning was active.
    ///   * false        — Kept while learning was paused (panel greys
    ///                    the row + tags it "paused" to make the
    ///                    distinction visible).
    credited: boolean | null;
    log_version: number;
  };

  // ---- Component 5b — lexicon proposals -------------------------------
  /// Mirrors `correction_engine::lexicon_proposal::Lane`.
  type Lane =
    | { kind: "fast" }
    | { kind: "slow"; rejected_confidence: LogConfidence };
  type MotorVerdict = "clean" | "mixed" | "slip" | "unknown";
  /// Mirrors the v3 `HoldReason` after the linguistic-gate stack.
  /// Held is "this token's execution looks slip-like OR this token
  /// is structurally unlike novel vocabulary" — motor + linguistic
  /// signals, never lane-only.
  type HoldReason =
    | "obvious_fragment"
    | "slip_signature"
    | "ill_formed"
    | "near_known_word"
    | "segmentable_merge"
    | "prefix_merge";
  /// Mirrors `correction_engine::linguistic::ProximityVerdict`.
  type ProximityVerdict =
    | "far_from_known"
    | "near_known_edit2"
    | "segmentable"
    | "prefix_merge";
  type ProposalTier =
    | { kind: "held"; reason: HoldReason }
    | { kind: "provisional" }
    | { kind: "confirmed" };
  /// Mirrors `correction_engine::lexicon_proposal::CasingBaseline`.
  /// Recency-weighted typing baseline + the derived rescue-active
  /// flag. The all-caps brand rescue fires only when this user
  /// hasn't been doing a lot of all-caps lately.
  type CasingBaseline = {
    all_caps_share: number;
    sample_count: number;
    rescue_active: boolean;
  };

  /// Mirrors `correction_engine::lexicon_proposal::LexiconProposal`.
  /// v4 adds `norvig_freq` — the typed word's web-corpus frequency
  /// (NOT through is_known). Combined with near-known proximity,
  /// `norvig_freq === 0` is what catches `aduluts`-class typos while
  /// letting `lol`-class informal real words through.
  type LexiconProposal = {
    word: string;
    lane: Lane;
    motor_verdict: MotorVerdict;
    tier: ProposalTier;
    occasions: number;
    last_motor_evidence: number | null;
    last_record_id: number;
    last_seen_ms: number;
    plausibility: number;
    proximity: ProximityVerdict;
    norvig_freq: number;
    version: number;
  };
  /// Mirrors the `LexiconProposalEvent` engine payload — proposal=null
  /// means the proposal was retracted (all contributing records rolled
  /// back under C5a's revisable transitions).
  type LexiconProposalEvent = {
    word: string;
    proposal: LexiconProposal | null;
  };
  // ---- C5c Layer A — MotorBaseline types ------------------------------
  /// One row of the per-finger motor baseline table.
  type MotorFingerRow = {
    hand: Hand;
    finger: Finger;
    reliability_score: number;
    n_eff: number;
    dwell_mean_ms: number | null;
    iki_mean_ms: number | null;
  };
  type MotorBaselineSnapshot = {
    per_finger: MotorFingerRow[];
  };
  /// Per-keystroke anomaly probe — the engine sends one of these per
  /// keystroke (before the baseline absorbs it).
  type DwellAnomaly = {
    z: number;
    anomaly: number;
    mean_used: number;
    sigma_used: number;
    certainty: number;
  };
  type IkiAnomaly = DwellAnomaly;
  type CoActivation = { overlap_ms: number; anomaly: number };
  type KeystrokeAnomaly = {
    dwell: DwellAnomaly;
    iki: IkiAnomaly | null;
    coactivation: CoActivation | null;
    combined: number;
    certainty: number;
  };
  type MotorKeystrokeEvent = {
    key: string;
    timestamp_ms: number;
    dwell_ms: number;
    hand: Hand | null;
    finger: Finger | null;
    anomaly: KeystrokeAnomaly | null;
  };

  type ModelSnapshot = {
    timing: { per_key: KeyTimingRow[]; per_finger: FingerTimingRow[] };
    asymmetry: AsymmetrySnapshot;
    ghost_keys: GhostKeysSnapshot;
    slips: SlipsSnapshot;
    motor_baseline: MotorBaselineSnapshot;
  };

  /// Cap on the per-key ghost list — keep the worst offenders visible,
  /// drop the long tail to keep the panel tidy.
  const GHOST_PER_KEY_LIMIT = 10;

  type Row =
    | { id: number; kind: "key"; key: string; dwell_ms: number; ingest_latency_ms: number }
    | {
        id: number;
        kind: "decision";
        outcome: DecisionOutcome;
        active_tier: ConfidenceTier;
        decide_time_ms: number;
      }
    | {
        id: number;
        kind: "slip";
        aimed_for: string;
        hit_instead: string;
        hand: Hand | null;
        finger: Finger | null;
      };

  // Cap the feed so a long session doesn't pin unbounded memory / DOM.
  const MAX_ROWS = 500;

  let rows: Row[] = [];
  let nextId = 0;

  // Running totals — kept separate from `rows` so the strip stays accurate
  // even after old rows are evicted from the visible feed. Under 3c-2 the
  // engine is observe-only, so we count "would-correct" decisions and
  // average their per-token decide time (scoring + decision policy cost).
  let keystrokeCount = 0;
  let wouldCorrectCount = 0;
  let decideSum = 0;
  let decideMax = 0;
  let decideCount = 0;
  $: decideAvg = decideCount === 0 ? 0 : decideSum / decideCount;
  /// Active engine mode, latched from the most recent DECISION payload.
  /// `null` until the first decision arrives (no decisions yet).
  let activeMode: ConfidenceTier | null = null;

  // Latest L2 snapshot. Replaced wholesale on every model-snapshot event.
  let modelRows: KeyTimingRow[] = [];
  let fingerRows: FingerTimingRow[] = [];
  let asymmetry: AsymmetrySnapshot | null = null;
  let ghostKeys: GhostKeysSnapshot | null = null;
  let slips: SlipsSnapshot | null = null;
  /// Tokens sealed by the L4 tokenizer for the CURRENT line. Cleared by
  /// `engine://line-reset` events (newline, backspace rebuild, special key).
  let lineTokens: TokenPayload[] = [];
  /// Latest anchor snapshot from the engine. Replaced wholesale on each
  /// `engine://anchor-snapshot`. The panel doesn't infer anchor state from
  /// edits — engine is the source of truth.
  let anchorsSnap: AnchorsSnapshot | null = null;
  /// Lexicon lookups for words sealed on the CURRENT line. One row per
  /// Word token, appended in order. Cleared by `engine://line-reset`.
  let lineLexicon: LexiconPayload[] = [];
  /// Candidate sets for UNKNOWN Word tokens on the CURRENT line. One row
  /// per unknown word (known words never emit). Cleared by
  /// `engine://line-reset` alongside the other line-scoped state.
  let lineCandidates: CandidatesPayload[] = [];
  /// Decision-ledger records from Component 4. Session-spanning by
  /// design (NOT cleared on line-reset) — this is the structured
  /// history of every decision the engine made, with outcome slots C5
  /// will resolve later. Capped here to bound DOM cost; the engine's
  /// own ledger has its own cap (the source of truth).
  const MAX_LOG_ROWS = 200;
  let logRows: LogRecord[] = [];

  /// Component 5b lexicon proposals — keyed by word (case-preserved).
  /// The engine is the source of truth (in-memory only); the panel
  /// mirrors the snapshot via per-word update events. Phase 1 is
  /// observe-only: nothing here writes to is_known.
  let lexiconProposals: Record<string, LexiconProposal> = {};

  /// Recency-weighted casing baseline. Updated on every fresh
  /// Word/Acronym seal — surfaced in the LEXICON header so the
  /// rescue's current state is visible.
  let casingBaseline: CasingBaseline = {
    all_caps_share: 0,
    sample_count: 0,
    rescue_active: true,
  };

  /// **C5b Phase 2** — set of words currently live in `is_known` via
  /// the runtime-learned set. Updated wholesale on each
  /// `engine://learned-snapshot` event. Used to mark Confirmed
  /// proposals with a "LIVE" indicator and to flag LOG records whose
  /// `top_candidate` targets a learned word.
  let learnedSet: Set<string> = new Set();

  /// **C5 capture-health** — engine-derived view of the sidecar's
  /// event-tap state. Drives the header pill. Default `unknown` on
  /// mount; first heartbeat from the sidecar (≤2s after engine
  /// start) flips it to `live`. The shipped user surface needs this
  /// silent-death warning to be impossible to miss — see commit M's
  /// rationale.
  type CaptureHealth = "unknown" | "live" | "unhealthy" | "stopped";
  let captureHealth: CaptureHealth = "unknown";

  /// **C5c motor map** — the `StabilityReport` read-model from
  /// `engine://motor-stability`: kill-switch inputs (sample coverage,
  /// overall slip rate) + the weakest-keys preview Practice mode would
  /// build a curriculum from. Emitted periodically (~30s) and on demand
  /// (`request_motor_stability`, fired on mount so the readout populates
  /// immediately rather than waiting for the first periodic tick). `null`
  /// until the first emit; the map starts empty on a fresh session.
  interface StabilityReport {
    total_observations: number;
    keys_tracked: number;
    keys_well_sampled: number;
    min_samples: number;
    overall_slip_rate: number;
    weakest: [string, number][];
    generated_at: number;
  }
  let stability: StabilityReport | null = null;

  /// **C5c Layer A** — per-finger motor baseline snapshot, replaced
  /// wholesale on each `engine://motor-baseline` event. Renders as a
  /// stable 10-row table even before any data arrives.
  let motorBaseline: MotorBaselineSnapshot = { per_finger: [] };
  /// Rolling buffer of the last per-keystroke anomaly probes, newest
  /// first. Capped so the panel doesn't grow unbounded.
  const MOTOR_KEYSTROKE_HISTORY = 20;
  let motorKeystrokes: MotorKeystrokeEvent[] = [];

  /// **Meta-context pause** — manual only. An earlier prototype also
  /// auto-paused on debug-window focus, but that mechanism guarded
  /// the wrong thing: it fired while the user was merely WATCHING the
  /// panel (the panel has no text input — keystrokes don't go INTO
  /// it) and never fired in the actual pollution case (typing about
  /// TypeAssist in another app's chat). Recency-reclaim is robust
  /// enough as a backstop; this leaves one explicit knob the user
  /// owns. `engineReportedPaused` mirrors what the engine actually
  /// applied (echo from `engine://learning-paused`), so the indicator
  /// can never silently drift from the engine's real flag.
  let manualPaused = false;
  let engineReportedPaused = false;

  /// **Hard pause** — distinct from `manualPaused` above. When set,
  /// the engine drops Key/Backspace events at its task boundary, so
  /// the FEED freezes and the engine effectively sleeps. Used when
  /// the user is talking ABOUT the system and wants the engine
  /// quiet, not just suppressing the credit step. `engineReportedInputPaused`
  /// mirrors what the engine applied (echo from
  /// `engine://input-paused`), same drift-prevention rationale as
  /// the learning-paused echo.
  let inputPaused = false;
  let engineReportedInputPaused = false;

  let feedEl: HTMLDivElement;
  let unlistens: UnlistenFn[] = [];

  // ---- Resize support ---------------------------------------------------
  // Panel height (px) is user-controlled via the top drag handle and survives
  // toggling the panel (Cmd+Shift+D close/reopen) via sessionStorage. Stays a
  // builder tool: no keyboard arrow support, no a11y wiring.
  const STORAGE_KEY = "typeassist:debug-panel-height";
  const MIN_HEIGHT = 220;
  // Leave at least this much of the regular app visible above the panel.
  const MIN_APP_VISIBLE = 80;

  let panelHeight = 480; // sensible default before onMount loads window/storage
  let dragging = false;
  let dragStartY = 0;
  let dragStartHeight = 0;

  function clampHeight(px: number): number {
    const max = Math.max(MIN_HEIGHT, window.innerHeight - MIN_APP_VISIBLE);
    return Math.max(MIN_HEIGHT, Math.min(max, Math.round(px)));
  }

  function setPanelHeight(px: number) {
    panelHeight = clampHeight(px);
    try {
      sessionStorage.setItem(STORAGE_KEY, String(panelHeight));
    } catch {
      // sessionStorage can be unavailable in some webview contexts — ignore.
    }
  }

  function onResizeDown(e: PointerEvent) {
    dragging = true;
    dragStartY = e.clientY;
    dragStartHeight = panelHeight;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    e.preventDefault();
  }
  function onResizeMove(e: PointerEvent) {
    if (!dragging) return;
    // Dragging UP grows the panel (clientY decreases).
    const delta = dragStartY - e.clientY;
    setPanelHeight(dragStartHeight + delta);
  }
  function onResizeUp(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    try {
      (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    } catch {
      // releasePointerCapture throws if capture was already lost — fine.
    }
  }

  function push(row: Row) {
    const next = [...rows, row];
    if (next.length > MAX_ROWS) next.splice(0, next.length - MAX_ROWS);
    rows = next;
    // Auto-scroll to bottom (newest) after Svelte flushes.
    queueMicrotask(() => {
      if (feedEl) feedEl.scrollTop = feedEl.scrollHeight;
    });
  }

  function clear() {
    rows = [];
    keystrokeCount = 0;
    wouldCorrectCount = 0;
    decideSum = 0;
    decideMax = 0;
    decideCount = 0;
    // LOG is session-spanning, but Clear should reset the panel view —
    // the engine's ledger keeps its own copy as the source of truth.
    logRows = [];
    // Same for LEXICON proposals: engine owns the truth, panel just
    // mirrors. Clear wipes the local copy; the next proposal event
    // will repopulate. learnedSet is similarly mirrored — the engine
    // will resend a snapshot on the next learning event.
    lexiconProposals = {};
    learnedSet = new Set();
    // Note: Clear only resets the *view*. The L2 model in the backend keeps
    // its own counts — modelRows/fingerRows will repopulate on the next keystroke.
  }

  /// Engine-side LEXICON wipe. Distinct from `clear()` (panel mirrors
  /// only): asks the Rust engine to drop every proposal, every per-
  /// record contribution, every learned-at-ms entry, AND the lex's
  /// learned set. The engine emits `engine://lexicon-reset` +
  /// `engine://learned-snapshot` in response; the listeners above wipe
  /// the local mirrors. Used to validate recency reclamation: confirm
  /// a slip, reset, re-type the canonical, watch the cascade demote
  /// the slip when the canonical confirms.
  async function resetLexicon() {
    try {
      await invoke("reset_lexicon");
    } catch (err) {
      console.error("reset_lexicon failed:", err);
    }
  }

  /// Ask the engine to soft-restart capture — sidecar tears down its
  /// current event tap and creates a fresh one. The primary recovery
  /// path when the CAPTURE pill goes amber (Unhealthy) or red
  /// (Stopped). Stays clickable in all states so it's never the case
  /// that "the only way out is to restart the app."
  async function restartCapture() {
    try {
      await invoke("restart_capture");
    } catch (err) {
      console.error("restart_capture failed:", err);
    }
  }

  /// Pause controls become aria-disabled when capture is Stopped —
  /// clicking them would post to an engine that may have lost contact
  /// with the sidecar. The Restart capture button is the visible
  /// recovery path; once capture comes back, Pause buttons re-enable.
  $: pauseControlsDisabled = captureHealth === "stopped";

  /// Whenever the manual toggle flips, post the new state to the
  /// engine. The engine's echo on `engine://learning-paused` then
  /// drives the indicator — keeping the UI honest about the engine's
  /// actual flag rather than the panel's optimistic guess.
  $: {
    void manualPaused;
    void invoke("set_learning_paused", { paused: manualPaused }).catch((err) =>
      console.error("set_learning_paused failed:", err),
    );
  }

  /// Same shape for hard pause. Posted on every toggle; engine echoes
  /// back via `engine://input-paused`. On transition to true, the
  /// engine flushes its line state and emits `engine://line-reset` —
  /// the panel's existing line-reset listener already handles that.
  $: {
    void inputPaused;
    void invoke("set_input_paused", { paused: inputPaused }).catch((err) =>
      console.error("set_input_paused failed:", err),
    );
  }

  function togglePauseLearning() {
    manualPaused = !manualPaused;
  }

  function togglePauseInput() {
    inputPaused = !inputPaused;
  }

  onMount(async () => {
    // Restore height: stored value if present, otherwise a tall 60% default.
    let initial: number | null = null;
    try {
      const stored = sessionStorage.getItem(STORAGE_KEY);
      if (stored) {
        const n = parseInt(stored, 10);
        if (Number.isFinite(n)) initial = n;
      }
    } catch {
      // ignore
    }
    panelHeight = clampHeight(initial ?? Math.round(window.innerHeight * 0.6));

    unlistens.push(
      await listen<KeystrokePayload>("engine://keystroke", (e) => {
        keystrokeCount += 1;
        push({
          id: nextId++,
          kind: "key",
          key: e.payload.key,
          dwell_ms: e.payload.dwell_ms,
          ingest_latency_ms: e.payload.ingest_latency_ms,
        });
      }),
    );
    unlistens.push(
      await listen<DecisionPayload>("engine://decision", (e) => {
        decideCount += 1;
        decideSum += e.payload.decide_time_ms;
        if (e.payload.decide_time_ms > decideMax) {
          decideMax = e.payload.decide_time_ms;
        }
        if (e.payload.outcome.kind === "would_correct") {
          wouldCorrectCount += 1;
        }
        activeMode = e.payload.active_tier;
        push({
          id: nextId++,
          kind: "decision",
          outcome: e.payload.outcome,
          active_tier: e.payload.active_tier,
          decide_time_ms: e.payload.decide_time_ms,
        });
      }),
    );
    unlistens.push(
      await listen<ModelSnapshot>("engine://model-snapshot", (e) => {
        modelRows = e.payload.timing.per_key;
        fingerRows = e.payload.timing.per_finger;
        asymmetry = e.payload.asymmetry;
        ghostKeys = e.payload.ghost_keys;
        slips = e.payload.slips;
      }),
    );
    unlistens.push(
      // C5 capture-health — only emitted on TRANSITION (not every
      // heartbeat), so the default `unknown` stays until the engine
      // tells us otherwise. A panel reload may miss earlier
      // transitions; commit O's watchdog will re-emit the current
      // state on a timer so reloads converge to truth.
      await listen<{ state: CaptureHealth }>("engine://capture-health", (e) => {
        captureHealth = e.payload.state;
      }),
    );
    unlistens.push(
      // C5c Layer A — per-finger motor baseline snapshot. Replaces
      // wholesale on each emission. (Same data flows in
      // engine://model-snapshot too, but a separate event lets
      // future panels listen for just the motor info without
      // re-parsing the whole model snapshot.)
      await listen<MotorBaselineSnapshot>("engine://motor-baseline", (e) => {
        motorBaseline = e.payload;
      }),
    );
    unlistens.push(
      // C5c Layer A — per-keystroke anomaly probe. Prepended to a
      // rolling buffer; the table renders newest first and the
      // buffer is capped so the panel doesn't grow unbounded.
      await listen<MotorKeystrokeEvent>("engine://motor-keystroke", (e) => {
        motorKeystrokes = [e.payload, ...motorKeystrokes].slice(
          0,
          MOTOR_KEYSTROKE_HISTORY,
        );
      }),
    );
    unlistens.push(
      await listen<SlipPayload>("engine://slip", (e) => {
        push({
          id: nextId++,
          kind: "slip",
          aimed_for: e.payload.aimed_for,
          hit_instead: e.payload.hit_instead,
          hand: e.payload.hand,
          finger: e.payload.finger,
        });
      }),
    );
    unlistens.push(
      await listen<TokenPayload>("engine://token", (e) => {
        lineTokens = [...lineTokens, e.payload];
      }),
    );
    unlistens.push(
      await listen("engine://line-reset", () => {
        lineTokens = [];
        lineLexicon = [];
        lineCandidates = [];
      }),
    );
    unlistens.push(
      await listen<AnchorsSnapshot>("engine://anchor-snapshot", (e) => {
        anchorsSnap = e.payload;
      }),
    );
    unlistens.push(
      await listen<LexiconPayload>("engine://lexicon", (e) => {
        lineLexicon = [...lineLexicon, e.payload];
      }),
    );
    unlistens.push(
      await listen<CandidatesPayload>("engine://candidates", (e) => {
        lineCandidates = [...lineCandidates, e.payload];
      }),
    );
    unlistens.push(
      await listen<LogRecord>("engine://log-record", (e) => {
        const next = [...logRows, e.payload];
        if (next.length > MAX_LOG_ROWS) next.splice(0, next.length - MAX_LOG_ROWS);
        logRows = next;
        // Auto-scroll within the LOG section. The whole right column
        // scrolls together, so the user controls position there — but
        // when LOG is in view we want newest visible. Defer to Svelte
        // flushing the DOM.
        queueMicrotask(() => {
          const el = document.getElementById("debug-log-tail");
          if (el) el.scrollIntoView({ block: "nearest" });
        });
      }),
    );
    unlistens.push(
      // Component 5b — casing baseline. Updated on every fresh
      // Word/Acronym seal; the engine emits the full snapshot each
      // time, so the panel just replaces wholesale.
      await listen<CasingBaseline>("engine://casing-baseline", (e) => {
        casingBaseline = e.payload;
      }),
    );
    unlistens.push(
      // Component 5b Phase 2 — runtime-learned-words snapshot.
      // Lowercase keys; replace wholesale on each emission.
      await listen<string[]>("engine://learned-snapshot", (e) => {
        learnedSet = new Set(e.payload.map((w) => w.toLowerCase()));
      }),
    );
    unlistens.push(
      // Component 5b — per-word proposal updates. proposal=null means
      // the proposal was retracted; remove from the local map so the
      // panel matches the engine's state.
      await listen<LexiconProposalEvent>("engine://lexicon-proposal", (e) => {
        const { word, proposal } = e.payload;
        if (proposal === null) {
          const next = { ...lexiconProposals };
          delete next[word];
          lexiconProposals = next;
        } else {
          lexiconProposals = { ...lexiconProposals, [word]: proposal };
        }
      }),
    );
    unlistens.push(
      // Component 5b Phase 2 follow-up — engine-side LEXICON wipe.
      // Distinct from a panel-side Clear: the engine's proposer state
      // and lex.learned have both been cleared (see Reset LEXICON
      // button). The accompanying learned-snapshot will also fire and
      // empty `learnedSet`; this listener wipes the per-word proposal
      // mirror so the table doesn't keep ghost rows. Empty payload.
      await listen("engine://lexicon-reset", () => {
        lexiconProposals = {};
        learnedSet = new Set();
      }),
    );
    unlistens.push(
      // Engine echoes the applied pause state after every
      // set_learning_paused command. Read by the indicator so the UI
      // can never silently drift from the engine's real flag (e.g.
      // if the command channel is closed, the indicator stays where
      // the engine left it rather than where the panel guessed).
      await listen<{ paused: boolean }>("engine://learning-paused", (e) => {
        engineReportedPaused = e.payload.paused;
      }),
    );
    unlistens.push(
      // Same echo for hard input-pause — drives the "input paused"
      // indicator state. The engine's emitted line-reset that
      // accompanies a pause-on transition is handled by the existing
      // engine://line-reset listener, which already wipes the
      // current-line panel state.
      await listen<{ paused: boolean }>("engine://input-paused", (e) => {
        engineReportedInputPaused = e.payload.paused;
      }),
    );
    unlistens.push(
      // Component 5a — outcome transition for an existing record.
      // **Revisable**: the same id may receive several updates as the
      // user revisits the word's span. Update in place by id; the
      // engine is the source of truth (we never re-derive outcomes
      // panel-side, only mirror what the resolver reports). If the
      // updated record was evicted from our local window (rare —
      // requires >200 records since first emit), drop the update.
      await listen<LogRecord>("engine://log-record-updated", (e) => {
        const updated = e.payload;
        const i = logRows.findIndex((r) => r.id === updated.id);
        if (i < 0) return;
        // New array reference so Svelte picks up the change.
        const next = logRows.slice();
        next[i] = updated;
        logRows = next;
      }),
    );
    unlistens.push(
      // C5c motor stability read-model. Replaced wholesale on each emit
      // (periodic ~30s + on-demand). Drives the always-visible MOTOR
      // readout so the slip map can be sanity-checked as it accumulates.
      await listen<StabilityReport>("engine://motor-stability", (e) => {
        stability = e.payload;
      }),
    );
    // Pull a fresh report now so the readout isn't blank until the first
    // periodic tick (it's a read-only request — emits, changes nothing).
    void invoke("request_motor_stability").catch((err) =>
      console.error("request_motor_stability failed:", err),
    );
  });

  onDestroy(() => {
    for (const u of unlistens) u();
    unlistens = [];
  });

  // Render-helpers — keep templates terse.
  function fmtMs(n: number): string {
    return n.toFixed(2);
  }
  function fmtKey(k: string): string {
    if (k === " ") return "␣";
    if (k === "\t") return "⇥";
    if (k === "\n" || k === "\r") return "⏎";
    return k;
  }
  function fmtFinger(hand: Hand, finger: Finger): string {
    const h = hand === "left" ? "L" : "R";
    return `${h} ${finger}`;
  }
  function fmtKeyFinger(row: { hand: Hand | null; finger: Finger | null }): string {
    if (!row.hand || !row.finger) return "—";
    return fmtFinger(row.hand, row.finger);
  }
  /// Slip rate (0–1) → one-decimal percent for the MOTOR readout.
  function fmtSlipPct(rate: number): string {
    return `${(rate * 100).toFixed(1)}%`;
  }
  /// Compose a token's raw form for display: leading + core + trailing,
  /// with core highlighted (the brief: span anchor tracks core only).
  function fmtTokenText(leading: string, core: string, trailing: string): string {
    return `${leading}${core}${trailing}`;
  }
  /// Char-indexed slice of `current_line` at `[start, end)` for an anchor.
  /// JS strings are UTF-16, so we go via Array.from to count chars properly.
  function sliceLine(line: string, start: number, end: number): string {
    const arr = Array.from(line);
    return arr.slice(start, end).join("");
  }
  /// Group separators for raw unigram counts so 23,135,851,162 is readable.
  function fmtFreq(n: number): string {
    if (n === 0) return "—";
    return n.toLocaleString("en-US");
  }
  /// Compact "23.1B" style for the CANDIDATES inline list — same numbers,
  /// just shorter so the per-word row fits e.g. `thge → the (23.1B), thee
  /// (8.6M), tage (695K)`.
  function fmtFreqCompact(n: number): string {
    if (n === 0) return "—";
    if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
    return String(n);
  }
  /// Two-decimal score formatter for the CANDIDATES table.
  function fmtScore(n: number | null): string {
    if (n === null) return "—";
    return n.toFixed(2);
  }
  /// Human-readable label for the LeaveAlone reason in DECISION rows.
  /// Maps the serde snake_case wire value to the panel's preferred
  /// phrasing. Uses mode/confidence vocabulary per the 3c-3 split.
  function fmtLeaveAloneReason(r: LeaveAloneReason): string {
    switch (r) {
      case "known":
        return "known word";
      case "no_candidates":
        return "no known candidates";
      case "below_floor":
        return "below confidence floor";
      case "below_active_tier":
        return "below active mode";
      case "ambiguous":
        return "ambiguous — top two too close";
    }
  }
  /// Human-readable confidence label for the candidate badge — never a
  /// mode name. `null` is rendered separately ("below floor").
  function fmtConfidence(c: Confidence): string {
    switch (c) {
      case "high":
        return "HIGH";
      case "medium":
        return "MED";
      case "low":
        return "LOW";
    }
  }
  function fmtAnchorState(state: AnchorState): string {
    if (state.kind === "tracking") return "Tracking";
    return `Void:${state.reason}`;
  }
  /// LogConfidence band → short uppercase label for the LOG table.
  function fmtLogConfidence(c: LogConfidence): string {
    switch (c) {
      case "high":
        return "HIGH";
      case "medium":
        return "MED";
      case "low":
        return "LOW";
      case "below_floor":
        return "—";
    }
  }
  /// Decision arm → terse "would correct / leave-alone" label for LOG.
  function fmtDecisionShort(d: DecisionOutcome): string {
    if (d.kind === "would_correct") {
      return `would correct → ${d.suggested}`;
    }
    return `leave alone · ${fmtLeaveAloneReason(d.reason)}`;
  }
  /// Outcome slot label. Component 5a flips records out of `pending`
  /// once the user moves on; revisits flip them again.
  function fmtOutcome(o: Outcome): string {
    switch (o.kind) {
      case "pending":
        return "pending";
      case "kept":
        return "kept";
      case "corrected_to_suggestion":
        return "→ suggestion";
      case "corrected_to_other":
        return "→ other";
      case "abandoned":
        return "abandoned";
    }
  }
  // ---- LEXICON (Component 5b) formatters --------------------------------
  function fmtLane(l: Lane): string {
    return l.kind === "fast"
      ? "fast"
      : `slow · rejected ${fmtLogConfidence(l.rejected_confidence)}`;
  }
  function fmtMotorVerdict(v: MotorVerdict): string {
    switch (v) {
      case "clean":
        return "clean";
      case "slip":
        return "slip";
      case "mixed":
        return "mixed";
      case "unknown":
        return "—";
    }
  }
  function fmtMotorEvidence(m: number | null): string {
    return m === null ? "—" : m.toFixed(2);
  }
  function fmtHoldReason(r: HoldReason): string {
    switch (r) {
      case "obvious_fragment":
        return "fragment";
      case "slip_signature":
        return "slip";
      case "ill_formed":
        return "ill-formed";
      case "near_known_word":
        return "near-known";
      case "segmentable_merge":
        return "segment";
      case "prefix_merge":
        return "prefix-merge";
    }
  }
  function fmtProximity(p: ProximityVerdict): string {
    switch (p) {
      case "far_from_known":
        return "far";
      case "near_known_edit2":
        return "near-edit2";
      case "segmentable":
        return "segment";
      case "prefix_merge":
        return "prefix";
    }
  }
  function fmtPlausibility(p: number): string {
    return p.toFixed(2);
  }
  /// Norvig web-corpus frequency, compact format. `—` when zero
  /// (never seen on the web) — this is the signal the proposer
  /// combines with near-known proximity to veto typos like `aduluts`.
  function fmtNorvig(n: number): string {
    if (n === 0) return "—";
    return fmtFreqCompact(n);
  }
  function fmtProposalTier(t: ProposalTier): string {
    switch (t.kind) {
      case "held":
        return `held · ${fmtHoldReason(t.reason)}`;
      case "provisional":
        return "provisional";
      case "confirmed":
        return "confirmed";
    }
  }
  /// Most-recent-first ordering for the LEXICON table. Mirrors the
  /// Rust-side snapshot() ordering so test reasoning carries over.
  function sortedProposals(map: Record<string, LexiconProposal>): LexiconProposal[] {
    return Object.values(map).sort((a, b) => b.last_seen_ms - a.last_seen_ms);
  }
  $: lexiconProposalList = sortedProposals(lexiconProposals);
  function fmtRatio(r: number): string {
    return `${r.toFixed(2)}×`;
  }
  /**
   * One-liner read of the asymmetry score. The aggregator now drives the
   * decision off **dwell** only — interval is shown for diagnostics but is
   * too easily contaminated by reading/thinking pauses to lead with, even
   * after gap filtering. So the readout always cites dwell.
   */
  function asymmetryReadout(a: AsymmetrySnapshot): string {
    if (a.left.count === 0 && a.right.count === 0) return "no keys observed yet";
    if (a.steadier_hand === null) {
      if (a.left.count === 0) return "only right hand observed so far";
      if (a.right.count === 0) return "only left hand observed so far";
      return "left and right have equal dwell";
    }
    const steadier = a.steadier_hand === "left" ? "Left" : "Right";
    return `${steadier} hand steadier — ${fmtRatio(a.overall_score)} on dwell`;
  }
</script>

<section
  class="debug"
  aria-label="Engine debug panel"
  style="height: {panelHeight}px"
>
  <!-- Drag handle (top edge). Pointer events; height persists in sessionStorage. -->
  <div
    class="resize-handle"
    class:dragging
    role="separator"
    aria-orientation="horizontal"
    aria-label="Resize debug panel"
    on:pointerdown={onResizeDown}
    on:pointermove={onResizeMove}
    on:pointerup={onResizeUp}
    on:pointercancel={onResizeUp}
  >
    <div class="resize-grip" />
  </div>

  <header class="strip">
    <!-- C5 capture-health pill. Leftmost in the strip so silent
         capture death is impossible to miss while glancing at the
         panel. Color + text — never color-only. -->
    <div class="stat" title="Capture health — sidecar event-tap state. Live = events flowing. Unhealthy = sidecar alive but tap disabled. Stopped = no heartbeats; restart needed.">
      <span class="stat-label">CAPTURE</span>
      <span class="stat-val capture-pill capture-pill-{captureHealth}">
        <span class="capture-dot"></span>{captureHealth}
      </span>
    </div>
    <!-- Restart capture — soft restart via OutboundCommand::RestartTap.
         Always clickable; visually elevated (amber border) when the
         pill is anything other than "live" so the recovery path is
         obvious exactly when it's needed. Sits next to the pill so
         the state + recovery action read as one unit. -->
    <button
      type="button"
      class="restart-capture"
      class:restart-capture-elevated={captureHealth !== "live"}
      title="Tell the sidecar to tear down its current event tap and create a fresh one. Always available; use when CAPTURE pill is amber/red."
      on:click={restartCapture}
    >
      Restart capture
    </button>
    <div class="stat"><span class="stat-label">KEYS</span><span class="stat-val">{keystrokeCount}</span></div>
    <!-- MODE = active engine setting (Cautious / Balanced / Eager). Per
         3c-3, modes and per-candidate confidences use SEPARATE vocabularies
         — the badge in CANDIDATES is HIGH/MED/LOW, never a mode name. -->
    <div class="stat">
      <span class="stat-label">MODE</span>
      <span class="stat-val mode-pill mode-pill-{activeMode ?? 'none'}"
        >{activeMode ?? '—'}</span
      >
    </div>
    <div class="stat"><span class="stat-label">WOULD-CORR</span><span class="stat-val">{wouldCorrectCount}</span></div>
    <div class="stat"><span class="stat-label">DECIDE AVG</span><span class="stat-val">{fmtMs(decideAvg)} ms</span></div>
    <div class="stat"><span class="stat-label">DECIDE MAX</span><span class="stat-val">{fmtMs(decideMax)} ms</span></div>
    <div class="spacer" />
    <button type="button" class="clear" on:click={clear}>Clear</button>
  </header>

  <!-- C5c MOTOR stability readout. Always visible (funnel spirit) so the
       slip map can be sanity-checked as it accumulates. Updates on the
       engine's periodic ~30s emit + the on-mount request. Headline numbers
       are the kill-switch inputs; the weakest list is Practice's source. -->
  <div class="motor-readout" aria-label="Motor map stability">
    <span class="stat-label">MOTOR</span>
    {#if stability}
      <span class="motor-stat" title="Lifetime observations folded into the motor map">
        obs <b>{stability.total_observations}</b>
      </span>
      <span
        class="motor-stat"
        title="Keys with ≥{stability.min_samples} decayed samples (earned a slip rate), of {stability.keys_tracked} tracked"
      >
        sampled <b>{stability.keys_well_sampled}/{stability.keys_tracked}</b>
      </span>
      <span class="motor-stat" title="Decayed slip rate across well-sampled keys">
        slip <b>{fmtSlipPct(stability.overall_slip_rate)}</b>
      </span>
      <span class="motor-sep">·</span>
      <span class="motor-stat">weakest</span>
      {#if stability.weakest.length > 0}
        {#each stability.weakest as [k, rate] (k)}
          <span class="motor-key" title="{fmtKey(k)} slips {fmtSlipPct(rate)}"
            >{fmtKey(k)} {fmtSlipPct(rate)}</span
          >
        {/each}
      {:else}
        <span class="motor-empty">no well-sampled keys yet</span>
      {/if}
    {:else}
      <span class="motor-empty">waiting for first report…</span>
    {/if}
  </div>

  <div class="body">
    <!-- Left column: live event feed (scrolls independently). -->
    <section class="col col-feed" aria-label="Event feed">
      <div class="col-head">FEED · newest at bottom</div>
      <div class="feed" bind:this={feedEl}>
        {#each rows as row (row.id)}
          {#if row.kind === "key"}
            <div class="row row-key">
              <span class="tag">KEY</span>
              <span class="col-key">{fmtKey(row.key)}</span>
              <span class="col-dwell">dwell {row.dwell_ms}ms</span>
              <span class="col-lat">ingest {fmtMs(row.ingest_latency_ms)} ms</span>
            </div>
          {:else if row.kind === "decision"}
            {#if row.outcome.kind === "would_correct"}
              <div class="row row-decision row-matched">
                <span class="tag">DECISION</span>
                <span class="badge badge-corrected">WOULD CORRECT</span>
                <span class="col-word"
                  >{row.outcome.original} → {row.outcome.suggested} ({fmtConfidence(
                    row.outcome.confidence,
                  )},
                  {fmtScore(row.outcome.score)})</span
                >
                <span class="col-lat">decide {fmtMs(row.decide_time_ms)} ms</span>
              </div>
            {:else}
              <div class="row row-decision">
                <span class="tag">DECISION</span>
                <span class="badge badge-left">LEAVE ALONE</span>
                <span class="col-word"
                  >{row.outcome.original} — {fmtLeaveAloneReason(row.outcome.reason)}</span
                >
                <span class="col-lat">decide {fmtMs(row.decide_time_ms)} ms</span>
              </div>
            {/if}
          {:else if row.kind === "slip"}
            <div class="row row-slip">
              <span class="tag">SLIP</span>
              <span class="badge badge-slip">CANDIDATE</span>
              <span class="col-word">aimed {fmtKey(row.aimed_for)} · hit {fmtKey(row.hit_instead)}</span>
              <span class="col-lat">{fmtKeyFinger(row)}</span>
            </div>
          {/if}
        {/each}
        {#if rows.length === 0}
          <div class="empty">waiting for keystrokes…</div>
        {/if}
      </div>
    </section>

    <!-- Right column: L2 model state. Per-finger pinned at top, per-key scrolls. -->
    <section class="col col-model" aria-label="L2 model state">
      <div class="col-head">MODEL STATE</div>

      <!-- Pinned: small enough to always fit, important enough not to scroll away. -->
      <div class="model-pinned">
        <div class="model-sub">ASYMMETRY · left vs right</div>
        <div class="asym-table">
          <div class="asym-row asym-head">
            <span class="col-ah">hand</span>
            <span class="col-an num">count</span>
            <span class="col-ad num">avg dwell</span>
            <span class="col-ai num">avg interval</span>
          </div>
          {#if asymmetry === null}
            <div class="empty">no data yet…</div>
          {:else}
            <div class="asym-row asym-left">
              <span class="col-ah">L</span>
              <span class="col-an num">{asymmetry.left.count}</span>
              <span class="col-ad num">{fmtMs(asymmetry.left.avg_dwell_ms)} ms</span>
              <span class="col-ai num">{fmtMs(asymmetry.left.avg_interval_ms)} ms</span>
            </div>
            <div class="asym-row asym-right">
              <span class="col-ah">R</span>
              <span class="col-an num">{asymmetry.right.count}</span>
              <span class="col-ad num">{fmtMs(asymmetry.right.avg_dwell_ms)} ms</span>
              <span class="col-ai num">{fmtMs(asymmetry.right.avg_interval_ms)} ms</span>
            </div>
            <div class="asym-score">
              <span class="asym-score-label">score</span>
              <span class="asym-score-val num">{fmtRatio(asymmetry.overall_score)}</span>
              <span class="asym-score-detail">
                (dwell {fmtRatio(asymmetry.dwell_ratio)} · interval {fmtRatio(asymmetry.interval_ratio)})
              </span>
            </div>
            <div class="asym-readout">{asymmetryReadout(asymmetry)}</div>
          {/if}
        </div>

        <div class="model-sub">PER FINGER · anatomical order</div>
        <div class="finger-table">
          <div class="finger-row finger-head">
            <span class="col-ff">finger</span>
            <span class="col-fn num">count</span>
            <span class="col-fd num">avg dwell</span>
            <span class="col-fi num">avg interval</span>
          </div>
          {#if fingerRows.length === 0}
            <div class="empty">no fingers observed yet…</div>
          {:else}
            {#each fingerRows as f (`${f.hand}-${f.finger}`)}
              <div class="finger-row" class:finger-left={f.hand === "left"} class:finger-right={f.hand === "right"}>
                <span class="col-ff">{fmtFinger(f.hand, f.finger)}</span>
                <span class="col-fn num">{f.total_count}</span>
                <span class="col-fd num">{fmtMs(f.avg_dwell_ms)} ms</span>
                <span class="col-fi num">{fmtMs(f.avg_interval_ms)} ms</span>
              </div>
            {/each}
          {/if}
        </div>
      </div>

      <!-- Scrolling: TOKENS (L4 Observing) → SLIPS → GHOST KEYS → PER KEY.
           Each section's sub-header is sticky so it stays visible as you
           scroll within the section. -->
      <div class="model-scroll">
        <div class="model-sub model-sub-sticky">TOKENS · current line · L4 boundary truth</div>
        <div class="token-block">
          {#if lineTokens.length === 0}
            <div class="empty">no tokens on this line yet…</div>
          {:else}
            <div class="token-table">
              <div class="token-row token-head">
                <span class="col-tc">core</span>
                <span class="col-tk">kind</span>
                <span class="col-tx">corr</span>
                <span class="col-ts num">[start,end)</span>
                <span class="col-tt">term</span>
              </div>
              {#each lineTokens as t, i (`${i}-${t.start}-${t.core}`)}
                <div class="token-row" class:token-correctable={t.correctable}>
                  <span class="col-tc">{fmtTokenText(t.leading, t.core, t.trailing)}</span>
                  <span class="col-tk">{t.kind}</span>
                  <span class="col-tx">{t.correctable ? "yes" : "—"}</span>
                  <span class="col-ts num">[{t.start},{t.end})</span>
                  <span class="col-tt">{t.terminator === null ? "—" : fmtKey(t.terminator)}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">LEXICON · current line · L4 word source</div>
        <div class="lex-block">
          {#if lineLexicon.length === 0}
            <div class="empty">no word lookups on this line yet…</div>
          {:else}
            <div class="lex-table">
              <div class="lex-row lex-head">
                <span class="col-lw">word</span>
                <span class="col-lk">known?</span>
                <span class="col-lf num">frequency</span>
              </div>
              {#each lineLexicon as l, i (`${i}-${l.word}`)}
                <div class="lex-row" class:lex-known={l.known} class:lex-unknown={!l.known}>
                  <span class="col-lw">{l.word}</span>
                  <span class="col-lk">{l.known ? "yes" : "no"}</span>
                  <!-- Frequency is a ranking-only signal under lexicon v2:
                       it's meaningless for unknown words (a Norvig typo
                       like `teh` has 1.7M occurrences but isn't a real
                       word). Suppress to "—" when known=NO so the panel
                       doesn't suggest the count is load-bearing. -->
                  <span class="col-lf num">{l.known ? fmtFreq(l.frequency) : "—"}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">CANDIDATES · unknown words · L4 score (display-only)</div>
        <div class="cand-block">
          {#if lineCandidates.length === 0}
            <div class="empty">no unknown words on this line yet…</div>
          {:else}
            {#each lineCandidates as c, i (`${i}-${c.word}`)}
              <div class="cand-group">
                <div class="cand-header">
                  <span class="cand-src">{c.word}</span>
                  <span class="cand-arrow">→</span>
                  {#if c.scored.length === 0}
                    <span class="cand-empty">(no known candidates within edit-1)</span>
                  {:else if c.top_confidence !== null}
                    <!-- 3c-3: badge shows the candidate's CONFIDENCE
                         (High/Medium/Low) — never a mode name. The active
                         mode lives in the strip. -->
                    <span class="cand-conf cand-conf-{c.top_confidence}"
                      >{fmtConfidence(c.top_confidence)}</span
                    >
                    <span class="cand-top-score">top {fmtScore(c.top_score)}</span>
                  {:else}
                    <span class="cand-conf cand-conf-none">below floor</span>
                    <span class="cand-top-score">top {fmtScore(c.top_score)}</span>
                  {/if}
                </div>
                {#if c.scored.length > 0}
                  <div class="score-table">
                    <div class="score-row score-head">
                      <span class="col-sw">candidate</span>
                      <span class="col-se">edit</span>
                      <span class="col-sx num">freq</span>
                      <span class="col-sl num">lex_ev</span>
                      <span class="col-sm num">motor_ev</span>
                      <span class="col-ss num">score</span>
                    </div>
                    {#each c.scored as s, j (s.word)}
                      <div class="score-row" class:score-top={j === 0}>
                        <span class="col-sw">{s.word}</span>
                        <span class="col-se">{s.edit_type}</span>
                        <span class="col-sx num">{fmtFreqCompact(s.frequency)}</span>
                        <span class="col-sl num">{fmtScore(s.lexicon_evidence)}</span>
                        <span class="col-sm num">{fmtScore(s.motor_evidence)}</span>
                        <span class="col-ss num">{fmtScore(s.score)}</span>
                      </div>
                    {/each}
                  </div>
                {/if}
              </div>
            {/each}
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">ANCHORS · L4 spans · edit-delta tracked</div>
        <div class="anchor-block">
          {#if anchorsSnap === null || anchorsSnap.anchors.length === 0}
            <div class="empty">no anchors registered yet…</div>
          {:else}
            <div class="anchor-status">
              <span class="num">{anchorsSnap.anchors.length}</span> total ·
              <span class="num anchor-void-count">{anchorsSnap.void_count}</span> void
            </div>
            <div class="anchor-table">
              <div class="anchor-row anchor-head">
                <span class="col-ao">original</span>
                <span class="col-as num">[start,end)</span>
                <span class="col-an">text-now</span>
                <span class="col-ast">state</span>
              </div>
              {#each anchorsSnap.anchors as a (a.id)}
                {@const voided = a.state.kind === "void"}
                <div class="anchor-row" class:anchor-voided={voided}>
                  <span class="col-ao">{a.original_core}</span>
                  <span class="col-as num">[{a.start},{a.end})</span>
                  <span class="col-an">
                    {voided ? "—" : sliceLine(anchorsSnap.current_line, a.start, a.end) || "—"}
                  </span>
                  <span class="col-ast">{fmtAnchorState(a.state)}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">LOG · decision ledger · session-spanning · newest at bottom</div>
        <div class="log-block">
          {#if logRows.length === 0}
            <div class="empty">no decisions logged yet…</div>
          {:else}
            <div class="log-status">
              <span class="num">{logRows.length}</span> shown · outcomes resolve when the user moves on
            </div>
            <div class="log-table">
              <div class="log-row log-head">
                <span class="col-lo">original</span>
                <span class="col-ld">decision</span>
                <span class="col-lc num">score</span>
                <span class="col-lb">conf</span>
                <span class="col-la num">anchor</span>
                <span class="col-lx">outcome</span>
              </div>
              {#each logRows as r (r.id)}
                {@const targetsLearned = r.top_candidate !== null && learnedSet.has(r.top_candidate.toLowerCase())}
                {@const wasPaused = r.credited === false}
                <div
                  class="log-row"
                  class:log-would={r.decision.kind === "would_correct"}
                  class:log-leave={r.decision.kind === "leave_alone"}
                  class:log-learned-target={targetsLearned}
                  class:log-paused={wasPaused}
                >
                  <span class="col-lo">{r.original_text}</span>
                  <span class="col-ld">
                    {fmtDecisionShort(r.decision)}
                    {#if targetsLearned}<span class="log-learned-marker" title="top_candidate is a runtime-learned word — would-correct against the learning lexicon, not bundled SCOWL.">★L</span>{/if}
                  </span>
                  <span class="col-lc num">{fmtScore(r.top_score)}</span>
                  <span class="col-lb cand-conf cand-conf-{r.confidence === 'below_floor' ? 'none' : r.confidence}"
                    >{fmtLogConfidence(r.confidence)}</span
                  >
                  <span class="col-la num">#{r.anchor_id}</span>
                  <span class="col-lx">
                    {fmtOutcome(r.outcome)}
                    {#if wasPaused}<span class="log-paused-tag" title="Kept, but learning was paused — this record was OBSERVED but did NOT contribute to the lexicon.">paused</span>{/if}
                  </span>
                </div>
              {/each}
              <!-- Tail anchor so the listener can scrollIntoView on append. -->
              <div id="debug-log-tail" />
            </div>
          {/if}
        </div>

        <div class="model-sub model-sub-sticky lex-sub">
          <span class="lex-sub-label">
            LEXICON · 5b proposals · observe-only · is_known untouched ·
            all-caps <span class="num">{(casingBaseline.all_caps_share * 100).toFixed(0)}%</span>
            {#if casingBaseline.rescue_active}
              <span class="casing-rescue-on">rescue ✓</span>
            {:else}
              <span class="casing-rescue-off">rescue OFF</span>
            {/if}
            <span class="casing-samples">(n={casingBaseline.sample_count.toFixed(0)})</span>
            {#if engineReportedInputPaused}
              <span
                class="lex-input-paused-tag"
                title="input paused — engine drops Key/Backspace events at its task boundary. FEED is frozen; nothing observes."
              >
                · input paused
              </span>
            {:else if engineReportedPaused}
              <span
                class="lex-paused-tag"
                title="learning paused — note_record short-circuits. Records still appear in FEED/LOG but contribute nothing to the lexicon."
              >
                · learning paused
              </span>
            {/if}
          </span>
          <button
            type="button"
            class="lex-pause"
            class:lex-pause-on={manualPaused}
            class:lex-pause-stuck={pauseControlsDisabled}
            disabled={pauseControlsDisabled}
            aria-disabled={pauseControlsDisabled}
            title={pauseControlsDisabled
              ? "Capture is stopped — Pause is unavailable until capture is restored. Click 'Restart capture' in the header."
              : "Soft pause — suppress LEXICON credit but keep observing. FEED and decisions keep flowing; the lexicon just doesn't update. Use when watching the engine work without polluting vocabulary."}
            on:click={togglePauseLearning}
          >
            {manualPaused ? "Resume learning" : "Pause learning"}
          </button>
          <button
            type="button"
            class="lex-pause lex-pause-input"
            class:lex-pause-on={inputPaused}
            class:lex-pause-stuck={pauseControlsDisabled}
            disabled={pauseControlsDisabled}
            aria-disabled={pauseControlsDisabled}
            title={pauseControlsDisabled
              ? "Capture is stopped — Pause is unavailable until capture is restored. Click 'Restart capture' in the header."
              : "Hard pause — engine drops Key/Backspace events at its task boundary. FEED freezes; engine effectively sleeps. Use when typing about the system in chat. Toggling triggers a line-reset on the engine so resume starts on a clean boundary."}
            on:click={togglePauseInput}
          >
            {inputPaused ? "Resume input" : "Pause input"}
          </button>
          <button
            type="button"
            class="lex-reset"
            title="Engine-side wipe: drops every proposal AND every learned word. Distinct from the global Clear button (panel mirrors only)."
            on:click={resetLexicon}
          >
            Reset LEXICON
          </button>
        </div>
        <div class="lex-block">
          {#if lexiconProposalList.length === 0}
            <div class="empty">no proposals yet…</div>
          {:else}
            <div class="lex-status">
              <span class="num">{lexiconProposalList.length}</span> candidate word{lexiconProposalList.length === 1 ? "" : "s"} ·
              <span class="num">{lexiconProposalList.filter((p) => p.tier.kind === "confirmed").length}</span> confirmed ·
              <span class="num">{lexiconProposalList.filter((p) => p.tier.kind === "provisional").length}</span> provisional ·
              <span class="num">{lexiconProposalList.filter((p) => p.tier.kind === "held").length}</span> held
            </div>
            <div class="lex-table">
              <div class="lex-row lex-head">
                <span class="col-lxw">word</span>
                <span class="col-lxl">lane</span>
                <span class="col-lxm">motor</span>
                <span class="col-lxs num" title="slip_score from the per-token motor signal: fraction of chars in span whose dwell ≤ graze threshold (≤30ms). 0.00 = all clean. 1.00 = all graze.">slip</span>
                <span class="col-lxp num" title="Mean log10-probability of the word's character bigrams against the SCOWL distribution. Negative; higher = more well-formed.">plaus</span>
                <span class="col-lxx" title="Proximity verdict: far / near-edit2 / segment / prefix.">prox</span>
                <span class="col-lxn num" title="Norvig web-corpus frequency for the typed word. '—' = never seen on the web. Combined with near-known proximity, zero web freq vetoes the proposal as a typo (catches aduluts; lets lol-class informal words through).">web</span>
                <span class="col-lxt">tier</span>
                <span class="col-lxo num">×</span>
              </div>
              {#each lexiconProposalList as p (p.word)}
                {@const live = learnedSet.has(p.word.toLowerCase())}
                <div
                  class="lex-row"
                  class:lex-held={p.tier.kind === "held"}
                  class:lex-confirmed={p.tier.kind === "confirmed"}
                >
                  <span class="col-lxw">{p.word}</span>
                  <span class="col-lxl">{fmtLane(p.lane)}</span>
                  <span class="col-lxm lex-motor-{p.motor_verdict}">{fmtMotorVerdict(p.motor_verdict)}</span>
                  <span class="col-lxs num">{fmtMotorEvidence(p.last_motor_evidence)}</span>
                  <span class="col-lxp num">{fmtPlausibility(p.plausibility)}</span>
                  <span class="col-lxx lex-prox-{p.proximity}">{fmtProximity(p.proximity)}</span>
                  <span class="col-lxn num" class:lex-no-web={p.norvig_freq === 0}>{fmtNorvig(p.norvig_freq)}</span>
                  <span class="col-lxt">
                    {fmtProposalTier(p.tier)}
                    {#if live}<span class="lex-live" title="Live in is_known — stop-flagging + correction anchor.">LIVE</span>{/if}
                  </span>
                  <span class="col-lxo num">{p.occasions}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">
          SLIPS · strict-filtered candidates ·
          {#if slips?.map_writes_enabled}
            <span class="slips-writes-on">writes to L3 map (live)</span>
          {:else}
            <span class="slips-writes-off">L3 map writes OFF (C5b kill-switch)</span>
          {/if}
        </div>
        <div class="slip-block">
          {#if slips === null}
            <div class="empty">no data yet…</div>
          {:else}
            <div class="slip-status">
              total <span class="num slip-total">{slips.total_slips}</span>
              · L3 map: <span class="num">{slips.map_swap_pairs}</span> swap pairs,
              <span class="num">{slips.map_key_confidence}</span> key entries
            </div>
            {#if slips.per_pair.length === 0}
              <div class="empty">no slips confirmed yet…</div>
            {:else}
              <div class="slip-table">
                <div class="slip-row slip-head">
                  <span class="col-sa">aimed</span>
                  <span class="col-sh">hit</span>
                  <span class="col-sf">finger</span>
                  <span class="col-sn num">count</span>
                </div>
                {#each slips.per_pair as p (`${p.aimed_for}-${p.hit_instead}`)}
                  <div class="slip-row">
                    <span class="col-sa">{fmtKey(p.aimed_for)}</span>
                    <span class="col-sh">{fmtKey(p.hit_instead)}</span>
                    <span class="col-sf">{fmtKeyFinger(p)}</span>
                    <span class="col-sn num">{p.count}</span>
                  </div>
                {/each}
              </div>
            {/if}
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">GHOST KEYS · likely, not certain</div>
        <div class="ghost-block">
          {#if ghostKeys === null}
            <div class="empty">no data yet…</div>
          {:else}
            <div class="ghost-status">
              {#if ghostKeys.warmup_remaining > 0}
                calibrating — {ghostKeys.warmup_remaining} more keystrokes before short-dwell flagging
              {:else}
                flagging dwell &lt; <span class="num">{fmtMs(ghostKeys.dwell_threshold_ms)}</span> ms
              {/if}
            </div>
            <div class="ghost-totals">
              <span class="ghost-totals-label">total</span>
              <span class="num ghost-totals-val">{ghostKeys.total_ghost_events}</span>
              <span class="ghost-totals-detail">
                short-dwell <span class="num">{ghostKeys.short_dwell_count}</span>
                · rapid-repeat <span class="num">{ghostKeys.rapid_repeat_count}</span>
                · self-corrected <span class="num">{ghostKeys.self_corrected_count}</span>
              </span>
            </div>

            <div class="ghost-sub">per hand · signature breakdown</div>
            <div class="ghost-hand-table">
              <div class="ghost-hand-row ghost-hand-head">
                <span class="col-ghh">hand</span>
                <span class="col-ghe num">events</span>
                <span class="col-ghs num">short</span>
                <span class="col-ghr num">repeat</span>
                <span class="col-ghc num">self</span>
              </div>
              {#each ghostKeys.per_hand as h (h.hand)}
                <div
                  class="ghost-hand-row"
                  class:finger-left={h.hand === "left"}
                  class:finger-right={h.hand === "right"}
                >
                  <span class="col-ghh">{h.hand === "left" ? "L" : "R"}</span>
                  <span class="col-ghe num">{h.events}</span>
                  <span class="col-ghs num">{h.short_dwell}</span>
                  <span class="col-ghr num">{h.rapid_repeat}</span>
                  <span class="col-ghc num">{h.self_corrected}</span>
                </div>
              {/each}
            </div>

            <div class="ghost-sub">per finger · anatomical</div>
            {#if ghostKeys.per_finger.length === 0}
              <div class="empty">none yet…</div>
            {:else}
              <div class="ghost-finger-table">
                {#each ghostKeys.per_finger as f (`${f.hand}-${f.finger}`)}
                  <div
                    class="ghost-finger-row"
                    class:finger-left={f.hand === "left"}
                    class:finger-right={f.hand === "right"}
                  >
                    <span class="col-gff">{fmtFinger(f.hand, f.finger)}</span>
                    <span class="col-gfn num">{f.events}</span>
                  </div>
                {/each}
              </div>
            {/if}

            <div class="ghost-sub">per key · worst first</div>
            {#if ghostKeys.per_key.length === 0}
              <div class="empty">none yet…</div>
            {:else}
              <div class="ghost-key-table">
                <div class="ghost-key-row ghost-key-head">
                  <span class="col-gk">key</span>
                  <span class="col-gf">finger</span>
                  <span class="col-ge num">events</span>
                  <span class="col-gs num">short</span>
                  <span class="col-gr num">repeat</span>
                  <span class="col-gc num">self</span>
                </div>
                {#each ghostKeys.per_key.slice(0, GHOST_PER_KEY_LIMIT) as r (r.key)}
                  <div class="ghost-key-row">
                    <span class="col-gk">{fmtKey(r.key)}</span>
                    <span class="col-gf">{fmtKeyFinger(r)}</span>
                    <span class="col-ge num">{r.events}</span>
                    <span class="col-gs num">{r.short_dwell}</span>
                    <span class="col-gr num">{r.rapid_repeat}</span>
                    <span class="col-gc num">{r.self_corrected}</span>
                  </div>
                {/each}
                {#if ghostKeys.per_key.length > GHOST_PER_KEY_LIMIT}
                  <div class="ghost-more">
                    + {ghostKeys.per_key.length - GHOST_PER_KEY_LIMIT} more keys with ghost activity
                  </div>
                {/if}
              </div>
            {/if}
          {/if}
        </div>

        <div class="model-sub model-sub-sticky">PER KEY · most-typed first</div>
        <div class="model-table">
          <div class="model-row model-head">
            <span class="col-mk">key</span>
            <span class="col-mf">finger</span>
            <span class="col-mn num">count</span>
            <span class="col-md num">avg dwell</span>
            <span class="col-mi num">avg interval</span>
          </div>
          {#if modelRows.length === 0}
            <div class="empty">no keys observed yet…</div>
          {:else}
            {#each modelRows as r (r.key)}
              <div class="model-row">
                <span class="col-mk">{fmtKey(r.key)}</span>
                <span class="col-mf">{fmtKeyFinger(r)}</span>
                <span class="col-mn num">{r.count}</span>
                <span class="col-md num">{fmtMs(r.avg_dwell_ms)} ms</span>
                <span class="col-mi num">{fmtMs(r.avg_interval_ms)} ms</span>
              </div>
            {/each}
          {/if}
        </div>

        <!-- C5c Layer A — MOTOR BASELINE. Per-finger reliability +
             EWMA-pooled dwell/IKI means + rolling per-keystroke
             anomaly probes. Observe-only this phase. -->
        <div class="model-sub model-sub-sticky">
          MOTOR BASELINE · 5c layer A · observe-only ·
          <span class="motor-half-life">half-life ~5000 keystrokes</span>
        </div>
        <div class="motor-block">
          <div class="motor-table">
            <div class="motor-row motor-head">
              <span class="col-mof">finger</span>
              <span class="col-mor">reliability</span>
              <span class="col-mon num">n_eff</span>
              <span class="col-mod num">dwell μ</span>
              <span class="col-moi num">IKI μ</span>
            </div>
            {#each motorBaseline.per_finger as f (`${f.hand}-${f.finger}`)}
              <div
                class="motor-row"
                class:motor-left={f.hand === "left"}
                class:motor-right={f.hand === "right"}
                class:motor-cold={f.n_eff === 0}
              >
                <span class="col-mof">{fmtFinger(f.hand, f.finger)}</span>
                <span class="col-mor">
                  {#if f.n_eff > 0}
                    <span
                      class="motor-rel-bar"
                      style:width="{(f.reliability_score * 100).toFixed(0)}%"
                    ></span>
                    <span class="motor-rel-num num">{(f.reliability_score * 100).toFixed(0)}%</span>
                  {:else}
                    <span class="motor-rel-dash">—</span>
                  {/if}
                </span>
                <span class="col-mon num">{f.n_eff.toFixed(0)}</span>
                <span class="col-mod num">
                  {f.dwell_mean_ms === null ? "—" : `${fmtMs(f.dwell_mean_ms)} ms`}
                </span>
                <span class="col-moi num">
                  {f.iki_mean_ms === null ? "—" : `${fmtMs(f.iki_mean_ms)} ms`}
                </span>
              </div>
            {/each}
          </div>

          <!-- Rolling per-keystroke anomaly log. Newest first. -->
          <div class="motor-recent">RECENT KEYSTROKES · newest first</div>
          {#if motorKeystrokes.length === 0}
            <div class="empty">no keystrokes scored yet…</div>
          {:else}
            <div class="motor-keys-table">
              <div class="motor-keys-row motor-keys-head">
                <span class="col-mkk">key</span>
                <span class="col-mkc num" title="combined anomaly = max(dwell, IKI, co-activation)">combined</span>
                <span class="col-mkd num" title="dwell anomaly: 1 - exp(-z²/2)">dwell</span>
                <span class="col-mki num" title="IKI anomaly (null on first keystroke / after pause)">IKI</span>
                <span class="col-mko num" title="co-activation overlap in ms (null if no overlap)">co-act</span>
                <span class="col-mkr num" title="certainty: how much data backs this score">cert</span>
              </div>
              {#each motorKeystrokes as k (k.timestamp_ms)}
                <div class="motor-keys-row" class:motor-anom={k.anomaly !== null && k.anomaly.combined > 0.5}>
                  <span class="col-mkk">{fmtKey(k.key)}</span>
                  <span class="col-mkc num">{k.anomaly === null ? "—" : k.anomaly.combined.toFixed(2)}</span>
                  <span class="col-mkd num">{k.anomaly === null ? "—" : k.anomaly.dwell.anomaly.toFixed(2)}</span>
                  <span class="col-mki num">{k.anomaly?.iki ? k.anomaly.iki.anomaly.toFixed(2) : "—"}</span>
                  <span class="col-mko num">{k.anomaly?.coactivation ? `${k.anomaly.coactivation.overlap_ms}ms` : "—"}</span>
                  <span class="col-mkr num">{k.anomaly === null ? "—" : k.anomaly.certainty.toFixed(2)}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>
      </div>
    </section>
  </div>
</section>

<style>
  /* Utilitarian on purpose — this panel is a builder tool, not part of the
     calm user surface. Monospace, dense, dark background, terminal feel. */
  .debug {
    position: fixed;
    inset: auto 0 0 0;
    /* height is set inline from `panelHeight`; CSS only declares the floor. */
    min-height: 220px;
    background: #0b0d10;
    color: #e6e6e6;
    border-top: 1px solid #2a2f36;
    display: flex;
    flex-direction: column;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px;
    /* Above app content; below any future native menus. */
    z-index: 9999;
    box-shadow: 0 -8px 24px rgba(0, 0, 0, 0.4);
  }

  /* ---- Resize handle (top edge) ---------------------------------------- */
  .resize-handle {
    height: 10px;
    flex: 0 0 10px;
    cursor: ns-resize;
    background: #15191f;
    border-bottom: 1px solid #2a2f36;
    display: flex;
    align-items: center;
    justify-content: center;
    touch-action: none; /* let pointer events drive the drag, not scroll */
  }
  .resize-handle:hover,
  .resize-handle.dragging {
    background: #1c222a;
  }
  .resize-grip {
    width: 48px;
    height: 3px;
    background: #3a414a;
    border-radius: 2px;
  }
  .resize-handle:hover .resize-grip,
  .resize-handle.dragging .resize-grip {
    background: #5a6470;
  }

  /* ---- Top stats strip -------------------------------------------------- */
  .strip {
    flex: 0 0 auto;
    display: flex;
    align-items: center;
    gap: 1.5rem;
    padding: 0.55rem 0.9rem;
    background: #15191f;
    border-bottom: 1px solid #2a2f36;
  }
  .stat {
    display: flex;
    align-items: baseline;
    gap: 0.45rem;
  }
  .stat-label {
    color: #7f8a96;
    letter-spacing: 0.08em;
  }
  .stat-val {
    color: #e6e6e6;
    font-weight: 600;
  }
  .spacer { flex: 1; }
  .clear {
    font: inherit;
    color: #e6e6e6;
    background: #2a2f36;
    border: 1px solid #3a414a;
    border-radius: 4px;
    padding: 0.3rem 0.85rem;
    cursor: pointer;
  }
  .clear:hover { background: #353c45; }
  .clear:focus { outline: 2px solid var(--focus-ring); outline-offset: 1px; }

  /* ---- C5c motor stability readout (always-visible bar) ---------------- */
  .motor-readout {
    flex: 0 0 auto;
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 0.45rem 0.9rem;
    padding: 0.4rem 0.9rem;
    background: #12161b;
    border-bottom: 1px solid #2a2f36;
    color: #c9d1d9;
  }
  .motor-stat { color: #7f8a96; }
  .motor-stat b { color: #e6e6e6; font-weight: 600; }
  .motor-sep { color: #3a414a; }
  .motor-key {
    color: #e6e6e6;
    background: #2a2f36;
    border: 1px solid #3a414a;
    border-radius: 4px;
    padding: 0.05rem 0.4rem;
    font-variant-numeric: tabular-nums;
  }
  .motor-empty { color: #7f8a96; font-style: italic; }

  /* ---- Two equal columns: feed | model state --------------------------- */
  .body {
    flex: 1;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    min-height: 0; /* allow children to shrink-and-scroll instead of overflowing */
  }
  .col {
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
  }
  .col-feed { border-right: 1px solid #2a2f36; }
  .col-model { background: #0d1117; }
  /* Section banner — one shared style so both columns read as peers. */
  .col-head {
    flex: 0 0 auto;
    padding: 0.45rem 0.75rem;
    color: #cdd5de;
    letter-spacing: 0.08em;
    font-weight: 600;
    background: #11161c;
    border-bottom: 1px solid #2a2f36;
  }

  /* ---- Feed (scrolls independently) ------------------------------------ */
  .feed {
    flex: 1;
    overflow-y: auto;
    padding: 0.4rem 0.55rem 0.6rem 0.55rem;
  }
  .empty {
    color: #7f8a96;
    padding: 0.55rem 0.75rem;
    font-style: italic;
  }
  .row {
    display: grid;
    grid-template-columns: 70px auto minmax(0, 1fr) auto;
    gap: 0.85rem;
    align-items: baseline;
    padding: 0.25rem 0.45rem;
    border-radius: 3px;
    white-space: nowrap;
  }
  .row-key { grid-template-columns: 70px auto minmax(0, 1fr) auto; }
  .row:nth-child(even) { background: rgba(255, 255, 255, 0.025); }

  .tag {
    color: #7f8a96;
    letter-spacing: 0.07em;
  }
  /* Distinct background tints + text labels — never color alone. */
  .badge {
    padding: 0 0.45rem;
    border-radius: 3px;
    letter-spacing: 0.07em;
    font-weight: 600;
  }
  .badge-corrected {
    background: #1f3a1f;
    color: #b6e3b6;
    border: 1px solid #2e5a2e;
  }
  .badge-left {
    background: #1f1f1f;
    color: #c8c8c8;
    border: 1px solid #3a3a3a;
  }
  /* Slip candidate — distinct amber so it stands apart from CORRECTED green
     and LEFT-ALONE grey, but text label still carries the meaning. */
  .badge-slip {
    background: #3a2f1a;
    color: #e6c98a;
    border: 1px solid #6a5320;
  }

  .col-key { color: #e6e6e6; font-weight: 600; }
  .col-dwell, .col-lat { color: #8aa1b8; }
  .col-word { color: #d5d5d5; overflow: hidden; text-overflow: ellipsis; }

  /* Subtle left border on would-correct rows so the eye can scan correction
     events without relying on the green badge. */
  .row-matched {
    border-left: 2px solid #2e5a2e;
    padding-left: 0.6rem;
  }
  /* Same trick for SLIP rows — amber rail mirrors the badge colour but the
     text tag is still load-bearing. */
  .row-slip {
    grid-template-columns: 70px auto minmax(0, 1fr) auto;
    border-left: 2px solid #6a5320;
    padding-left: 0.6rem;
  }

  /* ---- Model state column --------------------------------------------- */
  /* Pinned section — never scrolls. Sized by content. */
  .model-pinned {
    flex: 0 0 auto;
    border-bottom: 1px solid #2a2f36;
    background: #0d1117;
  }
  /* Scrolling section — flexes to fill, scrolls when needed. */
  .model-scroll {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
    background: #0d1117;
  }
  .model-sub {
    padding: 0.45rem 0.75rem 0.25rem;
    color: #8893a0;
    letter-spacing: 0.08em;
    font-size: 11px;
  }
  .model-sub-sticky {
    position: sticky;
    top: 0;
    background: #0d1117;
    z-index: 1;
  }

  /* Asymmetry block — left vs right rollup + single score + plain-language read.
     Sits above PER FINGER inside .model-pinned (always visible). */
  .asym-table {
    padding: 0 0 0.4rem;
    border-bottom: 1px solid #2a2f36;
  }
  .asym-row {
    display: grid;
    grid-template-columns: 32px 60px minmax(0, 1fr) minmax(0, 1fr);
    column-gap: 0.6rem;
    align-items: baseline;
    padding: 0.22rem 0.75rem;
    white-space: nowrap;
  }
  .asym-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.3rem;
    margin-bottom: 0.15rem;
  }
  /* Same hand tints as PER FINGER so left/right map visually between the
     two tables. Letter labels ("L"/"R") still carry the meaning so it's
     never colour-only. */
  .asym-left:not(.asym-head) { background: rgba(120, 160, 220, 0.045); }
  .asym-right:not(.asym-head) { background: rgba(220, 160, 120, 0.045); }
  .col-ah { color: #e6e6e6; font-weight: 600; }
  .col-an { color: #d5d5d5; }
  .col-ad, .col-ai { color: #8aa1b8; }

  .asym-score {
    display: flex;
    align-items: baseline;
    gap: 0.5rem;
    padding: 0.35rem 0.75rem 0.1rem;
    border-top: 1px dashed #2a2f36;
    margin-top: 0.1rem;
  }
  .asym-score-label {
    color: #7f8a96;
    letter-spacing: 0.08em;
  }
  .asym-score-val {
    color: #e6e6e6;
    font-weight: 600;
  }
  .asym-score-detail {
    color: #6a747f;
    font-size: 11px;
  }
  .asym-readout {
    color: #cdd5de;
    padding: 0.1rem 0.75rem 0.3rem;
  }

  /* Per-finger rollup table. */
  .finger-table {
    padding: 0 0 0.35rem;
  }
  .finger-row {
    display: grid;
    grid-template-columns: 90px 60px minmax(0, 1fr) minmax(0, 1fr);
    column-gap: 0.6rem;
    align-items: baseline;
    padding: 0.22rem 0.75rem;
    white-space: nowrap;
  }
  .finger-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.3rem;
    margin-bottom: 0.15rem;
  }
  /* Subtle hand grouping — slightly different row tints so left/right read as
     two clusters at a glance, but "L"/"R" labels still carry the meaning so it
     isn't color-only. */
  .finger-left:not(.finger-head) { background: rgba(120, 160, 220, 0.045); }
  .finger-right:not(.finger-head) { background: rgba(220, 160, 120, 0.045); }
  .col-ff { color: #e6e6e6; font-weight: 600; }
  .col-fn { color: #d5d5d5; }
  .col-fd, .col-fi { color: #8aa1b8; }

  /* TOKENS — L4 Observing brief, Component 1. Live tokens for the current
     line; cleared on engine://line-reset. */
  .token-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .token-table {
    padding: 0 0 0.2rem;
  }
  .token-row {
    display: grid;
    grid-template-columns: minmax(0, 1.4fr) 64px 40px 70px 36px;
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .token-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .token-row:not(.token-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  /* Correctable tokens get a subtle green rail — text label "yes" still
     carries the meaning so this is supplementary, not color-only. */
  .token-correctable {
    border-left: 2px solid #2e5a2e;
    padding-left: calc(0.75rem - 2px);
  }
  .col-tc {
    color: #e6e6e6;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-tk { color: #8aa1b8; }
  .col-tx { color: #d5d5d5; }
  .col-ts { color: #8aa1b8; }
  .col-tt { color: #d5d5d5; }

  /* LEXICON — L4 Component 3a, read-only word source. One row per sealed
     Word token on the current line. "known" gets a quiet green rail; "no"
     gets an amber rail — text labels still carry the meaning, the rails
     are just glanceability. */
  .lex-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .lex-table {
    padding: 0 0 0.2rem;
  }
  .lex-row {
    display: grid;
    grid-template-columns: minmax(0, 1.2fr) 60px minmax(0, 1fr);
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .lex-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .lex-row:not(.lex-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .lex-known {
    border-left: 2px solid #2e5a2e;
    padding-left: calc(0.75rem - 2px);
  }
  .lex-unknown {
    border-left: 2px solid #6a5320;
    padding-left: calc(0.75rem - 2px);
  }
  .col-lw {
    color: #e6e6e6;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-lk { color: #d5d5d5; }
  .col-lf { color: #8aa1b8; }

  /* CANDIDATES — L4 Components 3b + 3c-1. Per unknown word: a header
     line (typed word + tier badge + top score) and a small table with
     per-candidate edit type, frequency, lexicon evidence, motor evidence,
     and score. Tier badge is display-only this pass; the decision path
     is unchanged. */
  .cand-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .cand-group {
    padding: 0.3rem 0.75rem 0.45rem;
  }
  .cand-group:nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .cand-header {
    display: flex;
    align-items: baseline;
    gap: 0.45rem;
    margin-bottom: 0.2rem;
  }
  .cand-src {
    color: #e6c98a;
    font-weight: 600;
  }
  .cand-arrow {
    color: #7f8a96;
  }
  .cand-empty {
    color: #7f8a96;
    font-style: italic;
  }
  .cand-top-score {
    color: #8aa1b8;
    margin-left: auto;
    font-variant-numeric: tabular-nums;
  }
  /* Confidence badges (3c-3) — colour-coded by candidate strength.
     High = green (strong), Medium = blue, Low = amber, none = grey.
     Text label carries the meaning; color is supplementary. */
  .cand-conf {
    padding: 0 0.45rem;
    border-radius: 3px;
    letter-spacing: 0.07em;
    font-weight: 600;
    font-size: 11px;
  }
  .cand-conf-high {
    background: #1f3a1f;
    color: #b6e3b6;
    border: 1px solid #2e5a2e;
  }
  .cand-conf-medium {
    background: #1f2a3a;
    color: #9bb4d6;
    border: 1px solid #2e4a6a;
  }
  .cand-conf-low {
    background: #3a2a1f;
    color: #e6c98a;
    border: 1px solid #6a5320;
  }
  .cand-conf-none {
    background: #1f1f1f;
    color: #7f8a96;
    border: 1px solid #3a3a3a;
    font-style: italic;
  }
  /* Mode pill in the strip — uses muted, "setting"-feeling colors that
     intentionally don't match the confidence palette. Modes and
     confidence are separate vocabularies (3c-3 contract). */
  .mode-pill {
    padding: 0 0.5rem;
    border-radius: 3px;
    text-transform: uppercase;
    font-size: 11px;
    letter-spacing: 0.08em;
    font-weight: 600;
  }
  .mode-pill-cautious {
    background: #2a2230;
    color: #c8b6e3;
    border: 1px solid #4a3e5a;
  }
  .mode-pill-balanced {
    background: #1f2a2f;
    color: #9bd6c9;
    border: 1px solid #2e4f4a;
  }
  .mode-pill-eager {
    background: #2f261f;
    color: #d6b69b;
    border: 1px solid #5a432e;
  }
  /* Capture-health pill. Same chrome as the mode pill but its own
     state palette (semantic colors so the dot's meaning is
     readable). Each state ALSO carries text — never color-only. */
  .capture-pill {
    padding: 0 0.5rem;
    border-radius: 3px;
    text-transform: uppercase;
    font-size: 11px;
    letter-spacing: 0.08em;
    font-weight: 600;
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
  }
  .capture-dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: currentColor;
    box-shadow: 0 0 4px currentColor;
  }
  .capture-pill-unknown {
    background: #2a2f36;
    color: #7f8a96;
    border: 1px solid #3a414a;
  }
  .capture-pill-live {
    background: #1f2a23;
    color: #6ea76e;
    border: 1px solid #2e4a35;
  }
  .capture-pill-unhealthy {
    background: #2f261f;
    color: #d2885d;
    border: 1px solid #5a3a2a;
  }
  .capture-pill-stopped {
    background: #2f1f22;
    color: #c8553d;
    border: 1px solid #5a2a2e;
    /* Stopped is the must-act state — gently pulse so a glance
       catches it even if the user wasn't looking. */
    animation: capture-stopped-pulse 1.6s ease-in-out infinite;
  }
  @keyframes capture-stopped-pulse {
    0%, 100% { box-shadow: 0 0 0 0 rgba(200, 85, 61, 0); }
    50%      { box-shadow: 0 0 0 2px rgba(200, 85, 61, 0.25); }
  }

  /* Restart capture button — always visible, always clickable. The
     elevated variant (amber border) fires when CAPTURE is anything
     other than "live" so the recovery path stands out exactly when
     it's needed. Stays the same color even in Stopped state — never
     dimmed, never disabled, by intent: when everything else looks
     dead, this is the path forward. */
  .restart-capture {
    font: inherit;
    font-size: 11px;
    color: #e6e6e6;
    background: #2a2f36;
    border: 1px solid #3a414a;
    border-radius: 4px;
    padding: 0.2rem 0.6rem;
    cursor: pointer;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    font-weight: 600;
  }
  .restart-capture:hover { background: #353c45; }
  .restart-capture:focus { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
  .restart-capture-elevated {
    background: #3a2b22;
    color: #f0c0a0;
    border-color: #c8553d;
    box-shadow: 0 0 0 1px rgba(200, 85, 61, 0.4);
  }
  .restart-capture-elevated:hover { background: #4a352a; }

  .mode-pill-none {
    background: #2a2f36;
    color: #7f8a96;
    border: 1px solid #3a414a;
    font-style: italic;
  }
  /* Per-candidate score table — fixed columns, tabular numerals so the
     numeric stacks line up. */
  .score-table {
    margin-top: 0.15rem;
  }
  .score-row {
    display: grid;
    grid-template-columns: minmax(0, 1.2fr) 88px 56px 56px 60px 56px;
    column-gap: 0.4rem;
    align-items: baseline;
    padding: 0.18rem 0.3rem;
    white-space: nowrap;
  }
  .score-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.2rem;
    margin-bottom: 0.1rem;
    font-size: 11px;
    letter-spacing: 0.06em;
  }
  /* Top candidate gets a subtle green rail — supplementary; the row
     position (first) and the tier badge above carry the meaning. */
  .score-top {
    border-left: 2px solid #2e5a2e;
    padding-left: calc(0.3rem - 2px);
    background: rgba(46, 90, 46, 0.08);
  }
  .col-sw { color: #b6e3b6; font-weight: 600; overflow: hidden; text-overflow: ellipsis; }
  .col-se { color: #8aa1b8; }
  .col-sx { color: #8aa1b8; }
  .col-sl, .col-sm { color: #d5d5d5; }
  .col-ss { color: #e6e6e6; font-weight: 600; }

  /* LOG — L4 Observing, Component 4. Structured decision history with
     outcome slots. Session-spanning; not cleared on line-reset. Reuses
     the confidence-badge palette from CANDIDATES so the conf cells read
     identically across sections. */
  .log-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .log-status {
    color: #8893a0;
    padding: 0.15rem 0.75rem 0.3rem;
    font-size: 11px;
  }
  .log-table {
    padding: 0 0 0.2rem;
  }
  .log-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1.6fr) 56px 52px 56px minmax(0, 1fr);
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .log-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .log-row:not(.log-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  /* Visual rails mirror the FEED's would-correct (green) / leave-alone
     (grey) treatment so the eye can scan decision shape without parsing
     each cell. Text in the "decision" column still carries the meaning. */
  .log-would {
    border-left: 2px solid #2e5a2e;
    padding-left: calc(0.75rem - 2px);
  }
  .log-leave {
    border-left: 2px solid #3a3a3a;
    padding-left: calc(0.75rem - 2px);
  }
  .col-lo {
    color: #e6e6e6;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-ld {
    color: #d5d5d5;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-lc { color: #8aa1b8; }
  .col-lb { /* shares cand-conf-* badge styling */
    text-align: center;
  }
  .col-la { color: #8aa1b8; }
  .col-lx { color: #d5d5d5; }

  /* LEXICON proposals (Component 5b) — same visual family as LOG but its
     own column shape: word | lane | motor verdict | motor score | tier |
     occasions. Held rows fade slightly; confirmed rows get a green rail
     to mirror the "promote" intent. is_known is untouched in Phase 1 — the
     panel only exposes what the proposer would judge. */
  .lex-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .lex-status {
    color: #8893a0;
    padding: 0.15rem 0.75rem 0.3rem;
    font-size: 11px;
  }
  .lex-table {
    padding: 0 0 0.2rem;
  }
  .lex-row {
    display: grid;
    grid-template-columns:
      minmax(0, 1.2fr) /* word */
      minmax(0, 1.1fr) /* lane */
      52px            /* motor */
      44px            /* slip */
      48px            /* plaus */
      72px            /* prox */
      48px            /* web */
      minmax(0, 1.3fr) /* tier */
      32px;           /* × */
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .lex-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .lex-row:not(.lex-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .lex-held {
    opacity: 0.65;
  }
  .lex-confirmed {
    border-left: 2px solid #2e5a2e;
    padding-left: calc(0.75rem - 2px);
  }
  .col-lxw {
    color: #e6e6e6;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-lxl {
    color: #d5d5d5;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-lxm { text-align: center; }
  .col-lxs { color: #8aa1b8; }
  .col-lxt {
    color: #d5d5d5;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-lxo { color: #8aa1b8; }
  /* Motor-verdict colors mirror the cand-conf badge palette so the eye
     reads "clean = green-ish, slip = warning" at a glance. */
  .lex-motor-clean  { color: #6ea76e; font-weight: 600; }
  .lex-motor-mixed  { color: #d2c87b; font-weight: 600; }
  .lex-motor-slip   { color: #d2885d; font-weight: 600; }
  .lex-motor-unknown { color: #7f8a96; }
  /* Proximity-verdict colors: far = green (clear to promote);
     non-far = warning shades (a gate would fire). */
  .col-lxp { color: #8aa1b8; }
  .col-lxx { color: #d5d5d5; }
  .lex-prox-far_from_known   { color: #6ea76e; }
  .lex-prox-near_known_edit2 { color: #d2885d; font-weight: 600; }
  .lex-prox-segmentable      { color: #d2885d; font-weight: 600; }
  .lex-prox-prefix_merge     { color: #d2c87b; font-weight: 600; }
  /* Norvig column — '—' (no web presence) renders in the same warning
     shade as near-known proximity so the eye reads the combo at a glance. */
  .col-lxn { color: #8aa1b8; }
  .lex-no-web { color: #d2885d; font-weight: 600; }
  /* Casing baseline annotation — green when the rescue is firing for
     this user, amber when suppressed (all-caps is their norm or a
     burst is in progress). */
  .casing-rescue-on  { color: #6ea76e; font-weight: 600; }
  .casing-rescue-off { color: #d2885d; font-weight: 600; }
  .casing-samples    { color: #7f8a96; }

  /* LEXICON sub-header lays out as label + right-aligned Reset button. */
  .lex-sub {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .lex-sub-label { flex: 1; min-width: 0; }
  /* Reset LEXICON button — engine-side wipe. Sized like other buttons in
     the strip but flagged amber so it's not confused with the panel-side
     Clear (different semantics: this kills learned vocabulary). */
  .lex-reset {
    font: inherit;
    font-size: 11px;
    color: #e6e6e6;
    background: #3a2b22;
    border: 1px solid #5a3a2a;
    border-radius: 4px;
    padding: 0.2rem 0.6rem;
    cursor: pointer;
  }
  .lex-reset:hover { background: #4a352a; }
  .lex-reset:focus { outline: 2px solid var(--focus-ring); outline-offset: 1px; }

  /* Pause learning button — same chrome as Reset LEXICON but a quieter
     palette since the action is reversible and frequent. When manually
     ON, swap to an amber background so the running state is obvious. */
  .lex-pause {
    font: inherit;
    font-size: 11px;
    color: #e6e6e6;
    background: #2a2f36;
    border: 1px solid #3a414a;
    border-radius: 4px;
    padding: 0.2rem 0.6rem;
    cursor: pointer;
  }
  .lex-pause:hover { background: #353c45; }
  .lex-pause:focus { outline: 2px solid var(--focus-ring); outline-offset: 1px; }
  .lex-pause-on {
    background: #3a2b22;
    border-color: #5a3a2a;
  }
  .lex-pause-on:hover { background: #4a352a; }
  /* When capture is Stopped, the Pause buttons get the disabled
     attribute + a visual de-emphasis so the user sees the difference
     between "click did nothing" (pre-fix) and "this control is not
     available right now; Restart capture is what you need." */
  .lex-pause-stuck {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .lex-pause-stuck:hover { background: #2a2f36; }
  /* Quiet inline tag rendered next to the casing baseline when the
     engine reports learning is currently paused. */
  .lex-paused-tag {
    color: #d2885d;
    font-weight: 600;
  }
  /* Stronger tone — hard pause (engine asleep) is more severe than
     soft pause (engine watching but not crediting). */
  .lex-input-paused-tag {
    color: #c8553d;
    font-weight: 700;
  }

  /* C5b Phase 2 — LIVE indicator on confirmed proposals + star
     marker on LOG records targeting learned words. */
  .lex-live {
    color: #6ea76e;
    font-weight: 600;
    font-size: 10px;
    margin-left: 0.35rem;
    letter-spacing: 0.5px;
  }
  .log-learned-marker {
    color: #d2c87b;
    font-weight: 600;
    font-size: 10px;
    margin-left: 0.35rem;
  }
  .log-learned-target {
    border-left: 2px solid #c89a3d;
    padding-left: calc(0.75rem - 2px);
  }
  /* LOG_VERSION v5 — Kept records that landed while learning was
     paused. The row text dims to make it visually obvious the
     record was OBSERVED but did NOT contribute; a small inline tag
     in the outcome column carries the explicit "paused" label so
     the dim isn't ambiguous. */
  .log-paused {
    opacity: 0.55;
  }
  .log-paused-tag {
    margin-left: 0.4rem;
    padding: 0 0.3rem;
    background: #3a2b22;
    color: #d2885d;
    border: 1px solid #5a3a2a;
    border-radius: 3px;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  /* SLIPS section header annotation for the C5b L3-map kill-switch.
     Bright when live, dim/warning when off. */
  .slips-writes-on  { color: #6ea76e; }
  .slips-writes-off { color: #d2885d; }

  /* ANCHORS — L4 Observing, Component 2. Sibling of TOKENS; one row per
     live anchor on the current line. Voided rows get a faded look but
     stay in the list (the state label carries the meaning). */
  .anchor-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .anchor-status {
    color: #8893a0;
    padding: 0.15rem 0.75rem 0.3rem;
    font-size: 11px;
  }
  .anchor-void-count {
    color: #e6c98a;
    font-weight: 600;
  }
  .anchor-table {
    padding: 0 0 0.2rem;
  }
  .anchor-row {
    display: grid;
    grid-template-columns: minmax(0, 1.1fr) 70px minmax(0, 1.1fr) 96px;
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .anchor-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .anchor-row:not(.anchor-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .anchor-voided {
    color: #7f8a96;
  }
  .anchor-voided .col-ao,
  .anchor-voided .col-an,
  .anchor-voided .col-as {
    text-decoration: line-through;
    text-decoration-thickness: 1px;
  }
  .col-ao {
    color: #e6e6e6;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-as { color: #8aa1b8; }
  .col-an {
    color: #d5d5d5;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .col-ast {
    color: #d5d5d5;
    font-weight: 600;
  }
  .anchor-voided .col-ast { color: #e6c98a; }

  /* SLIPS — first L3 learning loop. Status + per-pair table.
     Sits at the very top of .model-scroll. */
  .slip-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .slip-status {
    color: #8893a0;
    padding: 0.15rem 0.75rem 0.4rem;
    font-size: 11px;
  }
  .slip-total {
    color: #e6c98a;
    font-weight: 600;
  }
  .slip-table {
    padding: 0 0 0.2rem;
  }
  .slip-row {
    display: grid;
    grid-template-columns: 44px 44px minmax(0, 1fr) 56px;
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .slip-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .slip-row:not(.slip-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .col-sa { color: #e6c98a; font-weight: 600; }
  .col-sh { color: #d5d5d5; }
  .col-sf { color: #8aa1b8; }
  .col-sn { color: #e6e6e6; font-weight: 600; }

  /* GHOST KEYS — totals header + per-finger rollup + per-key worst-first.
     Sits at the top of .model-scroll, above PER KEY. */
  .ghost-block {
    padding: 0.25rem 0 0.5rem;
    border-bottom: 1px solid #2a2f36;
  }
  .ghost-status {
    color: #8893a0;
    padding: 0.15rem 0.75rem 0.3rem;
    font-size: 11px;
  }
  .ghost-totals {
    display: grid;
    grid-template-columns: auto auto 1fr;
    column-gap: 0.6rem;
    align-items: baseline;
    padding: 0.1rem 0.75rem 0.4rem;
  }
  .ghost-totals-label {
    color: #7f8a96;
    letter-spacing: 0.08em;
  }
  .ghost-totals-val {
    color: #e6e6e6;
    font-weight: 600;
  }
  .ghost-totals-detail {
    color: #8aa1b8;
    font-size: 11px;
  }
  .ghost-sub {
    padding: 0.4rem 0.75rem 0.1rem;
    color: #6a747f;
    letter-spacing: 0.08em;
    font-size: 11px;
  }
  /* Per-hand × per-signature ghost breakdown. Two rows (L, R) reusing the
     same hand tints as PER FINGER / ASYMMETRY for visual continuity. */
  .ghost-hand-table {
    padding: 0 0 0.2rem;
  }
  .ghost-hand-row {
    display: grid;
    grid-template-columns: 32px 56px 48px 56px 44px;
    column-gap: 0.5rem;
    align-items: baseline;
    padding: 0.18rem 0.75rem;
    white-space: nowrap;
  }
  .ghost-hand-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .col-ghh { color: #e6e6e6; font-weight: 600; }
  .col-ghe { color: #e6e6e6; font-weight: 600; }
  .col-ghs, .col-ghr, .col-ghc { color: #8aa1b8; }

  /* Per-finger ghost mini-table (one number per finger). */
  .ghost-finger-table {
    padding: 0 0 0.2rem;
  }
  .ghost-finger-row {
    display: grid;
    grid-template-columns: 90px minmax(0, 1fr);
    column-gap: 0.6rem;
    align-items: baseline;
    padding: 0.18rem 0.75rem;
    white-space: nowrap;
  }
  .col-gff { color: #e6e6e6; font-weight: 600; }
  .col-gfn { color: #d5d5d5; }
  /* Per-key ghost table (worst offenders). */
  .ghost-key-table {
    padding: 0 0 0.2rem;
  }
  .ghost-key-row {
    display: grid;
    grid-template-columns: 36px 80px 56px 44px 48px 44px;
    column-gap: 0.4rem;
    align-items: baseline;
    padding: 0.18rem 0.75rem;
    white-space: nowrap;
  }
  .ghost-key-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.25rem;
    margin-bottom: 0.1rem;
  }
  .ghost-key-row:not(.ghost-key-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .col-gk { color: #e6e6e6; font-weight: 600; }
  .col-gf { color: #8aa1b8; }
  .col-ge { color: #e6e6e6; font-weight: 600; }
  .col-gs, .col-gr, .col-gc { color: #8aa1b8; }
  .ghost-more {
    color: #6a747f;
    font-style: italic;
    padding: 0.25rem 0.75rem;
  }

  /* Per-key table — scrolls inside .model-scroll. Header is sticky inside it. */
  .model-table {
    padding: 0.25rem 0 0.4rem;
  }
  .model-row {
    display: grid;
    grid-template-columns: 36px 84px 56px minmax(0, 1fr) minmax(0, 1fr);
    column-gap: 0.55rem;
    align-items: baseline;
    padding: 0.22rem 0.75rem;
    white-space: nowrap;
  }
  .model-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-top: 0.25rem;
    padding-bottom: 0.3rem;
  }
  .model-row:not(.model-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  .col-mk { color: #e6e6e6; font-weight: 600; }
  .col-mf { color: #8aa1b8; }
  .col-mn { color: #d5d5d5; }
  .col-md, .col-mi { color: #8aa1b8; }

  /* Right-align every numeric column across both tables. Keeps the digit
     stacks lined up so the gradient is easy to read. Tabular numerals so
     proportional fonts don't reshape the column on each update. */
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  /* MOTOR BASELINE — C5c Layer A. Per-finger reliability + rolling
     per-keystroke anomaly probes. Sticky sub-header matches the
     other model sections; uses the same left/right hand tints as
     PER FINGER so they read as a family. */
  .motor-half-life {
    color: #7f8a96;
    font-weight: 400;
  }
  .motor-block {
    padding: 0 0 0.6rem;
    border-bottom: 1px solid #2a2f36;
  }
  .motor-table { padding: 0 0 0.35rem; }
  .motor-row {
    display: grid;
    grid-template-columns: 90px minmax(120px, 1fr) 50px 70px 70px;
    column-gap: 0.6rem;
    align-items: baseline;
    padding: 0.22rem 0.75rem;
    white-space: nowrap;
  }
  .motor-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.3rem;
    margin-bottom: 0.15rem;
  }
  .motor-left:not(.motor-head)  { background: rgba(120, 160, 220, 0.045); }
  .motor-right:not(.motor-head) { background: rgba(220, 160, 120, 0.045); }
  .motor-cold { opacity: 0.55; }
  .col-mof { color: #e6e6e6; font-weight: 600; }
  .col-mod, .col-moi, .col-mon { color: #8aa1b8; }
  .col-mor {
    position: relative;
    height: 12px;
    background: rgba(255, 255, 255, 0.04);
    border-radius: 2px;
    overflow: hidden;
  }
  /* Reliability bar — green when high, fading to amber via opacity
     reduction handled by the .motor-cold row class. Width is set
     inline from `style:width=...`. */
  .motor-rel-bar {
    position: absolute;
    left: 0; top: 0; bottom: 0;
    background: #6ea76e;
    border-radius: 2px;
  }
  .motor-rel-num {
    position: absolute;
    right: 0.4rem;
    top: 0;
    color: #d5d5d5;
    font-size: 10px;
    line-height: 12px;
  }
  .motor-rel-dash {
    color: #7f8a96;
    font-style: italic;
  }

  .motor-recent {
    color: #7f8a96;
    padding: 0.4rem 0.75rem 0.3rem;
    border-top: 1px solid #1d2228;
    margin-top: 0.4rem;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .motor-keys-table { padding: 0 0 0.35rem; }
  .motor-keys-row {
    display: grid;
    grid-template-columns: 36px 70px 60px 60px 70px 50px;
    column-gap: 0.55rem;
    align-items: baseline;
    padding: 0.2rem 0.75rem;
    white-space: nowrap;
  }
  .motor-keys-head {
    color: #7f8a96;
    border-bottom: 1px solid #2a2f36;
    padding-bottom: 0.3rem;
  }
  .motor-keys-row:not(.motor-keys-head):nth-child(even) {
    background: rgba(255, 255, 255, 0.025);
  }
  /* Rows where the combined anomaly is high — visually flag them. */
  .motor-anom .col-mkc { color: #c8553d; font-weight: 700; }
  .col-mkk { color: #e6e6e6; font-weight: 600; }
  .col-mkc, .col-mkd, .col-mki, .col-mko, .col-mkr { color: #8aa1b8; }
</style>
