//! Engine host — spawns the Swift sidecar (L1) and runs the L4 capture →
//! tokenize → score → decide loop in-process. **Observe-only**: the engine
//! computes a real decision per sealed Word token but does not inject
//! anything (injection is Component 3c-3).
//!
//! L2 (`BehaviouralModel`) observes every keystroke; its `timing` aggregator
//! is the only one with real logic for now, the rest are safe no-ops. The
//! correction *decision* now runs the real L4 pipeline (lexicon → candidates
//! → score → decide). The tokenizer is the **single source of word-boundary
//! truth** — there is no parallel skeleton word-buffer.
//!
//! Two measurements per keystroke, so the debug view can attribute time:
//!   - `ingest_latency_ms`   — just the L2 dispatch (on the keystroke event)
//!   - `decide_time_ms`      — scoring + decision cost on a sealed token
//!
//! Events emitted to the debug panel:
//!   `engine://keystroke`           — every key (or backspace) + dwell + ingest cost
//!   `engine://decision`            — per sealed Word token, with would-correct / leave-alone outcome
//!   `engine://model-snapshot`      — L2 state after each ingested event
//!   `engine://slip`                — one event per confirmed slip (L3 learning loop)
//!   `engine://token`               — one event per sealed L4 token (Observing brief Component 1)
//!   `engine://line-reset`          — fired when the tokenizer line resets (newline / backspace rebuild / special key)
//!   `engine://anchor-snapshot`     — full `AnchorsSnapshot` after every anchor change (Observing brief Component 2)
//!   `engine://lexicon`             — per Word token: known? + frequency (Component 3a)
//!   `engine://candidates`          — per UNKNOWN Word token: scored edit-1 candidates + tier (Components 3b + 3c-1)
//!   `engine://log-record`          — per appended decision-ledger record (Component 4)
//!   `engine://log-record-updated`  — outcome transition for a ledger record (Component 5a). Revisable —
//!                                    the same record id may receive several updates as the user revisits the span.
//!   `engine://lexicon-proposal`    — per word, the C5b lexicon-learning proposal (lane, motor verdict,
//!                                    tier, occasions). Observe-only — does not write is_known yet.

use std::time::Instant;

use std::time::{SystemTime, UNIX_EPOCH};

use std::path::{Path, PathBuf};

use behavioural_model::{BehaviouralModel, InputEvent, OutboundCommand};
use correction_engine::{
    classify_slip, decide, has_motor_evidence, measure_token_motor, normalize_word,
    ranked_known_candidates, score_candidates, should_log, target_is_recordable, AnchorTracker,
    Confidence, ConfidenceTier, DecisionLedger, DecisionOutcome, GuessLedger, Lexicon,
    LexiconProposer, MotorLedger, MotorMap, ObserveReport, Outcome, OutcomeResolver,
    PatternReadiness, ScoredCandidate, SlipClass, SpellingVariant, StabilityReport, Token,
    TokenKind, Tokenizer, WordFreq, WordPatternStore, ACTIVE_TIER, CANDIDATES_VERSION,
    DECISION_VERSION, LEXICON_VERSION,
    MAX_PATTERN_EDIT_DISTANCE, MAX_PATTERN_LENGTH_DIFF, SCORE_VERSION,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;
use volatility_map::VolatilityMap;

pub const EVT_KEYSTROKE: &str = "engine://keystroke";
pub const EVT_DECISION: &str = "engine://decision";
pub const EVT_MODEL_SNAPSHOT: &str = "engine://model-snapshot";
/// Fired once per confirmed slip detected by L2's `SlipDetector` during
/// ingest. The debug panel marks these in the feed.
pub const EVT_SLIP: &str = "engine://slip";
/// Fired when the streaming tokenizer seals a token on the current line.
/// Payload is a [`Token`]. The debug panel's TOKENS section listens.
pub const EVT_TOKEN: &str = "engine://token";
/// Fired when the tokenizer resets to a new line — newline pressed, or the
/// caller rebuilt after a backspace. Payload is empty.
pub const EVT_LINE_RESET: &str = "engine://line-reset";
/// Fired after every change to L4's anchor tracker (register / edit-delta /
/// clear). Payload is an `AnchorsSnapshot`. Debug-view ANCHORS section
/// listens — it doesn't try to derive anchor state from individual edits.
pub const EVT_ANCHOR_SNAPSHOT: &str = "engine://anchor-snapshot";
/// Fired once per sealed correctable token (Word kind) with the L4 lexicon
/// lookup result. Read-only — Component 3a is a word source only; scoring
/// and correction come later. The debug-view LEXICON section listens.
pub const EVT_LEXICON: &str = "engine://lexicon";
/// Fired once per sealed UNKNOWN Word token with the top-N ranked known
/// edit-1 neighbours (Component 3b). Read-only — no score, no tier, no
/// correction. Known words don't emit this event (they're protected).
/// Empty `candidates` is a legitimate outcome and IS emitted, so the panel
/// can render "(no known candidates within edit-1)". The debug-view
/// CANDIDATES section listens.
pub const EVT_CANDIDATES: &str = "engine://candidates";
/// Fired once per appended decision-ledger record (Component 4). Payload
/// is the full `LogRecord` with `outcome = Pending`. The debug-view LOG
/// section listens and renders newest-at-bottom; the ledger itself is
/// in-memory and bounded — the panel keeps its own view, the engine is
/// the source of truth.
pub const EVT_LOG_RECORD: &str = "engine://log-record";
/// Fired by the Component 5a [`OutcomeResolver`] each time a record's
/// outcome transitions (Pending → Kept, Kept → CorrectedToOther, …).
/// Payload is the full updated `LogRecord`. **Revisable**: the same
/// record id may receive multiple updates as the user revisits the
/// span, and the panel must update in place (look up by `id`).
pub const EVT_LOG_RECORD_UPDATED: &str = "engine://log-record-updated";
/// Fired by the Component 5b [`LexiconProposer`] each time a word's
/// proposal state changes — a Kept resolution credits a contribution,
/// a revisable transition retracts one, etc. Payload shape:
/// `{ word: String, proposal: LexiconProposal | null }` — `null` means
/// the proposal was retracted (its only contributing record rolled
/// back). The LEXICON panel keys its table by `word` and applies the
/// update in place.
pub const EVT_LEXICON_PROPOSAL: &str = "engine://lexicon-proposal";
/// Fired after every fresh Word/Acronym seal: the proposer's casing
/// baseline (recency-weighted all-caps share + rescue-active flag).
/// The LEXICON panel renders this in its header so the user can see
/// whether the all-caps brand-name rescue is currently active. The
/// signal is global, not per-word.
pub const EVT_CASING_BASELINE: &str = "engine://casing-baseline";
/// **Component 5b Phase 2.** Fired whenever the runtime-learned
/// lexicon changes — a confirmed proposal entered `is_known`, or a
/// previously-confirmed word demoted out. Payload is the full
/// learned-words snapshot. **ZERO INJECTION:** the engine's
/// correction decisions read this through `Lexicon::is_known`, so
/// would-correct counts can rise (Krutrim becomes a correction
/// target), but no injection runs — this phase is the validation
/// gate.
pub const EVT_LEARNED_SNAPSHOT: &str = "engine://learned-snapshot";
/// Fired once when the engine processes a "Reset LEXICON" control
/// command. Tells the panel to wipe its mirror of `lexiconProposals`
/// (which is keyed by word and only ever updated in-place by per-word
/// proposal events — no implicit "clear all" signal otherwise).
/// Accompanied by a fresh empty `engine://learned-snapshot`. Payload
/// is empty.
pub const EVT_LEXICON_RESET: &str = "engine://lexicon-reset";
/// Fired whenever the proposer's meta-context pause flag changes. The
/// payload echoes the new state so the panel indicator reflects the
/// engine's actual flag (not just the panel's optimistic state).
/// Payload: `{ paused: bool }`. See [`EngineControl::SetLearningPaused`].
pub const EVT_LEARNING_PAUSED: &str = "engine://learning-paused";
/// Fired whenever the engine's HARD-pause flag changes. Hard pause
/// drops Key/Backspace events at the engine task boundary — before
/// model.ingest, before tokenization, before any emission. The FEED
/// freezes, the engine effectively sleeps. Distinct from learning
/// pause (which still observes). Payload: `{ paused: bool }`.
/// See [`EngineControl::SetInputPaused`].
pub const EVT_INPUT_PAUSED: &str = "engine://input-paused";
/// **C5c Layer A.** Per-finger motor baseline snapshot, emitted on
/// every keystroke. Payload is `MotorBaselineSnapshot` — 10 rows in
/// anatomical order, each with reliability + n_eff + dwell/IKI
/// means. The panel renders a stable per-finger table; rows with
/// n_eff == 0 render as dashes. Observe-only; no consumer reads
/// this for correction yet.
pub const EVT_MOTOR_BASELINE: &str = "engine://motor-baseline";
/// **C5c Layer A.** Per-keystroke anomaly probe, emitted before the
/// keystroke is ingested (so the anomaly reflects what the baseline
/// thought BEFORE this keystroke updated it). Payload includes the
/// key, its (hand, finger), and the full `KeystrokeAnomaly` with
/// dwell + IKI + co-activation dimensions. Skipped for non-typing
/// keys (`finger_for` returned `None`) — no anomaly to compute.
pub const EVT_MOTOR_KEYSTROKE: &str = "engine://motor-keystroke";
/// **C5c motor map.** The [`StabilityReport`] read-model — kill-switch
/// inputs (sample coverage, overall slip rate) plus the weakest-keys preview
/// Practice mode consumes. Emitted periodically by the watchdog and on
/// demand via [`EngineControl::RequestMotorStability`]. Exposes the data;
/// the kill-switch itself is not built.
pub const EVT_MOTOR_STABILITY: &str = "engine://motor-stability";
/// **Practice trend.** Per-key slip-rate series reconstructed from the daily
/// motor-map snapshots (`~/.typeassist/snapshots/*.json`) — the data behind the
/// Practice snapshot's "where these keys are heading". Emitted on demand via
/// [`EngineControl::RequestPracticeTrend`]. Daily granularity, and each key
/// carries only the days it had enough samples to be trustworthy, so
/// a key with <2 points simply renders no trend (graceful, never invented).
pub const EVT_PRACTICE_TREND: &str = "engine://practice-trend";
/// **C5 capture-health.** Engine-derived view of the sidecar's
/// capture state. Emitted whenever the state transitions — NOT on
/// every heartbeat. Panel renders a header pill so silent capture
/// death is visible mid-session (the long-session bug); a future
/// menu-bar surface will subscribe to the same event without a panel
/// rewrite. Payload is [`CaptureHealthEvent`].
pub const EVT_CAPTURE_HEALTH: &str = "engine://capture-health";
/// **Per-grant permission status for the permission rows.** Forwards the
/// sidecar's [`InputEvent::PermissionStatus`] verbatim — the two independent
/// macOS grants capture needs (Accessibility + Input Monitoring), each a
/// read-only no-prompt probe. Emitted on EVERY receipt (not transition-gated
/// like capture-health), so a freshly-opened onboarding or Reconnect window
/// converges to current truth within a poll cycle. Crucially this surfaces a
/// PARTIAL grant — which `EVT_CAPTURE_HEALTH`'s `live` can never show, because
/// the sidecar exits before its first heartbeat if either grant is missing —
/// so the UI can tick each permission row independently. `live` (both grants +
/// tap armed) stays the gate for "capture is actually running". Payload is
/// [`PermissionStatusEvent`].
pub const EVT_PERMISSION_STATUS: &str = "engine://permission-status";
/// **Debounced capture-active signal for the menu-bar UI.** `EVT_CAPTURE_HEALTH`
/// flips on every raw transition (a 2s tap-timeout blip flicks it to Unhealthy
/// and straight back), which would make the tray icon and status line strobe.
/// This event is the *settled* view: `active` only goes false once capture has
/// stayed non-`Live` continuously past the self-heal window (the sidecar
/// re-arms a disabled tap in ~2s; the watchdog auto-respawns a dead sidecar at
/// 15s) — see [`NOT_ACTIVE_DEBOUNCE_MS`]. Recovery to `Live` flips `active` back
/// true immediately (good news isn't debounced). `permission_revoked` tells the
/// menu which recovery action to surface: false → "Restart capture" (re-arm in
/// place), true → "Reconnect…" (the sidecar reported Accessibility missing, so
/// only re-granting recovers). Payload is [`CaptureUiEvent`].
pub const EVT_CAPTURE_UI: &str = "engine://capture-ui";
/// **M3 correction Step 1.** The current correction allow-list + master gate,
/// emitted whenever the engine mutates it (toggle from the panel/tray, or an
/// Escape teach-stop) and on an explicit `RequestAllowList`. The allow-list
/// panel and the tray master toggle render from this so the UI always reflects
/// the engine's authoritative state. Payload is [`crate::allow_list::AllowList`].
pub const EVT_CORRECTION_STATE: &str = "corrections://state";
/// **M3 correction bubble.** A confident correction is being SUGGESTED for the
/// just-sealed word — the bubble HUD shows it; *nothing changes on screen*. The
/// fix is applied only if the user taps Shift (accept). Payload is
/// [`CorrectionSuggestedEvent`]. (Principle #9: never a silent auto-apply.)
pub const EVT_CORRECTION_SUGGESTED: &str = "corrections://suggested";
/// **M3 correction bubble.** A correction was just APPLIED (the user accepted
/// with Shift) or reverted (Esc) — fired so the bubble can show the post-accept
/// "Fixed — Esc to undo" cue (and so every correction stays observable,
/// Principle #7). Payload is [`CorrectionAppliedEvent`] (`undo` flags a revert).
pub const EVT_CORRECTION_APPLIED: &str = "corrections://applied";
/// **M3 correction bubble.** A pending suggestion was dropped WITHOUT being
/// accepted — the user edited the word / typed on / moved the caret, the gate
/// went off, or it timed out. Tells the bubble to hide immediately, so a visible
/// bubble always means Shift will work (no "dead bubble"). No payload.
pub const EVT_CORRECTION_DISMISSED: &str = "corrections://dismissed";

/// **Data layer.** Result of a restore-from-backup. Payload `{ ok: bool,
/// message: String }`: on success `message` is the chosen filename's worth of
/// context; on failure it's a user-facing reason (bad file / newer version).
/// The Settings restore dialog listens to close on success or surface the error.
pub const EVT_DATA_RESTORED: &str = "data://restored";
/// **Data layer.** Fired after "Delete everything" completes — the disk is
/// first-launch-clean and the in-memory maps are empty. No payload.
pub const EVT_DATA_DELETED: &str = "data://deleted";

/// Engine-derived view of the sidecar's capture state. Transitions
/// are observation-only this phase — driven by the Heartbeat
/// InputEvent's `tap_enabled` flag. The watchdog (commit O) adds
/// time-based transitions (heartbeat staleness → Stopped) so the
/// enum carries the full state space here even though M's emitter
/// only ever produces `Live` / `Unhealthy` (and the implicit
/// `Unknown` initial).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)] // `Stopped` is constructed by the watchdog in commit O.
pub enum CaptureHealth {
    /// No heartbeat received yet — panel renders neutral pill.
    Unknown,
    /// Heartbeat fresh AND sidecar reports `tap_enabled: true`.
    Live,
    /// Heartbeat fresh BUT `tap_enabled: false` — sidecar is alive
    /// but its event tap is disabled. Auto-re-enable in the sidecar
    /// should recover this quickly; if it persists, the user can
    /// soft-restart via the panel (commit N).
    Unhealthy,
    /// No heartbeat for an extended period — watchdog escalated
    /// (commit O) or the sidecar's `CommandEvent::Terminated` arm
    /// fired. Capture is dead; only manual recovery via "Restart
    /// capture" can recover.
    Stopped,
}

/// Payload for [`EVT_CAPTURE_HEALTH`]. Single field rather than a
/// bare enum so future additions (reason text, retry count) don't
/// break the wire shape.
#[derive(Debug, Clone, Copy, Serialize)]
struct CaptureHealthEvent {
    state: CaptureHealth,
}

/// Payload for [`EVT_PERMISSION_STATUS`] — the two independent grants, each
/// reported by the sidecar's read-only probes. The UI shows a per-permission
/// row for each so a user mid-recovery (or mid-onboarding) sees exactly which
/// grant is still pending, not just an aggregate "capture is off".
#[derive(Debug, Clone, Copy, Serialize)]
struct PermissionStatusEvent {
    accessibility: bool,
    input_monitoring: bool,
}

/// The settled, menu-bar-facing capture state. Three states, deliberately — the
/// tray must never *assume* health (Principle #7). `NotStarted` is distinct from
/// `Stopped`: nothing has broken, capture simply hasn't been observed Live yet
/// (engine still booting, or — on a first run — deferred until onboarding grants
/// permission). Only an observed Live heartbeat promotes to `Active`; only a
/// settled outage demotes to `Stopped` (which alone carries a recovery action).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureUiState {
    NotStarted,
    Active,
    Stopped,
}

/// Payload for [`EVT_CAPTURE_UI`] — the debounced, menu-bar-facing view of
/// capture. When `state` is `Stopped`, `permission_revoked` chooses the recovery
/// action; it is meaningless (and always false) for the other two states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct CaptureUiEvent {
    state: CaptureUiState,
    permission_revoked: bool,
}

/// Control commands the engine task accepts from Tauri commands. Sent
/// through an unbounded mpsc channel whose sender lives in Tauri's
/// managed state. The engine task selects between sidecar events and
/// control commands so a button press is processed without waiting
/// for the next keystroke.
///
/// Kept deliberately small: the engine owns proposer / ledger /
/// anchors exclusively, so the only thing a control command does is
/// mutate that local state and emit the corresponding panel events.
#[derive(Debug)]
pub enum EngineControl {
    /// True engine-side LEXICON wipe — clears the proposer's state
    /// AND the lex's learned set. Emits [`EVT_LEXICON_RESET`] and a
    /// fresh empty [`EVT_LEARNED_SNAPSHOT`].
    ResetLexicon,
    /// Suppress / resume LEXICON credit. While paused, every
    /// `note_record` call is a no-op (see
    /// [`correction_engine::LexiconProposer::set_credit_paused`]) —
    /// records ingested while paused leave no proposer trace and are
    /// NOT retroactively credited on resume. Used by the panel's
    /// manual "Pause learning" toggle. Emits [`EVT_LEARNING_PAUSED`]
    /// echoing the new state.
    SetLearningPaused(bool),
    /// **Hard pause** — drop every Key/Backspace event at the engine
    /// task boundary, before [`BehaviouralModel::ingest`]. The engine
    /// effectively sleeps: no L2 ingest, no tokenization, no
    /// decisions, no events. Used by the panel's "Pause input" toggle
    /// when the user is talking ABOUT the system and wants the engine
    /// quiet (rather than just observing without learning).
    ///
    /// Transition to paused-on triggers a line-state flush (same as a
    /// newline reset): line_buf, line_dwells, caret, anchors,
    /// tokenizer — so the engine wakes on a clean boundary when input
    /// resumes. Pending ledger records without anchors linger but
    /// can't resolve (same as any unmonitored typing gap).
    ///
    /// Hard implies soft for learning purposes — paused input means
    /// no records flow, so the proposer never gets called regardless
    /// of `credit_paused`. The two flags stay independent at the
    /// engine; the panel surfaces them as separate indicators.
    /// Emits [`EVT_INPUT_PAUSED`] echoing the new state.
    SetInputPaused(bool),
    /// **Soft capture restart.** Writes
    /// `OutboundCommand::RestartTap` to the sidecar's stdin so it
    /// tears down its current CGEventTap and creates a fresh one.
    /// Used by the panel's "Restart capture" button when the sidecar
    /// is alive (heartbeats still arriving) but capture is unhealthy
    /// — auto-re-enable hasn't recovered, or the user manually
    /// triggered. Commit O escalates to a hard restart (respawn the
    /// sidecar process) when no heartbeat arrives within the
    /// soft-restart timeout.
    RestartCapture,
    /// **C5c motor stability request.** Ask the engine to emit the current
    /// [`StabilityReport`] on [`EVT_MOTOR_STABILITY`] immediately — Practice
    /// mode pulls fresh weakest-keys at session start; the (future)
    /// kill-switch reads the coverage/slip-rate inputs. Read-only: emits
    /// data, changes nothing.
    RequestMotorStability,
    /// **Practice trend request.** Reconstruct the per-key slip-rate trend for
    /// `keys` from the daily snapshot archives and emit it on
    /// [`EVT_PRACTICE_TREND`]. Practice asks for this at the snapshot, for the
    /// keys the round leaned into. Read-only: reads on-disk history, changes
    /// nothing.
    RequestPracticeTrend { keys: Vec<char> },
    /// **C5e word-freq gate.** Mark whether a warm-up / Practice round is on
    /// screen. While active, the typed text is **app-generated** (prompted
    /// sentences), so the [`WordFreq`] vocabulary tally must skip it or the
    /// personal word-frequency picture is skewed by our own prompts. The
    /// **motor map still observes** it — practising weak keys is exactly its
    /// purpose; only the vocabulary count is suppressed. Posted `true` when the
    /// Practice panel is shown, `false` when it hides (see `lib.rs`).
    SetPromptedCaptureActive(bool),
    /// **M3 correction Step 1 — master gate.** Flip `correction_enabled` on the
    /// allow-list. The instant global on/off the tray toggle posts. The engine
    /// persists and echoes the new state on [`EVT_CORRECTION_STATE`].
    SetCorrectionEnabled(bool),
    /// **M3 correction Step 1.** Ask the engine to emit the current allow-list
    /// on [`EVT_CORRECTION_STATE`] — the panel/tray pull fresh state on open.
    RequestAllowList,
    /// **Data layer — restore.** Replace the live learned data with a backup
    /// file (path is a user-chosen `.typingbackup`). The engine — the sole
    /// writer of `~/.typeassist` — re-validates the bundle, atomically replaces
    /// the learned + history stores on disk, swaps its in-memory maps, and emits
    /// [`EVT_DATA_RESTORED`] with the outcome. **Replace, never merge.** Leaves
    /// the correction gate (`allow_list`) and diagnostics untouched.
    RestoreData { path: String },
    /// **Data layer — delete everything.** Erase all learned data + history +
    /// diagnostics (a true first-launch-clean state), reset the in-memory maps,
    /// and reset the correction gate to **off**. Emits [`EVT_DATA_DELETED`] plus
    /// the refreshed [`EVT_CORRECTION_STATE`] / [`EVT_LEARNED_SNAPSHOT`].
    DeleteAllData,
    /// **UK English (v0.3.0) — locale-driven suggestion spelling.** Set the
    /// spelling variant a suggestion should use for a variant word (US-locale →
    /// `color`, UK-locale → `colour`). Detected in L5 from the webview locale
    /// (`navigator.language`) and pushed here at startup; the engine applies it
    /// only to the *suggested* target of a genuine motor slip
    /// ([`correction_engine::spelling_variant`]) — membership stays dialect-blind
    /// (both spellings always valid). Never persisted; no OS read in L2–L4.
    SetSpellingVariant(SpellingVariant),
}

/// Tauri-managed handle for sending [`EngineControl`] messages to
/// the engine task. Cloned by Tauri commands; the underlying channel
/// is unbounded so a button press never blocks the UI thread.
pub type EngineControlSender = tokio::sync::mpsc::UnboundedSender<EngineControl>;

/// Top-N candidates the engine surfaces per unknown word. Keep small so the
/// debug panel and any future spatial-scorer aren't paying for a long tail.
const CANDIDATES_TOP_N: usize = 3;

// macOS's `CGEvent.keyboardGetUnicodeString` translates navigation /
// function keys differently depending on the layout and the Fn modifier
// state. Both forms turn up in practice:
//
//   * C0 cursor-control range (US English layout, Fn off):
//     U+001C Left, U+001D Right, U+001E Up, U+001F Down.
//   * AppKit NSEvent function-key constants (private-use, with Fn on or
//     other layouts): U+F700..=U+F8FF — Left U+F702, Home U+F729, etc.
//
// Neither form is text. They must never reach line_buf or the tokenizer
// core (otherwise they render as box glyphs inside tokens). Both forms
// are filtered out by `is_non_text_key`; the known-nav codepoints below
// are mapped to caret moves.
const KEY_C0_LEFT: char = '\u{001C}';
const KEY_C0_RIGHT: char = '\u{001D}';
// Up/Down are filtered out of text (via `is_non_text_key`) and NOT mapped to
// caret moves in the single-line model. Instead `is_vertical_nav` routes them
// to a Fix-B line reset: they move the caret to another line the model can't
// follow, so leaving the caret put (the old behaviour) silently desynced.
const KEY_C0_UP: char = '\u{001E}';
const KEY_C0_DOWN: char = '\u{001F}';
const KEY_NS_LEFT: char = '\u{F702}';
const KEY_NS_RIGHT: char = '\u{F703}';
const KEY_NS_UP: char = '\u{F700}';
const KEY_NS_DOWN: char = '\u{F701}';
const KEY_NS_HOME: char = '\u{F729}';
const KEY_NS_END: char = '\u{F72B}';
// PageUp / PageDown (AppKit NSPageUp/DownFunctionKey). Like Up/Down they jump
// the caret off the tracked line → Fix-B reset, not a caret nudge.
const KEY_NS_PAGEUP: char = '\u{F72C}';
const KEY_NS_PAGEDOWN: char = '\u{F72D}';

/// True if `c` is a non-text key signal — a control character or an
/// AppKit private-use function-key code. These codepoints exist only to
/// tell the app "the user pressed this key"; they must not enter any text
/// buffer.
///
/// `\n` `\r` `\t` are deliberate exceptions: the engine handles newline /
/// carriage-return as line resets and treats tab as a whitespace boundary
/// the tokenizer is allowed to see. Every other control char and every
/// codepoint in U+F700..=U+F8FF is filtered.
fn is_non_text_key(c: char) -> bool {
    if c == '\n' || c == '\r' || c == '\t' {
        return false;
    }
    c.is_control() || (0xF700..=0xF8FF).contains(&(c as u32))
}

/// THE coherent nav-key handler: given a non-text codepoint, the current
/// caret + line length, and whether the Command modifier is held, return
/// the caret's new position. `None` means the char isn't a mapped nav key
/// (caller leaves the caret alone — function key, etc.).
///
/// All four nav keys live here so they can't drift apart between fixes:
///   Left            → caret − 1 (saturating)
///   Right           → caret + 1 (clamped to line_len)
///   Cmd+Left        → 0          (macOS convention for Home)
///   Cmd+Right       → line_len   (macOS convention for End)
///   Home (NS Home)  → 0          (dedicated key, external keyboard)
///   End (NS End)    → line_len   (dedicated key, external keyboard)
///
/// **`modifiers.function` is deliberately not consulted.** On a MacBook
/// every arrow press has `fn=true` (the arrows live in the function-key
/// cluster), so a Fn-modifier branch routes every plain Left into the
/// Home branch — the regression we already hit. The Command modifier IS
/// safe: it distinguishes the user's deliberate "go to start of line"
/// gesture from plain navigation.
fn nav_action(c: char, caret: usize, line_len: usize, command: bool) -> Option<usize> {
    match c {
        KEY_C0_LEFT | KEY_NS_LEFT => {
            if command {
                Some(0)
            } else {
                Some(caret.saturating_sub(1))
            }
        }
        KEY_C0_RIGHT | KEY_NS_RIGHT => {
            if command {
                Some(line_len)
            } else {
                Some((caret + 1).min(line_len))
            }
        }
        KEY_NS_HOME => Some(0),
        KEY_NS_END => Some(line_len),
        _ => None,
    }
}

/// True for the vertical / paging navigation keys (Up, Down, PageUp, PageDown).
/// `nav_action` returns `None` for these — in the single-line model there's no
/// caret position to move to — so the old behaviour left the caret put, which
/// silently desynced the model from the real (now multi-line) cursor. Fix-B
/// routes them to a full line reset instead. Plain Left/Right/Home/End are NOT
/// here: they move the model caret correctly (and off the line end, which
/// already disables firing), so they stay as-is.
fn is_vertical_nav(c: char) -> bool {
    matches!(
        c,
        KEY_C0_UP | KEY_C0_DOWN | KEY_NS_UP | KEY_NS_DOWN | KEY_NS_PAGEUP | KEY_NS_PAGEDOWN
    )
}

/// **Fix-B — caret-move line reset.** The engine dead-reckons `line_buf` /
/// `caret` / anchors from keystrokes alone, so any caret move it can't observe
/// (mouse / trackpad click, Up / Down / PageUp / PageDown, focus or app switch)
/// leaves that model stale. With live correction on, a stale model can fire
/// backspaces at the wrong position (the `edndd` desync). Resetting to a clean
/// line only costs a skipped correction on the *next* word — the safe
/// direction — whereas NOT resetting is what deletes the wrong thing, so every
/// unobservable caret move funnels through here.
///
/// Mirrors the newline reset (clears the line buffers + caret + anchors, resets
/// the tokenizer line, emits `EVT_LINE_RESET` + the anchor snapshot) and also
/// disarms any pending Escape-undo, whose revert would otherwise inject at the
/// now-stale caret. `trigger` is a content-free log tag only (`mouse` /
/// `app` / `focus-element` / `updown`): `app` = a real application switch,
/// `focus-element` = an in-app focused-element re-publish (noisy in rich-text /
/// web surfaces). Fix-B resets the line model on all of them.
fn reset_line_for_caret_move<R: Runtime>(
    app: &AppHandle<R>,
    tokenizer: &mut Tokenizer,
    line_buf: &mut Vec<char>,
    line_dwells: &mut Vec<u32>,
    caret: &mut usize,
    anchors: &mut AnchorTracker,
    last_correction: &mut Option<LastCorrection>,
    trigger: &str,
) {
    tracing::info!("LINE_RESET trigger={trigger}");
    tokenizer.reset_line();
    line_buf.clear();
    line_dwells.clear();
    *caret = 0;
    anchors.clear();
    *last_correction = None;
    let _ = app.emit(EVT_LINE_RESET, ());
    let snap = anchors.snapshot();
    let _ = app.emit(EVT_ANCHOR_SNAPSHOT, anchor_emit_payload(&snap, line_buf));
}

/// THE single entry point that puts a character into `line_buf`. Anything
/// that would write to the buffer goes through here so the non-text guard
/// is impossible to bypass. Returns the new caret position, or `None` if
/// the char was rejected (control or function-key code) — in which case
/// the buffer is unchanged.
fn insert_text_char(line_buf: &mut Vec<char>, caret: usize, c: char) -> Option<usize> {
    if is_non_text_key(c) {
        // Defensive log — should be unreachable now that the Key arm
        // gates non-text codepoints earlier, but if it ever fires we see
        // exactly which codepoint slipped through.
        tracing::warn!(
            "rejected non-text codepoint U+{:04X} at line_buf entry",
            c as u32
        );
        return None;
    }
    let p = caret.min(line_buf.len());
    line_buf.insert(p, c);
    Some(p + 1)
}

#[derive(Serialize, Clone)]
struct KeystrokePayload {
    key: String,
    dwell_ms: u32,
    /// Wall time the L2 `BehaviouralModel::ingest` call took for this event.
    /// Surfaced per-row so we can watch the observe-only L2 wiring stay cheap.
    ingest_latency_ms: f64,
}

/// Payload for [`EVT_CORRECTION_APPLIED`] — what the bubble shows after an accept
/// or revert. Carries the before/after words and whether this event is a fix or
/// its undo (`teh → the` on apply, `the → teh` reverting on undo).
#[derive(Serialize, Clone)]
struct CorrectionAppliedEvent {
    typed: String,
    target: String,
    /// `false` for an applied fix, `true` for an Escape revert.
    undo: bool,
}

/// Payload for [`EVT_CORRECTION_SUGGESTED`] — the bubble's before→after, with the
/// changed target letters (`highlight`) marked soft-blue. Nothing is injected
/// until the user accepts with Shift.
#[derive(Serialize, Clone)]
struct CorrectionSuggestedEvent {
    typed: String,
    target: String,
    highlight: Vec<usize>,
}

/// A correction the engine is currently SUGGESTING (the bubble is up), retained
/// until the user accepts it with an isolated Shift tap. Stays ACCEPTABLE for a
/// generous window even after the user types ON, via an ANCHORED replace keyed
/// off `word_end` (the word's end position in the line). Dropped the moment the
/// user edits the source word / navigates / the caret moves, exceeds the
/// distance cap, or the window expires — bias hard toward dismiss (a missed
/// accept is fine; a wrong-place edit is not). `word_len`/`boundary` reproduce
/// the immediate (no-typing-since) delete+retype; `word_end` anchors the
/// positional replace once the caret has moved on.
#[derive(Debug, Clone)]
struct PendingSuggestion {
    /// The word to correct, with any surrounding quotes/punctuation STRIPPED
    /// (QA-17): a matched straight-quote pair peeled off the core (5a) — so this
    /// is what the bubble shows and what matched a lane (`waht`, not `'waht'`).
    typed: String,
    target: String,
    boundary: char,
    /// Full core length at seal (= `tok.end - tok.start`) — includes any matched
    /// straight-quotes that were part of the core, so the delete covers them.
    word_len: usize,
    /// The word's end position in the line buffer at seal (= `tok.end`). Fixed
    /// while pending (it dismisses if anything edits at/before it), so the accept
    /// can compute how far the caret has moved on.
    word_end: usize,
    /// QA-17 re-attach: punctuation to retype AROUND the corrected word so quotes
    /// survive the fix. `lead` = the straight-quote run stripped from the core's
    /// front (retyped before `target`); `trail` = the core's stripped trailing
    /// straight-quotes PLUS the tokenizer's closing punct (`tok.trailing`, e.g. a
    /// double-quote), retyped after `target`, before `boundary`.
    lead: String,
    trail: String,
    /// Char count of `tok.trailing` alone — the closing punct that sits BEYOND
    /// `word_end` (between the word and the boundary). Drives the caret-at-rest
    /// `immediate` check (`after_len == outer_trail_len + 1`) and the extra delete.
    outer_trail_len: usize,
    armed_at_ms: u64,
}

/// How long a suggestion stays up if the user neither accepts (Shift) nor types —
/// the generous "window." Sized for the target user, who types slowly: long
/// enough to notice + accept, short enough that an ignored one doesn't overstay.
/// (The bubble's own SUGGEST_MS backstop sits above this.) **Tunable.**
const SUGGESTION_TIMEOUT_MS: u64 = 5_000;

/// Distance cap for the anchored accept: how many chars the caret may have moved
/// PAST the word and still let Shift land it (≈ 1–2 words). Beyond this the
/// dead-reckoning drift risk is too high → dismiss. Deliberately tight (the
/// target user types slowly — they won't be far ahead). **Tunable.**
const MAX_ANCHOR_CHARS: usize = 20;

/// Master switch for the ANCHORED accept (Shift *after* the caret has typed past
/// the word). **OFF.** A wrong-place anchored replace doubled a word in the live
/// field — `settgings` + ~3 words + Shift produced `Settingsettings` — because
/// the engine dead-reckons the caret from keystrokes and that model had drifted
/// from the real field (autocap / untracked edits the tap never observes). For
/// this audience, garbling text is the one failure we cannot ship, so until the
/// anchored path is *provably* safe Shift accepts ONLY on the immediate
/// no-arrows path (`after_len == 1`). Re-enabling requires L1 to confirm the word
/// at the position from the AX field (ground truth) — the dead-reckoned model
/// alone cannot make this safe. A missed accept is fine; a wrong-place edit is
/// not. See [`word_at_anchor`].
const ANCHORED_ACCEPT_ENABLED: bool = false;

/// Fail-safe check for the anchored accept: before an anchored replace
/// deletes/types, verify the engine's own line model still holds the ORIGINAL
/// word (`typed`) at the anchored position (`line_buf[word_end - word_len ..
/// word_end]`). A mismatch means the model has drifted — inject nothing.
///
/// This catches model-INTERNAL desync (retokenisation / miscounted positions).
/// It cannot catch divergence the model never observed (autocap rewriting the
/// field), because `line_buf` is itself the dead-reckoned model — which is why
/// anchored stays gated by [`ANCHORED_ACCEPT_ENABLED`] until L1 supplies the
/// field's ground truth. Necessary, not sufficient.
fn word_at_anchor(line_buf: &[char], word_end: usize, word_len: usize, typed: &str) -> bool {
    let Some(start) = word_end.checked_sub(word_len) else {
        return false;
    };
    match line_buf.get(start..word_end) {
        Some(slice) => slice.iter().copied().eq(typed.chars()),
        None => false,
    }
}

/// Whether an armed [`PendingSuggestion`] survives a forward keystroke (the
/// end-of-arm retention gate). Kept only when the keystroke was a clean forward
/// append (`keep_pending`), the caret is still PAST the word, and it hasn't moved
/// beyond the accept-reachable distance.
///
/// That distance is the caret's AT-REST position: the boundary PLUS any closing
/// punctuation beyond the word (`outer_trail_len + 1`), NOT a bare `1` (QA-17
/// 5b). A hardcoded `1` here dropped a word wrapped in a trailing `"` the instant
/// it sealed — the fire armed the pending, then this gate saw `dist == 2 > 1` and
/// nuked it (emitting `DISMISSED`) on the SAME keystroke, so the bubble never
/// rendered. This is the twin of the accept-side `immediate` check
/// (`after_len == outer_trail_len + 1`); both must agree on the rest position.
/// With the anchored accept enabled the reach extends to the full distance cap.
fn pending_survives(
    keep_pending: bool,
    caret: usize,
    word_end: usize,
    outer_trail_len: usize,
    anchored_enabled: bool,
) -> bool {
    if !keep_pending || caret <= word_end {
        return false;
    }
    let dist = caret - word_end;
    let cap = if anchored_enabled {
        MAX_ANCHOR_CHARS
    } else {
        outer_trail_len + 1
    };
    dist <= cap
}

/// The just-fired correction, retained so a single Escape can revert it within
/// [`UNDO_WINDOW_MS`]. `typed`/`target` are normalized (allow-list form);
/// `boundary` is the terminator char that sealed the word (re-typed verbatim on
/// both inject and revert). `fired_at_ms` arms the window.
#[derive(Debug, Clone)]
struct LastCorrection {
    typed: String,
    target: String,
    boundary: char,
    /// QA-17 re-attach — mirror of [`PendingSuggestion::lead`] / `trail`, so the
    /// revert restores the ORIGINAL word with its surrounding quotes intact
    /// (`'what' ` → `'waht' `, `believe" ` → `beleive" `).
    lead: String,
    trail: String,
    /// Whether the accept took the IMMEDIATE (caret-at-rest) path — the only
    /// enabled one. The revert is caret-relative when true. (Replaces the old
    /// `after_len == 1` test, which broke once a trailing quote made the rest
    /// position `outer_trail_len + 1 > 1`.)
    immediate: bool,
    /// `after_len` from the accept, retained for the (still-gated-off) anchored
    /// revert path. `last_correction` disarms on ANY non-Esc keystroke, so an Esc
    /// undo only ever fires with the caret exactly where the accept left it.
    after_len: usize,
    fired_at_ms: u64,
}

/// QA-17 (5a) — peel a MATCHED leading+trailing run of straight ASCII single
/// quotes off a token core. Returns `(inner, lead, trail)` where `lead`/`trail`
/// are the stripped quote runs (empty when nothing is stripped). Only strips
/// when BOTH sides carry ≥1 quote (a wrapping pair) and at least one non-quote
/// char remains — so a one-sided apostrophe is untouched:
///   * `'waht'` → (`waht`, `'`, `'`)   — a straight-quoted typo, now matchable
///   * `dogs'`  → (`dogs'`, ``, ``)     — trailing possessive, kept
///   * `'em`    → (`'em`, ``, ``)       — leading contraction, kept
///   * `don't` / `it's` → unchanged     — internal apostrophe, kept
/// Straight `'` is the only ambiguous case (it doubles as the apostrophe); curly
/// ‘…’ and "…" are already stripped as punctuation by the tokenizer. The rare
/// fully-quoted contraction `'twas'` → `twas` is the accepted tradeoff.
fn strip_matched_quotes(core: &str) -> (String, String, String) {
    let chars: Vec<char> = core.chars().collect();
    let lead_n = chars.iter().take_while(|&&c| c == '\'').count();
    let trail_n = chars.iter().rev().take_while(|&&c| c == '\'').count();
    if lead_n == 0 || trail_n == 0 || lead_n + trail_n >= chars.len() {
        return (core.to_string(), String::new(), String::new());
    }
    let inner: String = chars[lead_n..chars.len() - trail_n].iter().collect();
    let lead: String = chars[..lead_n].iter().collect();
    let trail: String = chars[chars.len() - trail_n..].iter().collect();
    (inner, lead, trail)
}

/// Immediate-accept injection geometry (QA-17). Delete the whole core + the
/// closing punct beyond it + the boundary, then retype the surrounding punct
/// re-wrapped around `target`. Reduces to the old `word_len + 1` / `target +
/// boundary` when there is no surrounding punctuation. Pure + tested so the
/// geometry is verifiable without driving the engine loop.
fn immediate_accept_injection(ps: &PendingSuggestion) -> (u32, String) {
    let delete_count = (ps.word_len + ps.outer_trail_len + 1) as u32;
    let replacement = format!("{}{}{}{}", ps.lead, ps.target, ps.trail, ps.boundary);
    (delete_count, replacement)
}

/// Immediate-revert geometry (QA-17) — the exact inverse of
/// [`immediate_accept_injection`]: delete the injected `lead+target+trail+
/// boundary` and retype the original `lead+typed+trail+boundary`, restoring the
/// surrounding quotes. Pure + tested.
fn immediate_revert_injection(lc: &LastCorrection) -> (u32, String) {
    let delete_count = (lc.lead.chars().count()
        + lc.target.chars().count()
        + lc.trail.chars().count()
        + 1) as u32;
    let replacement = format!("{}{}{}{}", lc.lead, lc.typed, lc.trail, lc.boundary);
    (delete_count, replacement)
}

/// How long after a correction an Escape still reverts it. Sized for slow /
/// stroke-survivor reaction time — generous, but the window also closes the
/// moment the user types any other character (an implicit accept), so a long
/// timeout doesn't keep Escape hijacked. **Tunable.**
const UNDO_WINDOW_MS: u64 = 6_000;

/// Escape's codepoint (U+001B). The engine sees it as a `Key` event (the L1 tap
/// streams every keystroke); within an armed undo window it reverts the last
/// correction instead of being a caret-only no-op.
const KEY_ESCAPE: char = '\u{001B}';

/// Per-Word-token decision (Component 3c-2). Observe-only — `would-correct`
/// is a *proposal*, not an injection. `outcome` is the full
/// [`DecisionOutcome`] tagged enum (`would_correct` / `leave_alone` with
/// reason). `decide_time_ms` is the scoring + decision cost; latency from
/// the triggering keystroke is no longer single-valued under the new
/// per-token-seal model (replays can re-fire).
#[derive(Serialize, Clone)]
struct DecisionPayload {
    outcome: DecisionOutcome,
    active_tier: ConfidenceTier,
    decide_time_ms: f64,
    decision_version: u32,
}

/// Re-apply the user's capitalisation pattern to a (lowercase) correction
/// target, so a fix never changes their casing: `Teh → The`, `WAHT → WHAT`,
/// `teh → the`. All-caps (2+ letters) → upper; leading capital → capitalise the
/// first letter; otherwise verbatim. Case is read from the RAW typed word.
fn match_source_case(typed_raw: &str, target_lower: &str) -> String {
    let letters: Vec<char> = typed_raw.chars().filter(|c| c.is_alphabetic()).collect();
    let all_caps = letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase());
    if all_caps {
        return target_lower.to_uppercase();
    }
    let leading_cap = typed_raw.chars().next().is_some_and(|c| c.is_uppercase());
    if leading_cap {
        let mut out = String::new();
        let mut chars = target_lower.chars();
        if let Some(f) = chars.next() {
            out.extend(f.to_uppercase());
        }
        out.push_str(chars.as_str());
        return out;
    }
    target_lower.to_string()
}

/// The "contraction filter" (locked principle): TypeAssist owns idiosyncratic
/// MOTOR garbles, not missing apostrophes / contractions / possessives — that's
/// Auto-Correct's job. Returns true for an apostrophe-only fix so the engine
/// suppresses it: the target has an apostrophe AND, with apostrophes removed and
/// case-folded, it equals the typed form (a pure apostrophe insert, `dont → don't`,
/// `todays → today's`) or the typed form + `s` (possessive completion, `key' →
/// key's`). A genuine motor garble in an apostrophe word (`doens't → doesn't`,
/// the letters differ by a transposition) is NOT suppressed.
fn is_apostrophe_fix(typed: &str, target: &str) -> bool {
    if !target.contains('\'') {
        return false;
    }
    let strip = |s: &str| -> String {
        s.chars()
            .filter(|&c| c != '\'')
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    let st = strip(typed);
    let sg = strip(target);
    st == sg || sg == format!("{st}s")
}

/// Emission wrapper for an anchor snapshot. The pure
/// [`correction_engine::AnchorsSnapshot`] is kept minimal for tests; this
/// adds the current line buffer so the panel can render text-now-at-span
/// without doing its own keystroke book-keeping.
#[derive(Serialize, Clone)]
struct AnchorEmitPayload<'a> {
    anchors: &'a [correction_engine::SpanAnchor],
    void_count: u32,
    current_line: String,
}

fn anchor_emit_payload<'a>(
    snap: &'a correction_engine::AnchorsSnapshot,
    line: &[char],
) -> AnchorEmitPayload<'a> {
    AnchorEmitPayload {
        anchors: &snap.anchors,
        void_count: snap.void_count,
        current_line: line.iter().collect(),
    }
}

/// Current wall-clock in ms since the Unix epoch. Saturating to 0 keeps
/// the resolver's debounce arithmetic well-defined if the clock query
/// ever fails (it shouldn't, but the engine is long-lived).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// **Capture-integrity funnel** (CLAUDE.md Principle #7 — capture integrity
/// is observable, not assumed). Cumulative per-session counters at each
/// pipeline boundary so the conversion ratio between adjacent stages is
/// auditable: `received → accepted → sealed → verdict → observe → save`.
/// A break in any ratio localises a silent drop. Task-local — reset each
/// engine session; not persisted.
#[derive(Debug, Default)]
struct Funnel {
    /// L1: raw Key/Backspace events received from the sidecar.
    keystrokes_received: u64,
    /// **L1 capture loss made visible (Principle #7).** Non-backspace
    /// auto-repeat keyDowns the adapter dropped (it emits a text key once, on
    /// keyUp, so held-key repeats are lost). This is NOT part of `received` —
    /// these never reached the pipeline. A healthy run keeps this ~0; a climbing
    /// count means held *letter* keys are dropping in real use (held backspace
    /// is already forwarded, so it never lands here). It exists so the drop is
    /// counted, not silent.
    autorepeat_dropped: u64,
    /// After filtering (modifier/Cmd-Ctrl drops, non-text nav, pause).
    keystrokes_accepted: u64,
    /// Fresh Word/Acronym seals (a new anchor registered). Replay re-seals
    /// are NOT counted (they re-tokenise existing content), so this is the
    /// distinct-word count.
    tokens_sealed: u64,
    /// Admitted to the C5c **motor ledger** — every fresh, motor-evidenced
    /// sealed word (known included). The stage between sealing and the
    /// motor verdict; the gate is just motor evidence (no lexicon Known-skip),
    /// so `admitted ≈ sealed` minus paste / zero-dwell. (Principle #7: the
    /// new boundary is observable.)
    records_admitted: u64,
    /// 5a verdicts emitted (from the MOTOR ledger), by outcome (revisable: a
    /// record can re-resolve, so totals can exceed `tokens_sealed`). The motor
    /// ledger is candidate-agnostic, so `corr_sug` is always 0 here.
    v_kept: u64,
    v_corr_sug: u64,
    v_corr_oth: u64,
    v_abandoned: u64,
    /// 5c char-level observations folded into the motor map.
    motor_kept: u64,
    motor_slip: u64,
    /// Successful `motor_map.json` flushes.
    motor_saves: u64,
    /// 5d word-pattern store: of the `CorrectedToOther` verdicts the store
    /// sees, how many were recorded as a `typed→target` pattern vs skipped
    /// (semantic rewrite / no-op / no recoverable post-edit text). The two
    /// reconcile against `v_corr_oth` (Principle #7: this stage drops data —
    /// the rewrite filter — so the drop is counted, not silent).
    word_patterns_observed: u64,
    word_pattern_skipped: u64,
    /// Successful `word_patterns.json` flushes.
    word_pattern_saves: u64,
    /// M3 correction Step 1 (Principle #7: a live correction is an action on
    /// the user's text — it must never be silent). `applied` counts injections
    /// fired from the manual allow-list; `undone` counts Escape teach-stops
    /// that reverted one. A healthy run reconciles `undone ≤ applied`.
    corrections_applied: u64,
    corrections_undone: u64,
    /// **M3 autocorrect coexistence — "Watch Dog Not Attack Dog" (Principle
    /// #7).** Times Jordan stood down on a host-redundant (common) suggestion
    /// because he behaviourally sensed an active competing corrector
    /// ([`CompetitorSense`]). A deliberate, visible drop — a suggestion the
    /// engine WOULD have surfaced but withheld so it doesn't double-correct the
    /// host. Idiosyncratic fixes are never counted here (they always fire).
    competitor_deferred: u64,
    /// **TF-08 host-scoped suppression (Principle #7).** Times Jordan went
    /// watch-only on a would-fire suggestion because L1 reported the focused field
    /// as an injection dead zone (Safari web/contenteditable on Intel — phantom AX
    /// caret; a synthetic fix can't land). A deliberate, visible drop: the engine
    /// WOULD have surfaced the bubble but withheld it (no cue) rather than garble
    /// the field. Kept observing/learning regardless. Distinct from
    /// `competitor_deferred` (host-redundant deferral); the dead zone withholds
    /// ALL fixes, not just redundant ones.
    dead_zone_suppressed: u64,
    /// **L1 space-drop detection (Principle #7, QA-15 Step 1 — observe-only).**
    /// `space_observed` is the latest cumulative space-keyDown total the L1 tap
    /// reported ([`InputEvent::SpaceObserved`]); `space_accepted` is the spaces
    /// the engine actually received as Key events. The tap sees a space keyDown
    /// even when its text key (emitted on keyUp) is lost, so a persistent
    /// shortfall of accepted spaces is a dropped space — the Google-Docs weld.
    /// `space_drops_suspected` is the cumulative settled shortfall (it only ever
    /// rises). Counts only; never content. **Drives no behaviour** — it flags.
    space_observed: u64,
    space_accepted: u64,
    space_drops_suspected: u64,
    /// Session start (ms since epoch), stamped at task spawn.
    session_started_ms: u64,
}

impl Funnel {
    fn new(now: u64) -> Self {
        Self {
            session_started_ms: now,
            ..Default::default()
        }
    }

    /// The engine received one space Key from the sidecar (QA-15 reconciliation).
    fn note_space_accepted(&mut self) {
        self.space_accepted += 1;
    }

    /// The L1 tap reports it has observed `total` space keyDowns. Updates the
    /// running totals and returns the new cumulative suspected-drop count **iff a
    /// not-yet-flagged drop just became evident** (so the caller logs
    /// `SPACE_DROP_SUSPECTED` once per real drop), else `None`.
    ///
    /// One space may be legitimately in flight — its keyDown seen here but its
    /// text key (keyUp) not yet processed — so the *settled* shortfall tolerates a
    /// single in-flight press. Because a lost space is never recovered, the
    /// settled shortfall is monotonic non-decreasing, so it doubles as the
    /// cumulative drop count. `saturating_sub` keeps it sound if a `SpaceObserved`
    /// marker itself is lost (accepted momentarily ahead of observed).
    fn note_space_observed(&mut self, total: u64) -> Option<u64> {
        self.space_observed = self.space_observed.max(total);
        let settled = self
            .space_observed
            .saturating_sub(1) // tolerate one in-flight space
            .saturating_sub(self.space_accepted);
        if settled > self.space_drops_suspected {
            self.space_drops_suspected = settled;
            Some(settled)
        } else {
            None
        }
    }

    /// Emit the structured funnel line to the log. Same output for both
    /// callers (the Cmd+Shift+F chord and the 60s auto-dump).
    fn dump(&self) {
        tracing::info!(
            "FUNNEL_DUMP {{ c_keystrokes_received: {}, c_autorepeat_dropped: {}, \
             c_keystrokes_accepted: {}, \
             c_tokens_sealed: {}, c_records_admitted: {}, c_verdicts_resolved: {{kept: {}, \
             corr_sug: {}, corr_oth: {}, abandoned: {}}}, c_motor_observations: {{kept: {}, \
             slip: {}}}, c_motor_saves: {}, c_word_patterns: {{observed: {}, skipped: {}}}, \
             c_word_pattern_saves: {}, c_corrections: {{applied: {}, undone: {}}}, \
             c_competitor_deferred: {}, c_dead_zone_suppressed: {}, \
             c_space: {{observed: {}, accepted: {}, \
             drops_suspected: {}}}, session_started_at: {} }}",
            self.keystrokes_received,
            self.autorepeat_dropped,
            self.keystrokes_accepted,
            self.tokens_sealed,
            self.records_admitted,
            self.v_kept,
            self.v_corr_sug,
            self.v_corr_oth,
            self.v_abandoned,
            self.motor_kept,
            self.motor_slip,
            self.motor_saves,
            self.word_patterns_observed,
            self.word_pattern_skipped,
            self.word_pattern_saves,
            self.corrections_applied,
            self.corrections_undone,
            self.competitor_deferred,
            self.dead_zone_suppressed,
            self.space_observed,
            self.space_accepted,
            self.space_drops_suspected,
            self.session_started_ms,
        );
    }

    /// Zero every counter and restamp the run start — closes a measurement
    /// run and opens a fresh one (Principle #7: counters reconciled **per
    /// run**, never conflated across runs). Logs a `FUNNEL_RESET` marker so
    /// run boundaries are visible when reconstructing from the log. Called
    /// by the explicit dump chord (after the dump) and the reset chord.
    fn reset(&mut self, now: u64) {
        *self = Funnel::new(now);
        tracing::info!("FUNNEL_RESET — counters zeroed, new run from {}", now);
    }
}

/// **EASILY-FLIPPED CONSTANT — placeholder (decision iii).** Host-redundant
/// footprints required before Jordan stands down on the common lane. N=2 so a
/// single stray correction (a one-off self-fix) never trips deferral; small
/// enough to engage quickly when a host corrector is genuinely active.
const COMPETITOR_EVIDENCE_THRESHOLD: u32 = 2;

/// **EASILY-FLIPPED CONSTANT — placeholder (decision iii).** Idle gap with no
/// fresh footprint after which the competitor is treated as gone: Jordan
/// re-arms and fires common fixes again. ~45s — long enough to span the pauses
/// of a slow-typing user mid-paragraph, short enough that toggling the host
/// corrector off is felt within a sentence or two.
const COMPETITOR_REARM_IDLE_MS: u64 = 45_000;

/// **Watch Dog Not Attack Dog (M3 autocorrect coexistence).** Jordan's purely
/// *behavioural* sense of whether another corrector (a host app's autocorrect)
/// is actively fixing words right now. He never reads the app's or system's
/// autocorrect switch (content-blind, Principle #8); he senses a competitor by
/// **footprint** — a `CorrectedToOther` in the host-redundant lane that he did
/// **not** cause (an external agent made a common fix). His own accepts can't be
/// footprints: their injected echo is dropped before the pipeline
/// (`pending_echo`), so they never resolve `CorrectedToOther`.
///
/// After [`COMPETITOR_EVIDENCE_THRESHOLD`] footprints he *defers* on
/// host-redundant (common) suggestions **only**; idiosyncratic fixes always
/// fire (his core value — never suppressed). He re-arms when the competitor goes
/// quiet ([`Self::decay`], ~45s idle) or on app/focus change ([`Self::reset`]).
///
/// **Global, not per-app** (decision ii): a single counter reset on focus
/// change, so **no app identity is ever held or persisted** — the strongest
/// Principle #8 posture. Cost: returning to an autocorrect app re-fires up to
/// N common bubbles before re-deferring (fail-toward-firing, accepted).
///
/// **Output-only:** this gates whether Jordan *suggests*, never what he
/// *learns* — the motor ledger / word-patterns learn from all ambient
/// resolution regardless (`tick_resolver`, the motor pass).
#[derive(Debug, Default)]
struct CompetitorSense {
    /// Footprints sensed in the current armed window. Saturates; zeroed by
    /// [`Self::decay`] (idle re-arm) or [`Self::reset`] (focus change).
    evidence: u32,
    /// Wall-clock (ms) of the most recent footprint; drives idle re-arm.
    last_footprint_ms: u64,
}

impl CompetitorSense {
    /// A competing corrector just made a host-redundant fix Jordan didn't
    /// cause. Bumps evidence and stamps the time.
    fn note_footprint(&mut self, now: u64) {
        self.evidence = self.evidence.saturating_add(1);
        self.last_footprint_ms = now;
    }

    /// True when a competitor is currently sensed: enough footprints AND the
    /// last one within the re-arm window. Pure read (always fresh) — the time
    /// guard means deferral lapses the instant the window passes, even before
    /// [`Self::decay`] zeroes the counter.
    fn is_deferring(&self, now: u64) -> bool {
        self.evidence >= COMPETITOR_EVIDENCE_THRESHOLD
            && now.saturating_sub(self.last_footprint_ms) < COMPETITOR_REARM_IDLE_MS
    }

    /// Re-arm if the competitor has gone quiet: once a full idle window passes
    /// with no footprint, zero the evidence so a fresh competitor must clear
    /// the threshold again (preserves the N-footprint hysteresis across quiet
    /// gaps). Called from the 1s watchdog. Idempotent.
    fn decay(&mut self, now: u64) {
        if self.evidence > 0
            && now.saturating_sub(self.last_footprint_ms) >= COMPETITOR_REARM_IDLE_MS
        {
            self.evidence = 0;
        }
    }

    /// Hard reset on app/focus change — a new app may have a different
    /// corrector regime, so re-arm immediately (fail toward firing).
    fn reset(&mut self) {
        self.evidence = 0;
        self.last_footprint_ms = 0;
    }
}

// ---- Component 5c motor-map persistence paths + cadence --------------------
//
// The L4 crate is deliberately path-agnostic (it stays portable); the host
// resolves the concrete `~/.typeassist/...` locations here. macOS-only for
// now, so HOME is sufficient — a Windows adapter would resolve differently.

/// `~/.typeassist`, or `None` if HOME is unset (the map then runs in-memory
/// only — no durable file this session).
///
/// `TYPEASSIST_DATA_DIR` overrides the location outright. This is the
/// **dev-safety valve**: a dev build can be pointed at a scratch folder so it
/// never writes the same files as an installed release build (two writers on
/// the same recovery data is a Principle #6/#8 hazard). Unset in normal use, so
/// the default `~/.typeassist` is unchanged.
fn typeassist_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TYPEASSIST_DATA_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".typeassist"))
}

/// `~/.typeassist/motor_map.json` — the live, periodically-saved map.
fn motor_map_path() -> Option<PathBuf> {
    typeassist_dir().map(|d| d.join("motor_map.json"))
}

/// macOS Accessibility trust — a pure, no-prompt read of `AXIsProcessTrusted()`.
/// The main app process is its OWN responsible process, so this reflects the
/// same app-level grant the spawned sidecar inherits. Used only for the
/// first-run decision; the sidecar remains the authoritative capture-time check.
#[cfg(target_os = "macos")]
pub fn accessibility_granted() -> bool {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    // SAFETY: AXIsProcessTrusted takes no args, has no side effects, and never
    // prompts (unlike AXIsProcessTrustedWithOptions with the prompt option).
    unsafe { AXIsProcessTrusted() }
}
#[cfg(not(target_os = "macos"))]
pub fn accessibility_granted() -> bool {
    false
}

/// First run = a fresh install with nothing to resume: **no learning data**
/// (`motor_map.json` absent) **and no Accessibility grant**. Either one present
/// ⇒ an existing user, so capture starts immediately and onboarding is skipped.
///
/// Deliberately narrow (product decision): `motor_map.json` OR
/// `AXIsProcessTrusted()`, NOT "any file under `~/.typeassist`" — a stray empty
/// dir or a lone config file must not suppress a genuine first-run onboarding.
/// This is the single source of truth for both the setup spawn-defer decision
/// and the `is_first_run` command the webview reads, so the two can't disagree.
pub fn is_first_run() -> bool {
    let has_learning_data = motor_map_path().map(|p| p.exists()).unwrap_or(false);
    !has_learning_data && !accessibility_granted()
}

/// `~/.typeassist/word_patterns.json` — the live word-pattern store (C5d),
/// its OWN file beside the motor map. Deliberately NOT under `snapshots/`:
/// the Practice-trend reader loads every file there as a `MotorMap`, so a
/// differently-shaped file in that directory would break it.
fn word_patterns_path() -> Option<PathBuf> {
    typeassist_dir().map(|d| d.join("word_patterns.json"))
}

/// `~/.typeassist/guess_accuracy.json` — the observe-only guesser accuracy
/// scoreboard (M3 accuracy-gated suggestion work, Phase 1). Its OWN file beside
/// the word-pattern store; measurement only, never read back into correction.
fn guess_accuracy_path() -> Option<PathBuf> {
    typeassist_dir().map(|d| d.join("guess_accuracy.json"))
}

/// `~/.typeassist/snapshots` — the daily dated archive directory.
fn snapshots_dir() -> Option<PathBuf> {
    typeassist_dir().map(|d| d.join("snapshots"))
}

/// `~/.typeassist/word_freq.json` — the local vocabulary tally (C5e),
/// observe-only, privacy-gated to `is_known` words. Its OWN file beside the
/// motor map (counts only — no order, no context).
fn word_freq_path() -> Option<PathBuf> {
    typeassist_dir().map(|d| d.join("word_freq.json"))
}

/// `~/.typeassist/word_freq_snapshots` — the word-tally's daily dated archive.
/// Deliberately its OWN directory, NOT under `snapshots/`: the Practice-trend
/// reader loads every file in `snapshots/` as a `MotorMap`, so a differently-
/// shaped file there would break it (same rule as `word_patterns_path`).
fn word_freq_snapshots_dir() -> Option<PathBuf> {
    typeassist_dir().map(|d| d.join("word_freq_snapshots"))
}

/// `~/.typeassist/allow_list.json` — the manual correction allow-list + master
/// gate (M3 correction Step 1). The engine task is its sole writer; the UI
/// reads it read-only. Its own file beside the learning stores — it is config,
/// not learned data. `None` when HOME is unset (corrections then run from an
/// in-memory default that ships dark, so nothing fires).
fn allow_list_path() -> Option<PathBuf> {
    typeassist_dir().map(|d| d.join("allow_list.json"))
}

/// `~/.typeassist/shadow_suggestions.log` — the **live dry-run** shadow log
/// (M3 observe-only). Whenever the classifier WOULD surface a suggestion for a
/// just-sealed word, one line is appended here; **nothing is applied to the
/// user's text.** A tail-able diagnostic so the builder can type and watch what
/// the engine *would* have suggested in real time. Honors `TYPEASSIST_DATA_DIR`.
/// `None` when HOME is unset. (No new privacy surface: the same `typed → target`
/// pairs already live in `word_patterns.json`.)
fn shadow_log_path() -> Option<PathBuf> {
    typeassist_dir().map(|d| d.join("shadow_suggestions.log"))
}

/// Minimum interval between live motor-map flushes when there are unsaved
/// observations. The watchdog checks every 1s; flushing at most this often
/// bounds force-quit data loss to ~this window while keeping disk writes
/// modest during continuous typing. See [`flush_motor_map`].
const MOTOR_FLUSH_INTERVAL_MS: u64 = 2_000;

/// Idle gap after which the watchdog drops the live line buffer (+ caret +
/// anchors) and starts the next keystroke on a clean line.
///
/// The line buffer is a dead-reckoned mirror of the focused field, updated
/// only from keystrokes. An unobserved caret/content change — a mouse-click
/// reposition, Up/Down in a multi-line field, an app/field switch — can
/// desync it and leave a stale tail to the RIGHT of the caret, which turns
/// every later keystroke into a mid-line replay (the source of the
/// "edndd"-tail artifact cascade). We can't read the field to resync, but a
/// real pause is a safe moment to discard the stale buffer. Set well above
/// the resolver's Kept/Abandoned idle thresholds so pending verdicts resolve
/// first; the buffer is transient, so nothing persisted is lost (Principle
/// #8). This is the conservative half of the desync fix — the durable
/// focus-change/echo-robustness signals are the tracked Fix-B follow-up.
const LINE_IDLE_RESET_MS: u64 = 30_000;

/// Watchdog ticks between periodic `EVT_MOTOR_STABILITY` emits. The report
/// changes slowly, so 30 s keeps a passive consumer (debug panel) current
/// without spam; Practice pulls fresh on demand via the control command.
const MOTOR_STABILITY_EMIT_TICKS: u64 = 30;

/// How many weakest keys the [`StabilityReport`] preview carries (Practice
/// curriculum source). A handful is plenty — Practice shows a few at a time.
const WEAKEST_PREVIEW_N: usize = 8;

/// UTC civil date `(year, month, day)` from ms-since-epoch. Howard
/// Hinnant's `civil_from_days` — exact, branch-light, no date crate.
fn ymd_from_epoch_ms(ms: u64) -> (i64, u32, u32) {
    let days = (ms / 86_400_000) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Inverse of [`ymd_from_epoch_ms`] (midnight UTC) — `days_from_civil`.
/// Used to compare existing dated snapshots against the daily cadence.
fn epoch_ms_from_ymd(y: i64, m: u32, d: u32) -> u64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146_097 + doe - 719_468;
    (days.max(0) as u64) * 86_400_000
}

/// `YYYY-MM-DD` for the snapshot filename stem.
fn snapshot_date(ms: u64) -> String {
    let (y, m, d) = ymd_from_epoch_ms(ms);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The machine's LOCAL UTC offset in seconds (east positive) for the instant
/// `epoch_ms`, via libc `localtime_r` → `tm_gmtoff`. DST-correct because the
/// offset is resolved *for that instant*. Falls back to `0` (UTC) if the clock
/// read fails — never panics on a date derivation. macOS-only (L5 app shell —
/// L2–L4 stay OS-agnostic); the non-macOS fallback keeps the workspace building.
#[cfg(target_os = "macos")]
fn local_utc_offset_secs(epoch_ms: u64) -> i64 {
    let t = (epoch_ms / 1000) as libc::time_t;
    // SAFETY: `localtime_r` writes the broken-down time into our stack `tm` and
    // returns a pointer to it (or null on failure). Both pointers are valid for
    // the call; we read `tm_gmtoff` only on the non-null (success) path.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let res = unsafe { libc::localtime_r(&t, &mut tm) };
    if res.is_null() {
        0
    } else {
        tm.tm_gmtoff as i64
    }
}
#[cfg(not(target_os = "macos"))]
fn local_utc_offset_secs(_epoch_ms: u64) -> i64 {
    0
}

/// LOCAL civil date `(year, month, day)` for an epoch-ms instant — the same
/// Hinnant math as [`ymd_from_epoch_ms`], but on the instant shifted into local
/// time so the day rolls at the user's **local midnight**, not UTC's. This is
/// the Today Words/Slip-rate tally boundary (v1.5 item 3).
///
/// Deliberately scoped to the display/tally path ONLY: the dated motor snapshots
/// ([`snapshot_date`], Principle #6 durable history) stay on UTC, so within a few
/// hours of midnight the Progress tally date and a motor-snapshot filename for
/// the "same" wall-clock day can differ by one. Accepted divergence — the two
/// serve different jobs (live daily rollup vs immutable history) and are never
/// joined on date.
fn local_ymd_from_epoch_ms(ms: u64) -> (i64, u32, u32) {
    let offset_ms = local_utc_offset_secs(ms) * 1000;
    let shifted = (ms as i64 + offset_ms).max(0) as u64;
    ymd_from_epoch_ms(shifted)
}

/// Parse a `YYYY-MM-DD.json` snapshot filename back to a civil date, or
/// `None` if it isn't one. Lets the cadence survive app restarts (seeded
/// from the newest file on disk rather than an in-memory-only timestamp).
fn parse_snapshot_date(filename: &str) -> Option<(i64, u32, u32)> {
    let stem = filename.strip_suffix(".json")?;
    let mut parts = stem.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

// ---- Progress daily stats (Statistics tab) ---------------------------------
//
// A tiny append-only daily rollup the Progress view's Statistics tab reads:
// per calendar day, words typed + slips, split into coordination vs precision
// (see `slip_class`). This is the ONLY new persisted file — the motor map and
// word-pattern stores keep their existing formats untouched. Today's row
// updates live; past days are immutable once the calendar day rolls.

/// On-disk shape version for `progress_snapshots.json`.
const PROGRESS_SNAPSHOTS_VERSION: u32 = 1;

/// Minimum interval between live `progress_snapshots.json` writes when the
/// day's tally has unsaved increments. Matches the motor-map flush cadence so
/// "today" tracks within ~2s without churning the disk.
const PROGRESS_FLUSH_INTERVAL_MS: u64 = 2_000;

/// `~/.typeassist/progress_snapshots.json` — the append-only daily rollup.
fn progress_snapshots_path() -> Option<PathBuf> {
    typeassist_dir().map(|d| d.join("progress_snapshots.json"))
}

/// One calendar day's typing rollup, as persisted. `date` is `YYYY-MM-DD` in the
/// machine's **LOCAL** calendar (v1.5 item 3 — the day rolls at the user's local
/// midnight via [`local_ymd_from_epoch_ms`], so "Words today" matches the user's
/// wall clock). NOTE: this is the display/tally path only — the dated motor
/// snapshots ([`snapshot_date`]) stay UTC, so near midnight a progress row's date
/// and a same-day snapshot filename can differ by one (accepted; see
/// `local_ymd_from_epoch_ms`). `coord + precis == slips` always (every counted
/// slip classifies as exactly one).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DailyEntry {
    date: String,
    words: u64,
    slips: u64,
    coord: u64,
    precis: u64,
}

/// The file: a version tag + the accumulated daily history.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProgressSnapshots {
    version: u32,
    days: Vec<DailyEntry>,
}

/// In-memory tally for the CURRENT day. Increments live in `tick_resolver`; the
/// watchdog upserts it into the file and rolls it at the day boundary.
#[derive(Debug, Clone)]
struct DailyTally {
    /// Civil date this tally is for (the watchdog rolls it when `now` differs).
    date: (i64, u32, u32),
    words: u64,
    slips: u64,
    coord: u64,
    precis: u64,
    /// Unwritten increments since the last flush — gates the periodic write.
    dirty: bool,
}

impl DailyTally {
    fn new(date: (i64, u32, u32)) -> Self {
        Self {
            date,
            words: 0,
            slips: 0,
            coord: 0,
            precis: 0,
            dirty: false,
        }
    }

    fn date_str(&self) -> String {
        let (y, m, d) = self.date;
        format!("{y:04}-{m:02}-{d:02}")
    }

    fn to_entry(&self) -> DailyEntry {
        DailyEntry {
            date: self.date_str(),
            words: self.words,
            slips: self.slips,
            coord: self.coord,
            precis: self.precis,
        }
    }

    /// A word landed (a `Kept` clean word, or a `CorrectedToOther` that also
    /// counts as a typed word). Bumps the slip-rate denominator.
    fn add_word(&mut self) {
        self.words += 1;
        self.dirty = true;
    }

    /// A motor slip resolved — record it under its class. The caller has
    /// already counted the word via [`Self::add_word`], so the two %s
    /// (coord / precis, each over `words`) sum to the slip rate.
    fn add_slip(&mut self, class: SlipClass) {
        self.slips += 1;
        match class {
            SlipClass::Coordination => self.coord += 1,
            SlipClass::Precision => self.precis += 1,
        }
        self.dirty = true;
    }
}

/// Read the accumulated daily history, or an empty list if absent/unparseable
/// (a missing file is the honest "no data yet" state, never an error).
fn read_progress_days(path: &Path) -> Vec<DailyEntry> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<ProgressSnapshots>(&bytes) {
            Ok(s) => s.days,
            Err(e) => {
                // Corrupt file: move it aside (never wipe accumulated history)
                // and start empty, rather than silently dropping it.
                correction_engine::persist::quarantine_corrupt(path, e);
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Seed the current-day tally from disk so a mid-day restart RESUMES today's
/// counts (Principle #6/#8 — never silently drop the morning's data on a
/// relaunch) instead of overwriting them with a fresh zero on the next flush.
fn load_daily_tally(path: Option<&Path>, now: u64) -> DailyTally {
    let mut tally = DailyTally::new(local_ymd_from_epoch_ms(now));
    let today = tally.date_str();
    if let Some(path) = path {
        if let Some(e) = read_progress_days(path)
            .into_iter()
            .find(|e| e.date == today)
        {
            tally.words = e.words;
            tally.slips = e.slips;
            tally.coord = e.coord;
            tally.precis = e.precis;
        }
    }
    tally
}

/// Upsert the tally's day into the file, preserving every other day. Atomic
/// (temp + rename). Only the row for `tally.date` is replaced or appended —
/// past days are never mutated.
fn write_progress(path: &Path, tally: &DailyTally) -> std::io::Result<()> {
    let mut days = read_progress_days(path);
    let entry = tally.to_entry();
    match days.iter_mut().find(|e| e.date == entry.date) {
        Some(slot) => *slot = entry,
        None => days.push(entry),
    }
    days.sort_by(|a, b| a.date.cmp(&b.date)); // YYYY-MM-DD sorts chronologically
    let snap = ProgressSnapshots {
        version: PROGRESS_SNAPSHOTS_VERSION,
        days,
    };
    let json = serde_json::to_vec_pretty(&snap).map_err(std::io::Error::other)?;
    // Durable atomic write (temp → fsync → rename → fsync dir) so a crash can't
    // leave a zero-length or torn progress history.
    correction_engine::persist::durable_write(path, &json)
}

/// Watchdog-driven progress persistence + day rollover. On a calendar-day
/// change, finalize the (now-complete) old day — only if it had data, so empty
/// days never clutter the history — then open a fresh tally. Otherwise flush
/// today at most every [`PROGRESS_FLUSH_INTERVAL_MS`] when it has unsaved
/// increments. Returns `true` iff a write succeeded. On a rollover write
/// failure the old tally is kept (not reset) so the day retries next tick
/// rather than being silently lost.
fn tick_progress(
    tally: &mut DailyTally,
    path: Option<&Path>,
    now: u64,
    last_save_ms: &mut u64,
) -> bool {
    let today = local_ymd_from_epoch_ms(now);

    if today != tally.date {
        let mut wrote = false;
        if tally.words > 0 || tally.slips > 0 {
            if let Some(path) = path {
                match write_progress(path, tally) {
                    Ok(()) => wrote = true,
                    Err(e) => {
                        tracing::warn!("progress rollover write failed: {e}");
                        return false; // keep old tally; retry before resetting
                    }
                }
            }
        }
        *tally = DailyTally::new(today);
        *last_save_ms = now;
        return wrote;
    }

    if !tally.dirty || now.saturating_sub(*last_save_ms) < PROGRESS_FLUSH_INTERVAL_MS {
        return false;
    }
    let Some(path) = path else {
        return false; // no durable file this session
    };
    match write_progress(path, tally) {
        Ok(()) => {
            *last_save_ms = now;
            tally.dirty = false;
            true
        }
        Err(e) => {
            tracing::warn!("progress snapshot write failed: {e}");
            false
        }
    }
}

/// How many well-sampled keys to pull from each historical snapshot when
/// building a trend. Large enough to cover any key the user might have
/// practiced (a stability report only carries gated, trustworthy keys).
const TREND_LOOKUP_N: usize = 64;

/// The menu-bar "capture stopped" icon:
/// the same hand glyph as the active state, with a diagonal slash composited
/// over it — the universal "off" look (like wifi-off). On and Off are one
/// identity differing only by the slash. Derived from `tray-icon.png` at
/// runtime (so it tracks whatever base art is dropped in) as a **template**
/// (alpha-only; macOS recolours it for the light/dark bar), so the alarm reads
/// by SHAPE, never colour — NEVER red (a11y + the no-deficit-framing rule).
///
/// The base hand is a *filled* glyph, so a same-colour slash drawn straight on
/// top would be invisible inside the silhouette (template tints everything one
/// colour). To make the slash read *through* the hand we carve a thin
/// transparent **gutter** around it and lay the slash line inside that gutter —
/// the groove + line is visible over filled and empty pixels alike.
fn capture_off_icon() -> tauri::image::Image<'static> {
    let base = tauri::include_image!("icons/tray-icon.png");
    let (w, h) = (base.width(), base.height());
    let n = w as f32; // square asset; width drives the slash geometry
    let mut rgba = base.rgba().to_vec();

    // Diagonal slash, top-right → bottom-left, inset from the edges. Geometry
    // scales with the asset so it holds if the art is dropped at another size.
    let inset = n * (7.0 / 44.0);
    let (ax, ay) = (n - inset, inset);
    let (bx, by) = (inset, n - inset);
    let slash = n * (4.5 / 44.0); // slash stroke width
    let gutter = slash * 1.9; // transparent groove width around the slash
    let seg_dist = |px: f32, py: f32| -> f32 {
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
        let (qx, qy) = (ax + t * dx, ay + t * dy);
        ((px - qx).powi(2) + (py - qy).powi(2)).sqrt()
    };

    for y in 0..h {
        for x in 0..w {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let d = seg_dist(px, py);
            let line_cov = (1.0 - (d - slash / 2.0)).clamp(0.0, 1.0); // the slash itself
            let gutter_cov = (1.0 - (d - gutter / 2.0)).clamp(0.0, 1.0); // the groove
            let i = ((y * w + x) * 4) as usize;
            // Carve the gutter out of the base alpha, then lay the slash back in.
            let a0 = rgba[i + 3] as f32 / 255.0;
            let carved = a0 * (1.0 - gutter_cov);
            let final_a = carved.max(line_cov);
            rgba[i + 3] = (final_a * 255.0) as u8;
        }
    }
    tauri::image::Image::new_owned(rgba, w, h)
}

/// Set the menu-bar icon from the single capture-health signal, only touching
/// the OS when the rendered state changes (tracked via `last`). TWO states by
/// design: **On** (`tray-icon.png`, the plain hand) and **Off** (the same hand
/// + diagonal slash, [`capture_off_icon`]) when capture is stopped. Both are
/// **template** images (monochrome, OS-recoloured) so the signal is shape,
/// never colour.
fn apply_tray_icon<R: Runtime>(app: &AppHandle<R>, not_active: bool, last: &mut Option<bool>) {
    if *last == Some(not_active) {
        return;
    }
    if let Some(tray) = app.tray_by_id("main-tray") {
        let icon = if not_active {
            capture_off_icon()
        } else {
            tauri::include_image!("icons/tray-icon.png")
        };
        let _ = tray.set_icon(Some(icon));
        let _ = tray.set_icon_as_template(true);
        *last = Some(not_active);
    }
}

/// One `(date, slip_rate)` sample in a key's [`PracticeTrend`] series.
#[derive(Serialize, Clone)]
struct TrendPoint {
    /// `YYYY-MM-DD` of the daily snapshot this point came from.
    date: String,
    /// Decayed slip rate for the key as of that snapshot, `[0,1]`.
    slip_rate: f32,
}

/// A single key's slip-rate trend across the daily snapshots. `points` holds
/// only the weeks where the key cleared the sample bar — so a sparse history is
/// honest rather than back-filled with invented numbers.
#[derive(Serialize, Clone)]
struct KeyTrend {
    key: char,
    points: Vec<TrendPoint>,
}

/// Payload for [`EVT_PRACTICE_TREND`].
#[derive(Serialize, Clone)]
struct PracticeTrend {
    keys: Vec<KeyTrend>,
    generated_at: u64,
}

/// Reconstruct a per-key slip-rate trend from the daily snapshot archives.
/// Loads each dated snapshot, reads its gated stability report, and records a
/// point for each requested key that the snapshot sampled well enough to trust.
/// Pure read of on-disk history — touches no live state. Returns an empty
/// series (no points) when the directory is missing or holds < 1 snapshot.
fn build_practice_trend(dir: Option<&Path>, keys: &[char], now: u64) -> PracticeTrend {
    let mut series: Vec<KeyTrend> = keys
        .iter()
        .map(|&key| KeyTrend {
            key,
            points: vec![],
        })
        .collect();

    if let Some(dir) = dir {
        // Collect dated snapshots oldest → newest so each series reads in time
        // order (the trend line is drawn left = older, right = newer).
        let mut dated: Vec<(u64, PathBuf, String)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let s = name.to_str()?;
                let (y, m, d) = parse_snapshot_date(s)?;
                Some((epoch_ms_from_ymd(y, m, d), entry.path(), s.to_string()))
            })
            .collect();
        dated.sort_by_key(|(ms, _, _)| *ms);

        for (_, path, filename) in &dated {
            let Ok(snap) = MotorMap::load_from(path) else {
                continue;
            };
            // The gated weakest list is exactly "keys with a trustworthy slip
            // rate" — reuse it as the per-key lookup for this snapshot.
            let lookup: std::collections::HashMap<char, f32> = snap
                .stability_report(TREND_LOOKUP_N)
                .weakest
                .into_iter()
                .collect();
            let date = filename.strip_suffix(".json").unwrap_or(filename);
            for kt in &mut series {
                if let Some(&slip_rate) = lookup.get(&kt.key) {
                    kt.points.push(TrendPoint {
                        date: date.to_string(),
                        slip_rate,
                    });
                }
            }
        }
    }

    PracticeTrend {
        keys: series,
        generated_at: now,
    }
}

/// The newest dated snapshot in `dir`, as ms-since-epoch — `None` if the
/// directory is missing/empty or holds no parseable dated file.
fn most_recent_snapshot_ms(dir: &Path) -> Option<u64> {
    let mut best: Option<u64> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        if let Some((y, m, d)) = name.to_str().and_then(parse_snapshot_date) {
            let ms = epoch_ms_from_ymd(y, m, d);
            best = Some(best.map_or(ms, |b| b.max(ms)));
        }
    }
    best
}

/// Payload for [`EVT_LEXICON_PROPOSAL`]. `proposal: None` means the
/// proposal was retracted (its only contributing record rolled back
/// under C5a's revisable transitions).
#[derive(Serialize, Clone)]
struct LexiconProposalEvent {
    word: String,
    proposal: Option<correction_engine::LexiconProposal>,
}

/// Payload for [`EVT_LEARNING_PAUSED`]. Single field rather than a
/// bare bool so the JSON has a stable shape if future controls are
/// added (e.g. a reason string for "auto-paused" vs "manual").
#[derive(Serialize, Clone)]
struct LearningPausedEvent {
    paused: bool,
}

/// Payload for [`EVT_INPUT_PAUSED`]. Same shape as learning-paused so
/// the panel can treat the two echoes identically (different stores,
/// same listener style).
#[derive(Serialize, Clone)]
struct InputPausedEvent {
    paused: bool,
}

/// Payload for [`EVT_MOTOR_KEYSTROKE`]. Carries the per-keystroke
/// motor anomaly probe alongside identifying metadata so the panel
/// can render "what just happened" without needing to re-parse the
/// key event stream. `hand` / `finger` are `None` for keys outside
/// the touch-typing map (in which case `anomaly` is `None` too —
/// nothing to score).
#[derive(Serialize, Clone)]
struct MotorKeystrokeEvent {
    key: String,
    timestamp_ms: u64,
    dwell_ms: u32,
    hand: Option<volatility_map::Hand>,
    finger: Option<volatility_map::Finger>,
    anomaly: Option<behavioural_model::motor_baseline::KeystrokeAnomaly>,
}

/// Run the outcome resolver and surface every transition. **Two passes,
/// two ledgers, one shared verdict machine** (the C5c decoupling):
///
///   * **Decision pass (C5b).** Resolves the [`DecisionLedger`] — *unknown*
///     words only (gated by `should_log`). Applies transitions, broadcasts
///     `EVT_LOG_RECORD_UPDATED`, re-notes the lexicon proposer. The motor
///     map is NOT fed here.
///   * **Motor pass (C5c).** Resolves the [`MotorLedger`] — *every*
///     motor-evidenced word (known included), candidate-agnostic. Each
///     `Kept` / `CorrectedToOther` is folded into `motor_map` (the funnel's
///     verdict + observation counters live here, since this is the capture
///     pipeline Principle #7 reconciles). A `MotorRecord` never carries a
///     candidate, so every correction reads as a `CorrectedToOther` slip.
///
/// Each uses its own resolver instance (independent stability caches). Both
/// run on every anchor-affecting edit AND the idle watchdog. Live-map
/// persistence is NOT done here — [`flush_motor_map`] handles it on a timer.
#[allow(clippy::too_many_arguments)]
fn tick_resolver<R: Runtime>(
    app: &AppHandle<R>,
    resolver: &mut OutcomeResolver,
    motor_resolver: &mut OutcomeResolver,
    anchors: &AnchorTracker,
    line_buf: &[char],
    caret: usize,
    ledger: &mut DecisionLedger,
    motor_ledger: &mut MotorLedger,
    proposer: &mut LexiconProposer,
    motor_map: &mut MotorMap,
    word_freq: &mut WordFreq,
    prompted_capture_active: bool,
    word_patterns: &mut WordPatternStore,
    guess_ledger: &mut GuessLedger,
    funnel: &mut Funnel,
    tally: &mut DailyTally,
    competitor_sense: &mut CompetitorSense,
) {
    let now = now_ms();

    // --- Decision pass (C5b lexicon proposer): unknown words only. ---
    let dchanges = resolver.tick(now, anchors.anchors(), line_buf, caret, ledger.iter());
    for (record_id, outcome) in dchanges {
        tracing::info!("DECISION_VERDICT rid={} -> {:?}", record_id, outcome);
        if ledger.resolve_outcome(record_id, outcome) {
            // Write the `credited` slot BEFORE the emit so the panel sees the
            // right value on the same event. Kept → reflects the proposer's
            // pause state at note-time; non-Kept → None.
            let credited = if matches!(outcome, Outcome::Kept) {
                Some(!proposer.credit_paused())
            } else {
                None
            };
            ledger.set_credited(record_id, credited);
            if let Some(rec) = ledger.get(record_id).cloned() {
                let _ = app.emit(EVT_LOG_RECORD_UPDATED, rec.clone());
                emit_proposal_change(app, proposer, &rec);
            }
        }
    }

    // --- Motor pass (C5c motor map): EVERY motor-evidenced word. ---
    let mchanges =
        motor_resolver.tick(now, anchors.anchors(), line_buf, caret, motor_ledger.iter());
    for (record_id, outcome) in mchanges {
        tracing::info!("RESOLVE_OUTCOME rid={} -> {:?}", record_id, outcome);
        if motor_ledger.resolve_outcome(record_id, outcome) {
            // Funnel (Principle #7): a verdict was assigned in the capture
            // pipeline. Counts transitions, so revisions can tally > once.
            // Same-day stats (Statistics tab): a resolved word is one typed
            // word. Both Kept (clean) and CorrectedToOther (slipped) count
            // toward today's words — the slip-rate denominator. Like the funnel
            // these count verdict transitions, so a re-edited word can tally
            // more than once (a mild, documented bias while observe-only).
            match outcome {
                Outcome::Kept => {
                    funnel.v_kept += 1;
                    tally.add_word();
                }
                Outcome::CorrectedToSuggestion => funnel.v_corr_sug += 1,
                Outcome::CorrectedToOther => {
                    funnel.v_corr_oth += 1;
                    tally.add_word();
                }
                Outcome::Abandoned => funnel.v_abandoned += 1,
                Outcome::Pending => {}
            }
            if let Some(rec) = motor_ledger.get(record_id).cloned() {
                // Fold the outcome into the motor map (observe-and-store
                // only; the kill-switch stays off).
                let report = match outcome {
                    Outcome::Kept => {
                        // C5e local vocabulary tally (observe-only, privacy-
                        // gated): a Kept word is clean, left-in vocabulary, so
                        // tally it for the future personal-frequency scorer.
                        // `observe_kept` itself enforces the is_known gate, so a
                        // name/password/junk token is never written to disk.
                        // Only Kept feeds this — corrected/abandoned words are a
                        // slip, not vocabulary. SKIP while a warm-up / Practice
                        // round is on screen: that text is our own prompts, not
                        // the user's vocabulary (the motor map below still
                        // observes it — practising weak keys is its purpose).
                        if !prompted_capture_active {
                            word_freq.observe_kept(&rec.original_text, Lexicon::shared(), now);
                        }
                        motor_map.observe_outcome(outcome, &rec.original_text, None, now)
                    }
                    Outcome::CorrectedToOther => {
                        let corrected = correction_engine::resolver::post_edit_text(
                            &rec,
                            anchors.anchors(),
                            line_buf,
                        );
                        // Watch Dog Not Attack Dog (M3 autocorrect coexistence):
                        // a word was just corrected toward a COMMON target that
                        // Jordan didn't cause (his own accepts are echo-skipped
                        // before this pipeline, so they never reach here). That's
                        // a competing-corrector footprint — sense it so the fire
                        // gate can defer on the redundant lane. OUTPUT-path only:
                        // this reads the resolved pair and bumps the behavioural
                        // sense; the learning writes below are untouched. Skip
                        // during Practice/warm-up — that text is our own prompts,
                        // not ambient host activity. (The self-correction case is
                        // made moot by scope: deferral only ever applies to the
                        // host-redundant common lane; idiosyncratic always fires.)
                        if !prompted_capture_active {
                            if let Some(c) = corrected.as_deref() {
                                if correction_engine::is_host_redundant(
                                    &rec.original_text,
                                    c,
                                    Lexicon::shared(),
                                ) {
                                    competitor_sense.note_footprint(now);
                                }
                            }
                        }
                        // C5d word-pattern store: learn the typed→target pair
                        // (observe-only; kill-switch off). Counts observed vs
                        // skipped so the rewrite-filter drop is auditable.
                        match corrected.as_deref() {
                            Some(c) => {
                                // Guard 1 (soft): only record a pair whose
                                // intended `target` is a real word — is_known
                                // (bundled dict OR the user's learned set) OR
                                // letter-trigram plausible. Rejects the
                                // "edndd"-tail re-tokenization artifacts (each
                                // carries a dictionary-absent trigram) without
                                // blocking genuine vocabulary. Gates BOTH the
                                // scoreboard and the word-pattern store below so
                                // the two populations stay consistent.
                                let target_recordable =
                                    target_is_recordable(&normalize_word(c), Lexicon::shared());
                                // --- Guesser accuracy scoreboard (M3 Phase 1,
                                // observe-only) — PREDICT, then learn. Ask the
                                // guesser for its top guess of `typed` using the
                                // word-pattern model AS IT STANDS NOW, BEFORE
                                // this correction is folded in below, so the
                                // score is genuinely out-of-sample. Measurement
                                // only: nothing fires, the master gate is
                                // untouched, the existing stores are unchanged.
                                // Runs only on a resolved CorrectedToOther (rare,
                                // human-paced) — never on the typing hot path.
                                {
                                    let typed_n = normalize_word(&rec.original_text);
                                    let target_n = normalize_word(c);
                                    // Skip no-ops (typed == target after
                                    // normalization) so a pair that isn't a real
                                    // slip never enters the scoreboard — the same
                                    // guard the word-pattern store applies, so the
                                    // two stay consistent. (A no-op forces the
                                    // guesser to pick a *different* word, which it
                                    // then "misses", dragging the hit-rate down.)
                                    if !typed_n.is_empty()
                                        && !target_n.is_empty()
                                        && typed_n != target_n
                                        && target_recordable
                                    {
                                        let snaps = word_patterns.snapshots();
                                        let model = correction_engine::guesser::build_model(
                                            snaps.iter().map(|s| {
                                                (
                                                    s.target.as_str(),
                                                    s.typed.as_str(),
                                                    s.weight as f64,
                                                )
                                            }),
                                        );
                                        let g = correction_engine::guesser::guess(
                                            &model,
                                            &typed_n,
                                            Lexicon::shared(),
                                        );
                                        // Within-guard = the ≤2-edit typo-fix
                                        // population the store learns / the
                                        // offline benchmark runs on; tagged so
                                        // both populations are readable.
                                        let len_diff = typed_n
                                            .chars()
                                            .count()
                                            .abs_diff(target_n.chars().count());
                                        let within_guard =
                                            correction_engine::edit_distance(&typed_n, &target_n)
                                                <= MAX_PATTERN_EDIT_DISTANCE
                                                && len_diff <= MAX_PATTERN_LENGTH_DIFF;
                                        guess_ledger.record(
                                            &typed_n,
                                            g.as_ref(),
                                            &target_n,
                                            within_guard,
                                            now,
                                        );
                                        // debug!, not info!: prints raw typed +
                                        // guess + target words (privacy).
                                        tracing::debug!(
                                            "GUESS_SCORED typed={:?} guess={:?} conf={:.2} target={:?} hit={} within_guard={}",
                                            typed_n,
                                            g.as_ref().map(|g| g.word.as_str()),
                                            g.as_ref().map(|g| g.confidence).unwrap_or(0.0),
                                            target_n,
                                            g.as_ref().map(|g| g.word == target_n).unwrap_or(false),
                                            within_guard,
                                        );
                                    }
                                }
                                // Same-day slip rate (Statistics): a motor typo
                                // (not a semantic rewrite) is a slip, split
                                // coordination vs precision. Derived from the
                                // pair — nothing new persisted to the stores.
                                if let Some(class) = classify_slip(&rec.original_text, c) {
                                    tally.add_slip(class);
                                }
                                if target_recordable
                                    && word_patterns
                                        .observe_correction(outcome, &rec.original_text, c, now)
                                        .recorded
                                {
                                    funnel.word_patterns_observed += 1;
                                    // Read-only (kill-switch OFF): log what this
                                    // just-updated pattern WOULD be classified as,
                                    // so the M3 decision is observable as evidence
                                    // accrues (e.g. the flip to Suggest when weight
                                    // crosses the risk-tiered bar). No injection.
                                    let readiness = correction_engine::classify(
                                        &rec.original_text,
                                        c,
                                        word_patterns,
                                        Lexicon::shared(),
                                        motor_map,
                                    );
                                    // debug!, not info!: prints raw typed + target words (privacy).
                                    tracing::debug!(
                                        "KILL_SWITCH_CLASSIFY typed={:?} target={:?} -> {:?}",
                                        rec.original_text,
                                        c,
                                        readiness
                                    );
                                } else {
                                    funnel.word_pattern_skipped += 1;
                                }
                            }
                            // CorrectedToOther with no recoverable post-edit
                            // text — can't form a pattern.
                            None => funnel.word_pattern_skipped += 1,
                        }
                        motor_map.observe_outcome(
                            outcome,
                            &rec.original_text,
                            corrected.as_deref(),
                            now,
                        )
                    }
                    // Abandoned / Pending carry no key-for-key intent in v0.
                    // (CorrectedToSuggestion never arises from the motor ledger.)
                    _ => ObserveReport::default(),
                };
                // Funnel: char-level observations folded in (5c boundary).
                funnel.motor_kept += u64::from(report.correct);
                funnel.motor_slip += u64::from(report.slips);
            }
        }
    }
}

/// Flush the live motor map to disk if it has unsaved observations and at
/// least [`MOTOR_FLUSH_INTERVAL_MS`] has passed since the last save.
/// Driven by the watchdog so the live file is maintained on a time cadence
/// — closing the data-loss window that the (unreliable) shutdown save and
/// the coarse 100-observation mark leave open. `last_save_ms` is advanced
/// only on a successful write, so a failed write retries next tick.
///
/// Returns `true` iff a write succeeded this call (so the caller can bump
/// the capture funnel's `c_motor_saves`).
fn flush_motor_map(
    motor_map: &mut MotorMap,
    path: Option<&Path>,
    now: u64,
    last_save_ms: &mut u64,
) -> bool {
    if !motor_map.has_unsaved() {
        return false;
    }
    if now.saturating_sub(*last_save_ms) < MOTOR_FLUSH_INTERVAL_MS {
        return false;
    }
    let Some(path) = path else {
        return false; // no HOME — in-memory only this session
    };
    match motor_map.save_to(path) {
        Ok(()) => {
            *last_save_ms = now;
            tracing::info!(
                "MOTOR_MAP_SAVED (flush) obs={} path={:?}",
                motor_map.total_observations(),
                path
            );
            true
        }
        Err(e) => {
            tracing::warn!("motor map flush save failed: {e}");
            false
        }
    }
}

/// Flush the live word-pattern store (C5d) on the same time cadence as
/// [`flush_motor_map`] — its own file, its own last-save clock. Returns
/// `true` iff a write succeeded this call (so the caller can bump the funnel's
/// `c_word_pattern_saves`).
fn flush_word_patterns(
    store: &mut WordPatternStore,
    path: Option<&Path>,
    now: u64,
    last_save_ms: &mut u64,
) -> bool {
    if !store.has_unsaved() {
        return false;
    }
    if now.saturating_sub(*last_save_ms) < MOTOR_FLUSH_INTERVAL_MS {
        return false;
    }
    let Some(path) = path else {
        return false; // no HOME — in-memory only this session
    };
    match store.save_to(path) {
        Ok(()) => {
            *last_save_ms = now;
            tracing::info!(
                "WORD_PATTERNS_SAVED (flush) patterns={} obs={} path={:?}",
                store.len(),
                store.total_observations(),
                path
            );
            true
        }
        Err(e) => {
            tracing::warn!("word-pattern store flush save failed: {e}");
            false
        }
    }
}

/// Flush the live word-frequency tally (C5e) on the same time cadence as
/// [`flush_motor_map`] — its own file, its own last-save clock. Observe-only;
/// no pruning (it does not decay). Returns whether a write succeeded.
fn flush_word_freq(
    word_freq: &mut WordFreq,
    path: Option<&Path>,
    now: u64,
    last_save_ms: &mut u64,
) -> bool {
    if !word_freq.has_unsaved() {
        return false;
    }
    if now.saturating_sub(*last_save_ms) < MOTOR_FLUSH_INTERVAL_MS {
        return false;
    }
    let Some(path) = path else {
        return false; // no HOME — in-memory only this session
    };
    match word_freq.save_to(path) {
        Ok(()) => {
            *last_save_ms = now;
            tracing::info!(
                "WORD_FREQ_SAVED (flush) words={} tokens={:.0} path={:?}",
                word_freq.len(),
                word_freq.total_count(),
                path
            );
            true
        }
        Err(e) => {
            tracing::warn!("word-freq tally flush save failed: {e}");
            false
        }
    }
}

/// **Data layer — restore.** Replace the live learned stores with a backup file.
///
/// Validate-all-then-apply: parse + version-check + confirm every core store
/// deserializes BEFORE any write (so a bad file never leaves a half-restored
/// disk). [`crate::backup::apply_bundle`] clears the learned/history footprint
/// and writes the bundle's contents; we then reload each in-memory store from
/// the freshly-written file (a cleared/absent store reloads as empty — a true
/// **replace, never merge**). The decay timestamps came over verbatim, so decay
/// continues the backup's curve rather than resetting to now. Runs only on the
/// engine task (the sole writer), so no flush interleaves.
fn restore_from_backup(
    path: &str,
    motor_map: &mut MotorMap,
    word_freq: &mut WordFreq,
    word_patterns: &mut WordPatternStore,
) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("could not read backup: {e}"))?;
    let bundle = crate::backup::parse_bundle(&bytes)?;
    crate::backup::validate_core(&bundle)?;
    let data_dir = typeassist_dir().ok_or_else(|| "no data directory (HOME unset)".to_string())?;
    crate::backup::apply_bundle(&bundle, &data_dir)?;

    *motor_map = match motor_map_path().as_deref() {
        Some(p) if p.exists() => MotorMap::load_from(p).unwrap_or_else(|_| MotorMap::new()),
        _ => MotorMap::new(),
    };
    *word_freq = match word_freq_path().as_deref() {
        Some(p) if p.exists() => WordFreq::load_from(p).unwrap_or_else(|_| WordFreq::new()),
        _ => WordFreq::new(),
    };
    *word_patterns = match word_patterns_path().as_deref() {
        Some(p) if p.exists() => {
            WordPatternStore::load_from(p).unwrap_or_else(|_| WordPatternStore::new())
        }
        _ => WordPatternStore::new(),
    };
    Ok(())
}

/// **Data layer — delete everything.** Erase all learned data + history +
/// diagnostics from disk and reset the in-memory stores, returning the app to a
/// first-launch-clean state. The correction gate (`allow_list`) is reset by the
/// caller, which also persists + echoes it. Disk first, then memory: if a file
/// removal fails we return `Err` with memory untouched (the engine keeps serving
/// the old in-memory state rather than claiming a fresh start that didn't happen).
fn delete_all_local_data(
    motor_map: &mut MotorMap,
    word_freq: &mut WordFreq,
    word_patterns: &mut WordPatternStore,
) -> Result<(), String> {
    let data_dir = typeassist_dir().ok_or_else(|| "no data directory (HOME unset)".to_string())?;
    crate::backup::delete_all_data(&data_dir)?;
    *motor_map = MotorMap::new();
    *word_freq = WordFreq::new();
    *word_patterns = WordPatternStore::new();
    Ok(())
}

/// Flush the guesser accuracy scoreboard to its own file on the same time
/// cadence as the other live stores. Observe-only measurement — no pruning (an
/// accuracy row is history, not a decayed learner). Returns whether a write
/// succeeded this call.
fn flush_guess_ledger(
    ledger: &mut GuessLedger,
    path: Option<&Path>,
    now: u64,
    last_save_ms: &mut u64,
) -> bool {
    if !ledger.has_unsaved() {
        return false;
    }
    if now.saturating_sub(*last_save_ms) < MOTOR_FLUSH_INTERVAL_MS {
        return false;
    }
    let Some(path) = path else {
        return false; // no HOME — in-memory only this session
    };
    match ledger.save_to(path) {
        Ok(()) => {
            *last_save_ms = now;
            tracing::info!(
                "GUESS_LEDGER_SAVED (flush) patterns={} tries={} hits={} path={:?}",
                ledger.len(),
                ledger.overall().tries,
                ledger.overall().hits,
                path
            );
            true
        }
        Err(e) => {
            tracing::warn!("guess-accuracy ledger flush save failed: {e}");
            false
        }
    }
}

/// Observe-only accuracy readout (M3 Phase 1). Counts-only at `info!` (text-free
/// — safe at the default level): overall and within-guard hit-rate plus the
/// τ-sweep. The per-pattern `typed → guess` rows carry raw words, so they go at
/// `debug!` only (privacy — mirrors the word-pattern dumps). No-op when empty.
/// Emitted alongside the 60s funnel dump.
fn dump_guess_accuracy(ledger: &GuessLedger) {
    let o = ledger.overall();
    if o.tries == 0 {
        return;
    }
    let rate = |hits: u64, tries: u64| {
        if tries == 0 {
            0.0
        } else {
            100.0 * hits as f64 / tries as f64
        }
    };
    let by_tau: Vec<String> = o
        .by_tau
        .iter()
        .map(|b| {
            format!(
                "{:.1}:{:.0}%({}/{})",
                b.tau,
                rate(b.hits, b.fired),
                b.hits,
                b.fired
            )
        })
        .collect();
    tracing::info!(
        "GUESS_ACCURACY_DUMP tries={} hits={} rate={:.0}% within_guard={{tries: {}, hits: {}, rate: {:.0}%}} by_tau=[{}]",
        o.tries,
        o.hits,
        rate(o.hits, o.tries),
        o.within_guard_tries,
        o.within_guard_hits,
        rate(o.within_guard_hits, o.within_guard_tries),
        by_tau.join(" "),
    );
    // debug!, not info!: per-pattern rows print raw typed + guess words.
    for (typed, acc) in ledger.rows() {
        tracing::debug!(
            "GUESS_ACCURACY_ROW typed={:?} tries={} hits={} last_guess={:?} last_conf={:.2} last_target={:?}",
            typed,
            acc.tries,
            acc.hits,
            acc.last_guess.as_deref(),
            acc.last_confidence,
            acc.last_target,
        );
    }
}

/// Persist the allow-list (the engine is its sole writer) and broadcast the new
/// state on [`EVT_CORRECTION_STATE`] so the panel + tray master toggle reflect
/// the engine's authoritative state. A save failure is logged but not fatal —
/// the in-memory state stays correct for this session, and the next mutation
/// retries the write.
fn persist_and_emit_allow_list<R: Runtime>(
    app: &AppHandle<R>,
    allow_list: &crate::allow_list::AllowList,
    path: Option<&Path>,
) {
    if let Some(path) = path {
        if let Err(e) = allow_list.save_to(path) {
            tracing::warn!("allow-list save failed: {e}");
        }
    }
    let _ = app.emit(EVT_CORRECTION_STATE, allow_list.clone());
}

/// Write an `InjectCorrection` to the sidecar's stdin: delete the last
/// `delete_count` characters back from the caret, then type `replacement`
/// (which already carries the trailing boundary char). The sidecar tags the
/// synthesized CGEvents (`eventSourceUserData`) so the tap drops their echo —
/// the engine never sees its own injection, so it can't re-learn or re-correct
/// it. Returns whether the command was written (a dead stdin returns false).
fn send_inject_correction(
    child: &mut tauri_plugin_shell::process::CommandChild,
    delete_count: u32,
    replacement: String,
) -> bool {
    let cmd = OutboundCommand::InjectCorrection {
        delete_count,
        replacement,
    };
    let mut line = match serde_json::to_string(&cmd) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to serialize InjectCorrection: {e}");
            return false;
        }
    };
    line.push('\n');
    match child.write(line.as_bytes()) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("InjectCorrection write to sidecar failed: {e}");
            false
        }
    }
}

/// Anchored replace (M3 bubble accept-after-typing-on): Left × `left`,
/// Backspace × `delete_count`, type `replacement`, Right × `right`. The whole
/// burst echoes back through the tap; the caller adds `left + delete_count +
/// replacement.chars() + right` to `pending_echo` to drop it.
fn send_inject_anchored(
    child: &mut tauri_plugin_shell::process::CommandChild,
    left: u32,
    delete_count: u32,
    replacement: String,
    right: u32,
) -> bool {
    let cmd = OutboundCommand::InjectAnchored {
        left,
        delete_count,
        replacement,
        right,
    };
    let mut line = match serde_json::to_string(&cmd) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to serialize InjectAnchored: {e}");
            return false;
        }
    };
    line.push('\n');
    match child.write(line.as_bytes()) {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("InjectAnchored write to sidecar failed: {e}");
            false
        }
    }
}

/// One line of the live shadow dry-run: the strongest learned pattern for a
/// just-typed word that the classifier WOULD surface as a suggestion. Read-only.
struct ShadowSuggestion {
    typed: String,
    target: String,
    tier: &'static str,
    /// Decayed evidence weight the classifier saw (the "N obs" count).
    evidence: f32,
    /// How far off the slip was — Levenshtein edit distance typed→target.
    edit_distance: usize,
    /// Most-affected involved key's slip rate, `[0,1]` (the Motor-Map signal).
    affectedness: f32,
    /// The evidence bar actually applied (eased by affectedness).
    bar_used: f32,
    /// The un-eased tier base bar — what would apply with no motor evidence.
    base_bar: f32,
    /// This suggestion surfaced ONLY because the affected-key bonus eased the
    /// bar (weight is below the base bar). The signal to watch for sharp-vs-noisy.
    surfaced_early: bool,
}

/// Observe-only dry run: for a just-sealed `typed` word, return the strongest
/// learned `typed → target` pattern the risk-tiered, Motor-Map-aware classifier
/// WOULD surface as a suggestion, or `None`. Applies nothing — pure read over the
/// store + lexicon + motor map. `snapshots()` is strongest-first, so the first
/// matching `Suggest` is the best.
///
/// Uses the **length-scaled shadow curve** (`classify_explained_scaled`): a
/// length-scaled non-word evidence bar (2–3 → 4, 4–6 → 3, 7+ → 1; real-word
/// source unchanged at the high bar) and a length-scaled motor-edit budget (2–3 →
/// 1, 4+ → up to 2). Every other gate is unchanged, and nothing is ever applied
/// (`correction_enabled` stays off) — this only changes what the dry-run log says
/// it WOULD suggest. The live Impact ledger and KILL_SWITCH_CLASSIFY line keep the
/// flat 4/12 + budget-1 classifier.
fn shadow_suggestion(
    typed: &str,
    store: &WordPatternStore,
    lexicon: &Lexicon,
    motor_map: &MotorMap,
) -> Option<ShadowSuggestion> {
    let typed_n = normalize_word(typed);
    if typed_n.is_empty() {
        return None;
    }
    for s in store.snapshots() {
        if s.typed != typed_n {
            continue;
        }
        let ex = correction_engine::classify_explained_scaled(
            &s.typed, &s.target, store, lexicon, motor_map,
        );
        if let PatternReadiness::Suggest { tier } = ex.readiness {
            let tier = match tier {
                correction_engine::SuggestTier::NonWordSource => "non_word_source",
                correction_engine::SuggestTier::RealWordSource => "real_word_source",
            };
            let edit_distance = correction_engine::edit_distance(&s.typed, &s.target);
            return Some(ShadowSuggestion {
                typed: s.typed,
                target: s.target,
                tier,
                evidence: s.weight,
                edit_distance,
                affectedness: ex.affectedness,
                bar_used: ex.bar_used,
                base_bar: ex.base_bar,
                surfaced_early: ex.surfaced_early,
            });
        }
    }
    None
}

/// Append one shadow-suggestion line to the tail-able dry-run log. Best-effort:
/// a write failure is logged and dropped (the dry run must never disturb the
/// pipeline). The line is human-readable and `tail -f`-friendly. An `EARLY`
/// marker flags suggestions that surfaced only via the affected-key bonus, with
/// the affectedness score and the bar used, so the builder can judge sharp vs noisy.
fn append_shadow_log(path: &Path, now: u64, s: &ShadowSuggestion) {
    use std::io::Write;
    let marker = if s.surfaced_early {
        "  EARLY(affected-key bonus)"
    } else {
        ""
    };
    let line = format!(
        "{now} would-suggest  {} -> {}  tier={}  evidence={:.1}  edit_dist={}  affected={:.2}  bar={:.1}/{:.1}{}\n",
        s.typed,
        s.target,
        s.tier,
        s.evidence,
        s.edit_distance,
        s.affectedness,
        s.bar_used,
        s.base_bar,
        marker,
    );
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::warn!("shadow log write failed: {e}");
            }
        }
        Err(e) => tracing::warn!("shadow log open failed: {e}"),
    }
}

/// Append one **bold** convergence line to the shadow log: a non-word that
/// converges on exactly one base-dictionary word (the dictionary-driven,
/// first-sighting signal). Distinct `would-suggest(bold)` tag, and includes the
/// candidate count, the non-word flag, and the motor-map affectedness of the
/// edit's keys (even when ~0 today). Applies nothing. Best-effort, like
/// [`append_shadow_log`].
fn append_shadow_bold_log(path: &Path, now: u64, typed: &str, scan: &correction_engine::ConvergenceScan) {
    use std::io::Write;
    // The fire target — the lone candidate, or the frequency-dominant one when
    // several were reachable (so NOT necessarily `candidates.first()`).
    let candidate = scan.target.as_deref().unwrap_or("");
    let line = format!(
        "{now} would-suggest(bold)  {} -> {}  candidates={}  nonword={}  budget={}  affected={:.2}  first-sighting\n",
        typed,
        candidate,
        scan.candidates.len(),
        scan.typed_is_non_word,
        scan.budget,
        scan.affectedness,
    );
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::warn!("shadow bold log write failed: {e}");
            }
        }
        Err(e) => tracing::warn!("shadow bold log open failed: {e}"),
    }
}

/// Read-only (kill-switch OFF): classify every learned pattern and log a
/// summary, so the M3 decision is observable against real accumulating data
/// BEFORE anything is ever suggested. Counts by readiness and lists the patterns
/// that WOULD surface a (confirm-to-accept) suggestion, with their evidence tier
/// and decayed weight. Emitted alongside the 60s funnel dump. No injection —
/// this only surfaces what the classifier *would* decide. No-op on an empty
/// store. (There is no silent path: every correction is a suggestion.)
fn dump_classifications(store: &WordPatternStore, motor_map: &MotorMap) {
    if store.is_empty() {
        return;
    }
    let lexicon = Lexicon::shared();
    let (mut suggest, mut observe) = (0u32, 0u32);
    let mut actionable: Vec<String> = Vec::new();
    for snap in store.snapshots() {
        let ex = correction_engine::classify_explained(
            &snap.typed,
            &snap.target,
            store,
            lexicon,
            motor_map,
        );
        match ex.readiness {
            PatternReadiness::Suggest { tier } => {
                suggest += 1;
                let early = if ex.surfaced_early { " EARLY" } else { "" };
                actionable.push(format!(
                    "{}→{} SUGGEST({:?}) w={:.1} affected={:.2} bar={:.1}/{:.1}{}",
                    snap.typed,
                    snap.target,
                    tier,
                    snap.weight,
                    ex.affectedness,
                    ex.bar_used,
                    ex.base_bar,
                    early
                ));
            }
            PatternReadiness::Observe { .. } => observe += 1,
        }
    }
    // debug!, not info!: `actionable` lists raw typed→target word pairs (privacy).
    // Counts live in FUNNEL_DUMP (text-free), which stays at info.
    tracing::debug!(
        "KILL_SWITCH_DUMP patterns={} suggest={} observe={} actionable={:?}",
        store.len(),
        suggest,
        observe,
        actionable
    );
}

/// Note a record into the proposer and broadcast every per-word
/// change in the batch. A single Kept can ripple: the focal word's
/// tier transitions to Confirmed → enters `is_known` → the
/// proposer's re-eval flips OTHER proposals' proximity verdicts
/// (the self-cleaning hook), each of which may transition tier too.
/// All changes ship as separate `engine://lexicon-proposal` events;
/// a `learned-set-changed` flag drives a fresh
/// `engine://learned-snapshot`.
fn emit_proposal_change<R: Runtime>(
    app: &AppHandle<R>,
    proposer: &mut LexiconProposer,
    record: &correction_engine::LogRecord,
) {
    let update = proposer.note_record(record);
    for (word, proposal) in update.changes {
        let _ = app.emit(
            EVT_LEXICON_PROPOSAL,
            LexiconProposalEvent { word, proposal },
        );
    }
    if update.learned_set_changed {
        let _ = app.emit(EVT_LEARNED_SNAPSHOT, proposer.learned_snapshot());
    }
}

/// Handle one sealed token: register an anchor if it's a Word, run the
/// full L4 pipeline (lexicon → candidates → score → decide), emit the
/// per-stage panel events. Centralised so every emission site (end-of-line,
/// mid-line replay, backspace replay, newline final seal) gets the same
/// side-effects in the same order.
///
/// **Observe-only** (Component 3c-2): the decision is computed and
/// emitted but the engine does NOT inject anything. Injection is 3c-3.
///
/// **Component 4 — decision ledger.** When the token is a Word and the
/// upstream gates pass (see [`should_log`] + [`has_motor_evidence`]), a
/// `Pending` [`LogRecord`] is appended to `ledger` and emitted on
/// [`EVT_LOG_RECORD`]. The `anchor_id` carried on the record is the
/// bridge C5 will use to resolve the outcome; here it must be cleanly
/// available — see the resolution dance below (`try_register` for fresh
/// anchors, `find_tracking_id` for replays).
#[allow(clippy::too_many_arguments)]
fn emit_sealed_token<R: Runtime>(
    app: &AppHandle<R>,
    tok: Token,
    anchors: &mut AnchorTracker,
    lexicon: &Lexicon,
    map: &VolatilityMap,
    ledger: &mut DecisionLedger,
    motor_ledger: &mut MotorLedger,
    line_dwells: &[u32],
    proposer: &mut LexiconProposer,
    funnel: &mut Funnel,
) {
    // C5b acronym fix: route `Acronym` tokens through the same L4
    // pipeline as `Word`. All-caps product names (UPI, BBMP, ONDC) were
    // classified as Acronym by the tokenizer and previously skipped
    // anchoring / decision / ledger / proposer entirely, so they could
    // never be learned. The decide() pipeline is observe-only — even
    // if it produces a `WouldCorrect` arm for an acronym, nothing
    // injects; the panel just sees it.
    if matches!(tok.kind, TokenKind::Word | TokenKind::Acronym) {
        // Anchor registration is the dedupe signal. `try_register`
        // returns `Some(id)` only on a FRESH seal — replays from the
        // backspace and mid-line-insert rebuild loops re-walk the
        // tokenizer over an unchanged `line_buf`, and `try_register`
        // returns `None` for every token whose `(start, end, core)`
        // tuple is already in the tracker.
        //
        // Everything below — casing baseline, LEXICON / CANDIDATES /
        // DECISION emissions, ledger append, proposer note — fires
        // ONCE per real seal. On replay we log and fall through to
        // `EVT_TOKEN` (the panel's tokens-list refresh) at the bottom.
        //
        // Without this gate, every backspace re-appends a fresh
        // `LogRecord` per unknown word in the line (each with a new
        // monotonic record id pointing at the same anchor id), and
        // each one triggers `proposer.note_record` which credits a
        // fresh occasion via `credit_kept_contribution` — inflating
        // the proposer's `occasions` count and poisoning the
        // tier-promotion signal that C5b reads. Observed in the
        // 2026-05-29 trace as N×LEDGER_APPEND + N×EMIT_DECISION
        // bursts on every backspace, with the same N anchor ids
        // repeating across three back-to-back backspace replays.
        let Some(anchor_id) = anchors.try_register(tok.start, tok.end, &tok.core) else {
            // debug!, not info!: tok.core is the raw typed word (privacy).
            tracing::debug!("REPLAY-SKIP-EMIT tok_core={:?}", tok.core);
            let _ = app.emit(EVT_TOKEN, tok);
            return;
        };

        // Funnel (Principle #7): a fresh Word/Acronym seal. Counted here —
        // the single fresh-seal chokepoint — so replays don't inflate it.
        funnel.tokens_sealed += 1;

        // C5b casing baseline. Count only fresh seals so backspace
        // replays don't double-count. Includes known-word seals — the
        // baseline reflects ALL of the user's real typing, which is
        // exactly the signal we need to decide if all-caps is rare
        // for them (rescue active) or routine (rescue suppressed).
        proposer.note_token_seal(matches!(tok.kind, TokenKind::Acronym));
        let _ = app.emit(EVT_CASING_BASELINE, proposer.casing_baseline());

        let row = lexicon_row_for(&tok.core, lexicon);
        let known = row.known;
        let _ = app.emit(EVT_LEXICON, row);

        // Compute the score report once, used for both CANDIDATES (display)
        // and DECISION (policy). Known words produce no candidates and the
        // decision short-circuits to LeaveAlone(Known) — but we still emit a
        // DECISION row so the FEED has one entry per Word token.
        let t_decide_start = Instant::now();
        let candidates = if known {
            Vec::new()
        } else {
            ranked_known_candidates(&tok.core, lexicon, CANDIDATES_TOP_N)
        };
        let report = score_candidates(&tok.core, &candidates, map);
        let outcome = decide(&tok.core, known, &report, ACTIVE_TIER);
        let decide_time_ms = t_decide_start.elapsed().as_secs_f64() * 1000.0;

        // Snapshot what the C5a resolver and C5b proposer will need
        // from the score report BEFORE we move `report.scored` into the
        // panel emission. All sourced from the report (not the decision
        // arm) so a `LeaveAlone(BelowActiveTier)` record still carries
        // the candidate + motor signal. Motor evidence is what C5b
        // reads to decide "clean vs slip" without re-fetching the
        // report.
        let top_candidate_word = report.scored.first().map(|s| s.word.clone());
        let top_motor_for_log = report.scored.first().map(|s| s.motor_evidence);
        let top_score_for_log = report.top_score;
        let top_confidence_for_log = report.top_confidence;

        // CANDIDATES only when there's something to show — known words
        // get no candidate set.
        if !known {
            let _ = app.emit(
                EVT_CANDIDATES,
                CandidatesPayload {
                    word: tok.core.clone(),
                    scored: report.scored,
                    top_score: report.top_score,
                    top_confidence: report.top_confidence,
                    candidates_version: CANDIDATES_VERSION,
                    score_version: SCORE_VERSION,
                },
            );
        }

        // DECISION fires for every Word token (known included — its reason
        // is `known`). The FEED needs one row per word so the builder can
        // see why each token did or didn't fire.
        // debug!, not info!: tok.core is the raw typed word (privacy).
        tracing::debug!("EMIT_DECISION tok_core={:?}", tok.core);
        let _ = app.emit(
            EVT_DECISION,
            DecisionPayload {
                outcome: outcome.clone(),
                active_tier: ACTIVE_TIER,
                decide_time_ms,
                decision_version: DECISION_VERSION,
            },
        );

        // Motor evidence: per-char dwells on this token's span. The privacy
        // guarantee is structural — no motor evidence (pasted / synthetic
        // text, dwell == 0) → no record in either ledger.
        let has_motor = has_motor_evidence(line_dwells, tok.start, tok.end);

        // Component 5c — MOTOR ledger admission. Unlike the decision ledger
        // (unknown words only, for the lexicon proposer), the motor map wants
        // EVERY motor-evidenced word — a cleanly-typed known word is prime
        // motor data. Gate is motor evidence alone (no Known-skip). One lean
        // record per fresh seal; the shared resolver verdicts it and the
        // motor map observes it (in `tick_resolver`'s motor pass).
        if has_motor {
            motor_ledger.append(now_ms(), anchor_id, tok.core.clone());
            funnel.records_admitted += 1;
        }

        // Component 4 — decision ledger (C5b lexicon proposer): UNKNOWN words
        // only, via `should_log`'s Known-skip. This gate stays where it
        // belongs — it's a lexicon concern, not a capture one.
        if should_log(&outcome, has_motor) {
            let ts = now_ms();
            // C5b fix: candidate-INDEPENDENT motor signal computed
            // from the dwell slice on the token's own keystrokes.
            // Populated for every loggable record so the fast
            // lane (no candidate) has a real motor verdict.
            let span_dwells = if tok.end <= line_dwells.len() {
                &line_dwells[tok.start..tok.end]
            } else {
                // Defensive — should be unreachable given C4's
                // motor-evidence gate above.
                &[][..]
            };
            let token_motor = Some(measure_token_motor(span_dwells));
            // debug!, not info!: tok.core is the raw typed word (privacy).
            tracing::debug!("LEDGER_APPEND anchor_id={} core={:?}", anchor_id, tok.core);
            let new_id = ledger.append(
                ts,
                outcome,
                anchor_id,
                ACTIVE_TIER,
                top_candidate_word,
                top_score_for_log,
                top_motor_for_log,
                top_confidence_for_log,
                token_motor,
            );
            // Emit the just-appended record. The ledger owns it and
            // may evict later, but the panel keeps its own copy in
            // its own bounded list.
            if let Some(rec) = ledger.get(new_id).cloned() {
                let _ = app.emit(EVT_LOG_RECORD, rec.clone());
                // Seed the proposer with the Pending record so a
                // subsequent resolver transition has a contribution
                // slot to credit. Pending notes are no-credit but
                // they cache the per-record state.
                emit_proposal_change(app, proposer, &rec);
            }
        }
    }
    let _ = app.emit(EVT_TOKEN, tok);
}

/// Build the lexicon-row payload for a Word token. Pure function — pulled
/// out so the v2 "membership vs frequency" invariant can be pinned by
/// tests. **Membership MUST come from [`Lexicon::is_known`], not from
/// `frequency > 0`** — the Norvig freq table contains web typos with real
/// counts that are not real words, and we must never let those pass as
/// known. This was a real regression (engine reported `teh known=yes,
/// freq 1.7M` even after the lexicon module was correctly split).
fn lexicon_row_for(core: &str, lexicon: &Lexicon) -> LexiconPayload {
    LexiconPayload {
        word: core.to_string(),
        known: lexicon.is_known(core),
        frequency: lexicon.frequency(core),
        lexicon_version: LEXICON_VERSION,
    }
}

/// Per-Word-token lexicon lookup. Emitted alongside `EVT_TOKEN` so the
/// debug panel can show known/freq next to the same words it lists in
/// TOKENS, without changing the L4 `Token` contract itself.
#[derive(Serialize, Clone)]
struct LexiconPayload {
    /// The token's core, case preserved. Lookup itself is case-insensitive.
    word: String,
    known: bool,
    frequency: u64,
    lexicon_version: u32,
}

/// Per-unknown-Word-token candidate set with confidence scoring
/// Per-unknown-Word-token candidate set with confidence scoring
/// (Components 3b + 3c-1 + 3c-3). Empty `scored` is emitted when the word
/// has no known edit-1 neighbour — the panel renders that as "(no known
/// candidates within edit-1)", a real outcome the brief wants visible.
///
/// `top_confidence` is the per-candidate label (`Low` / `Medium` / `High`)
/// — **the panel's badge**. The active-mode gate that drives the actual
/// decision lives in [`DecisionPayload`]. Modes and corrections use
/// separate vocabularies (3c-3): a candidate has confidence, a mode
/// chooses what confidence to act on.
#[derive(Serialize, Clone)]
struct CandidatesPayload {
    word: String,
    scored: Vec<ScoredCandidate>,
    top_score: Option<f64>,
    top_confidence: Option<Confidence>,
    candidates_version: u32,
    score_version: u32,
}

/// Max hard-restart attempts allowed within [`RESPAWN_WINDOW`]. Beyond
/// this the engine gives up auto-respawning and goes terminal
/// `Stopped`. The user can still recover via manual "Restart capture",
/// which resets this counter (per the design contract — they're
/// signaling fresh start).
const MAX_RESPAWN_ATTEMPTS: usize = 3;
/// Sliding window for [`MAX_RESPAWN_ATTEMPTS`]. Old attempts age out so
/// a sidecar that crashed once last hour but is fine now doesn't
/// count toward the cap.
const RESPAWN_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
/// Heartbeat ≥ this many ms old → Unhealthy. Sidecar emits every 2s,
/// so 6s = 3 missed heartbeats — meaningful staleness without
/// false-flagging brief stalls.
const HEARTBEAT_STALE_MS: u128 = 6_000;
/// Heartbeat ≥ this many ms old → Stopped + auto-respawn. 8s = 4
/// missed heartbeats (was 15s / 7): respawn sooner so a genuine stop
/// surfaces faster — paired with [`NOT_ACTIVE_DEBOUNCE_MS`] below (v1.5
/// health-latency cut). Tradeoff: slightly eager respawns on a long
/// stall that might have self-cleared, accepted because a respawn is
/// cheap and idempotent (same spawn shape as boot — see `spawn_sidecar`).
const HEARTBEAT_STOPPED_MS: u128 = 8_000;
/// How often the watchdog re-emits the current health state even
/// when nothing has changed — so a panel that just mounted converges
/// to truth without waiting for a transition.
const HEALTH_REPEAT_TICKS: u64 = 5;
/// How long capture must stay continuously non-`Live` before the menu-bar UI
/// declares it not-active (drives [`EVT_CAPTURE_UI`]). Measured from the moment
/// health *left* `Live` (~[`HEARTBEAT_STALE_MS`], 6s into an outage). Sized to
/// clear the self-heal window: a disabled tap re-arms in the sidecar within ~2s,
/// and a dead sidecar is auto-respawned at [`HEARTBEAT_STOPPED_MS`] (8s) with its
/// first fresh heartbeat ~2s later — so recovery lands ~10s into the outage. This
/// 9s debounce alarms at ~15s (leave-Live + 9s), keeping a ~5s margin past the
/// self-heal window so a recovering blip never strobes the icon, while a genuine
/// stop now surfaces ~7s sooner than the old 16s (v1.5 health-latency cut).
/// NOTE (v1.5): watch dogfooding — if a respawn ever briefly flashes not-active,
/// the respawn heartbeat is landing slower than ~2s and this must grow back
/// toward the [`HEARTBEAT_STOPPED_MS`] + heartbeat-interval sum.
const NOT_ACTIVE_DEBOUNCE_MS: u128 = 9_000;

/// Spawn (or respawn) the Swift sidecar via `app.shell().sidecar()`. Factored
/// out so commit O's hard-restart path uses the SAME spawn shape as the initial
/// boot — divergence here would be a fertile source of "works the first time,
/// then dies on restart" bugs.
///
/// We deliberately DO NOT set `TYPEASSIST_AX_PROMPT=1` here. Production must keep
/// the system Accessibility modal suppressed — the onboarding / Reconnect UI
/// owns that conversation and opens the Settings pane directly (see
/// `Accessibility.isTrusted(prompt:)` and `main.swift`). Forcing the prompt on
/// meant every respawn (the onboarding 3s `restart_capture` poll, the watchdog
/// auto-respawn) re-ran `AXIsProcessTrustedWithOptions(prompt: true)` and popped
/// a fresh modal — turning a single failed trust read into an endless prompt
/// loop. The `TYPEASSIST_AX_PROMPT` env still exists for the headless
/// walking-skeleton (no UI to drive the grant); the app just never sets it.
fn spawn_sidecar<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<
    (
        tokio::sync::mpsc::Receiver<CommandEvent>,
        tauri_plugin_shell::process::CommandChild,
    ),
    Box<dyn std::error::Error>,
> {
    let mut cmd = app.shell().sidecar("typeassist-input-macos")?;
    // Phase 0 / M3 debug: propagate the AX-geometry probe flag to the
    // sidecar so it runs the feasibility probe under the app's *working*
    // Accessibility grant (a Terminal launch of the same binary hits
    // kAXErrorCannotComplete (-25204) — the grant is attributed to the
    // launch context, not the binary). Launch with `TYPEASSIST_AX_PROBE=1
    // just dev` and watch the sidecar's stderr in the dev console.
    if std::env::var("TYPEASSIST_AX_PROBE").as_deref() == Ok("1") {
        cmd = cmd.env("TYPEASSIST_AX_PROBE", "1");
    }
    Ok(cmd.spawn()?)
}

/// Try to respawn the sidecar, replacing the receiver and child handles
/// in place. Returns `true` on success.
///
/// Caps total attempts via the sliding window — once
/// [`MAX_RESPAWN_ATTEMPTS`] are recorded within [`RESPAWN_WINDOW`], the
/// next call refuses ("we've tried enough"). Manual
/// [`EngineControl::RestartCapture`] clears `attempts` before calling
/// this, so the user always gets at least one fresh chance.
///
/// On failure (either cap hit OR spawn errored), the existing handles
/// are left untouched — the caller should mark health Stopped and
/// continue the loop so the control channel stays live.
fn try_hard_restart<R: Runtime>(
    app: &AppHandle<R>,
    rx: &mut tokio::sync::mpsc::Receiver<CommandEvent>,
    child: &mut tauri_plugin_shell::process::CommandChild,
    attempts: &mut Vec<Instant>,
) -> bool {
    let now = Instant::now();
    attempts.retain(|t| now.duration_since(*t) < RESPAWN_WINDOW);
    if attempts.len() >= MAX_RESPAWN_ATTEMPTS {
        tracing::error!(
            "sidecar respawn refused — {}/{} attempts in last {}s",
            attempts.len(),
            MAX_RESPAWN_ATTEMPTS,
            RESPAWN_WINDOW.as_secs()
        );
        return false;
    }
    attempts.push(now);
    match spawn_sidecar(app) {
        Ok((new_rx, new_child)) => {
            *rx = new_rx;
            *child = new_child;
            tracing::info!(
                "sidecar respawned ({}/{} attempts in window)",
                attempts.len(),
                MAX_RESPAWN_ATTEMPTS
            );
            true
        }
        Err(e) => {
            tracing::error!("sidecar respawn failed: {e}");
            false
        }
    }
}

/// Transition `current` to `new` if they differ, emitting the
/// [`EVT_CAPTURE_HEALTH`] event on transition. Returns `true` iff a
/// transition fired. Side-effect free apart from the emit + the
/// mutation of `current`.
fn transition_capture_health<R: Runtime>(
    app: &AppHandle<R>,
    current: &mut CaptureHealth,
    new: CaptureHealth,
) -> bool {
    if *current != new {
        *current = new;
        let _ = app.emit(EVT_CAPTURE_HEALTH, CaptureHealthEvent { state: new });
        tracing::info!("capture health -> {:?}", new);
        true
    } else {
        false
    }
}

pub fn spawn<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<EngineControlSender, Box<dyn std::error::Error>> {
    // `sidecar_child` owns the parent-side write-end of the sidecar's
    // stdin pipe. It is **moved into the async task below** and dropped
    // when that task ends — see the load-bearing comment at the bottom
    // of the closure for the lifetime contract.
    let (mut rx, mut sidecar_child) = spawn_sidecar(app)?;
    let app_handle = app.clone();

    // Control channel: Tauri commands → engine task. Unbounded so the
    // UI thread is never blocked. The returned sender is `manage`d by
    // Tauri and cloned per command invocation.
    let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel::<EngineControl>();

    tauri::async_runtime::spawn(async move {
        // L2 lives here for the life of the engine. Single owner, single async
        // task — no sync needed.
        let mut model = BehaviouralModel::new();
        // L4 streaming tokenizer (Component 1 of the Observing brief) —
        // the SINGLE source of word-boundary truth. The lexicon / candidate
        // / score / decision pipeline all consume sealed Word tokens from
        // here; no parallel word-buffer exists. We do keep a tiny char
        // buffer for the current line so backspaces can rebuild cheaply
        // (the tokenizer itself is forward-only).
        let mut tokenizer = Tokenizer::new();
        let mut line_buf: Vec<char> = Vec::new();
        // Component 4 motor-evidence proxy. Parallel to `line_buf`: one
        // entry per char carrying that key's `dwell_ms`. Pasted /
        // auto-filled / synthetic chars arrive with no press/release
        // timing (dwell == 0) and fail [`has_motor_evidence`] by
        // construction, so the decision ledger never captures them.
        // Stays in lock-step with `line_buf` across inserts, backspaces,
        // and line resets; replays read it as the source of truth.
        let mut line_dwells: Vec<u32> = Vec::new();
        // L4 Observing brief, Component 2: span anchor tracker. The tracker
        // is engine-owned (not panel-side) so its edit-delta logic is pure
        // Rust and the panel just renders snapshots. Each sealed Word token
        // is offered to `try_register` (it dedupes replay).
        let mut anchors = AnchorTracker::new();
        // L4 Observing brief, Component 4: bounded in-memory decision
        // ledger. Owns Pending records keyed by anchor id; outcome
        // resolution is C5. Lives for the life of the engine; no disk
        // writes by design.
        let mut ledger = DecisionLedger::new();
        // L4 Observing brief, Component 5a: outcome resolver. Watches
        // each anchor's state + content, debounces, and transitions
        // ledger records through their final outcome. Ticked once per
        // edit alongside the anchor snapshot emit — the user typing
        // the next word's first letter is also what surfaces the
        // PREVIOUS word's resolution. No background timer in 5a:
        // resolutions land on the next keystroke after the debounce
        // window. Idle gaps with no further input leave the record
        // Pending until the next key arrives — fine for the debug
        // panel; revisit if real users notice.
        let mut resolver = OutcomeResolver::new();
        // C5c motor resolver — a SECOND resolver instance with its own
        // stability cache, resolving the motor ledger (every word). Kept
        // separate from the decision resolver so the two passes never share
        // per-anchor state. See `tick_resolver`.
        let mut motor_resolver = OutcomeResolver::new();
        // L4 Observing brief, Component 5b Phase 1: lexicon proposer.
        // Watches Kept outcomes from the resolver, classifies each
        // word's promotion lane and motor verdict, and emits per-word
        // proposals to the debug panel. **Observe-only** this phase —
        // does NOT touch `lexicon.is_known`. Reacts to revisable
        // resolver transitions: Kept-then-Corrected retracts the
        // contribution so a kept-then-corrected word never stays
        // promoted.
        let mut proposer = LexiconProposer::new();
        // L4 Observing brief, Component 5c: motor map. Learns this user's
        // per-key slip distribution from resolved Kept / CorrectedToOther
        // outcomes (fed in `tick_resolver`). Loaded from disk on startup;
        // saved every 100 observations + on graceful shutdown; a DAILY dated
        // snapshot is written by the watchdog, plus one at startup before any
        // writes (Principle #6 — every meaningful state is preserved).
        // **Observe-and-store only** — the L2→L3 kill-switch stays OFF; nothing
        // here feeds a correction back. Paths resolve under `~/.typeassist`;
        // `None` (HOME unset) degrades to an in-memory map with no durability.
        let motor_map_path = motor_map_path();
        let snapshots_dir = snapshots_dir();
        let mut motor_map = match motor_map_path.as_deref() {
            Some(path) if path.exists() => match MotorMap::load_from(path) {
                Ok(map) => {
                    tracing::info!(
                        "MOTOR_MAP_LOADED obs={} keys={} path={:?}",
                        map.total_observations(),
                        map.len(),
                        path
                    );
                    map
                }
                Err(e) => {
                    // Don't clobber a possibly-recoverable file by silently
                    // starting fresh — surface it and keep going in memory.
                    tracing::warn!("motor map load failed ({e}); starting fresh in memory");
                    MotorMap::new()
                }
            },
            _ => MotorMap::new(),
        };
        // C5e local vocabulary tally — counts is_known words the user types
        // correctly and leaves in place, for a FUTURE personal-frequency
        // candidate-ranking experiment (observe-only; nothing reads it back
        // into correction yet). Its own file beside the motor map; load-fail
        // keeps going in memory rather than clobbering a recoverable file.
        // Privacy-gated to is_known words, so the file can never hold a name,
        // password, ID, or any out-of-dictionary string.
        let word_freq_path = word_freq_path();
        let word_freq_snapshots_dir = word_freq_snapshots_dir();
        let mut word_freq = match word_freq_path.as_deref() {
            Some(path) if path.exists() => match WordFreq::load_from(path) {
                Ok(wf) => {
                    tracing::info!(
                        "WORD_FREQ_LOADED words={} tokens={:.0} path={:?}",
                        wf.len(),
                        wf.total_count(),
                        path
                    );
                    wf
                }
                Err(e) => {
                    tracing::warn!("word-freq tally load failed ({e}); starting fresh in memory");
                    WordFreq::new()
                }
            },
            _ => WordFreq::new(),
        };
        let mut last_word_freq_save_ms: u64 = 0;
        // C5e: while a warm-up / Practice round is on screen, typed text is
        // app-generated, so the word-freq tally skips it (the motor map still
        // observes — that's the point of practice). Flipped by
        // `EngineControl::SetPromptedCaptureActive` from the panel's show/hide.
        let mut prompted_capture_active = false;
        // C5d word-pattern store — learns typed→target word corrections,
        // observe-only (kill-switch OFF). Its own file beside the motor map;
        // load-fail keeps going in memory rather than clobbering a recoverable
        // file. No dated snapshot (the live file is the durability this slice;
        // snapshots/ is motor-map-shaped — see `word_patterns_path`).
        let word_patterns_path = word_patterns_path();
        let mut word_patterns = match word_patterns_path.as_deref() {
            Some(path) if path.exists() => match WordPatternStore::load_from(path) {
                Ok(store) => {
                    tracing::info!(
                        "WORD_PATTERNS_LOADED patterns={} obs={} path={:?}",
                        store.len(),
                        store.total_observations(),
                        path
                    );
                    store
                }
                Err(e) => {
                    tracing::warn!(
                        "word-pattern store load failed ({e}); starting fresh in memory"
                    );
                    WordPatternStore::new()
                }
            },
            _ => WordPatternStore::new(),
        };
        let mut last_word_patterns_save_ms: u64 = 0;
        // Guesser accuracy scoreboard (M3 Phase 1, observe-only) — measures how
        // often the guesser's top guess matches the user's actual fix. Its own
        // file beside the word-pattern store; load-fail keeps going in memory
        // rather than clobbering a recoverable file. Never read back into
        // correction — measurement only.
        let guess_accuracy_path = guess_accuracy_path();
        let mut guess_ledger = match guess_accuracy_path.as_deref() {
            Some(path) if path.exists() => match GuessLedger::load_from(path) {
                Ok(ledger) => {
                    tracing::info!(
                        "GUESS_LEDGER_LOADED patterns={} tries={} hits={} path={:?}",
                        ledger.len(),
                        ledger.overall().tries,
                        ledger.overall().hits,
                        path
                    );
                    ledger
                }
                Err(e) => {
                    tracing::warn!("guess-accuracy ledger load failed ({e}); starting fresh");
                    GuessLedger::new()
                }
            },
            _ => GuessLedger::new(),
        };
        let mut last_guess_ledger_save_ms: u64 = 0;
        // M3 correction Step 1 — the manual correction allow-list + master
        // gate. The engine task is its sole writer; loaded once here, mutated
        // in-memory on EngineControl + Escape-undo, flushed atomically. A
        // load failure keeps going with the shipped-dark default (gate off, no
        // patterns) rather than clobbering the user's curated file — but, since
        // the default would silently disable corrections, it is logged loud.
        let allow_list_path = allow_list_path();
        let mut allow_list = match allow_list_path.as_deref() {
            Some(path) => match crate::allow_list::AllowList::load_from(path) {
                Ok(al) => {
                    tracing::info!(
                        "ALLOW_LIST_LOADED enabled={} patterns={} path={:?}",
                        al.correction_enabled,
                        al.patterns.len(),
                        path
                    );
                    al
                }
                Err(e) => {
                    tracing::warn!(
                        "allow-list load failed ({e}); corrections OFF this session (shipped-dark default)"
                    );
                    crate::allow_list::AllowList::new()
                }
            },
            None => crate::allow_list::AllowList::new(),
        };
        // C5c motor ledger — the motor map's own record stream: EVERY
        // motor-evidenced sealed word (known included), lean records the
        // shared resolver verdicts. Decouples the motor map from the
        // decision ledger's lexicon Known-skip (the sealed→verdict cliff).
        // In-memory only, like the decision ledger.
        let mut motor_ledger = MotorLedger::new();
        // Watch Dog Not Attack Dog (M3 autocorrect coexistence): behavioural
        // sense of an active competing corrector. Global (decision ii), so no
        // app identity is ever held. Fed by host-redundant footprints in
        // `tick_resolver`; read by the fire gate; reset on focus change; decayed
        // by the watchdog.
        let mut competitor_sense = CompetitorSense::default();
        // TF-08 host-scoped suppression: the latest injection dead-zone verdict
        // from L1 (`InputEvent::InjectionZone`). `true` ⇒ the focused field is a
        // place a synthetic correction can't land (Safari web/contenteditable on
        // Intel — phantom AX caret), so the fire gate goes WATCH-ONLY there:
        // keep observing/learning, withhold the bubble (no cue). Level signal,
        // updated on focus change; default `false` so nothing changes for native
        // fields, Chrome, or Apple Silicon.
        let mut injection_dead_zone = false;
        // Daily-snapshot bookkeeping (Principle #6): the calendar date whose
        // snapshot we've already handled this run, seeded from the newest dated
        // file on disk so the first event of a *new* calendar day is detected
        // correctly across restarts.
        let mut last_snapshot_date: Option<(i64, u32, u32)> = snapshots_dir
            .as_deref()
            .and_then(most_recent_snapshot_ms)
            .map(ymd_from_epoch_ms);
        // **Startup snapshot (Principle #6).** Preserve the as-loaded state
        // BEFORE the loop mutates the live map, so every launch leaves a
        // checkpoint — even a session that crashes before its first flush.
        // Dated files are NEVER overwritten or deleted by the engine: write
        // today's only if absent, so snapshots accumulate as durable history.
        if let Some(dir) = snapshots_dir.as_deref() {
            let now = now_ms();
            let today = dir.join(format!("{}.json", snapshot_date(now)));
            if !today.exists() {
                let _ = std::fs::create_dir_all(dir);
                match motor_map.write_snapshot(&today) {
                    Ok(()) => tracing::info!("MOTOR_SNAPSHOT_STARTUP path={today:?}"),
                    Err(e) => tracing::warn!("startup motor snapshot failed: {e}"),
                }
            }
            // Mark today handled (whether we wrote or it already existed), so
            // the watchdog only acts when the calendar day rolls over.
            last_snapshot_date = Some(ymd_from_epoch_ms(now));
        }
        // C5e word-tally daily snapshot bookkeeping (Principle #6) — its own
        // directory + date guard, mirroring the motor map. Startup snapshot
        // preserves the as-loaded tally before the loop mutates it.
        let mut last_word_freq_snapshot_date: Option<(i64, u32, u32)> = word_freq_snapshots_dir
            .as_deref()
            .and_then(most_recent_snapshot_ms)
            .map(ymd_from_epoch_ms);
        if let Some(dir) = word_freq_snapshots_dir.as_deref() {
            let now = now_ms();
            let today = dir.join(format!("{}.json", snapshot_date(now)));
            if !today.exists() {
                let _ = std::fs::create_dir_all(dir);
                match word_freq.write_snapshot(&today) {
                    Ok(()) => tracing::info!("WORD_FREQ_SNAPSHOT_STARTUP path={today:?}"),
                    Err(e) => tracing::warn!("startup word-freq snapshot failed: {e}"),
                }
            }
            last_word_freq_snapshot_date = Some(ymd_from_epoch_ms(now));
        }
        // Last time the live motor map was flushed to disk. Drives the
        // periodic flush (see `flush_motor_map`); 0 means "never this
        // session" so the first dirty watchdog tick flushes promptly.
        let mut last_motor_save_ms: u64 = 0;
        // Capture-integrity funnel (Principle #7). Per-session boundary
        // counters; dumped on the Cmd+Shift+F chord and every 60s.
        let mut funnel = Funnel::new(now_ms());
        // Build marker (Principle #7 observability): one line at engine startup so
        // the dev Terminal proves WHICH build is running — the bubble suggest path
        // (fire from the classifier, accept via ShiftTap), not a stale engine.
        tracing::info!(
            "ENGINE_BUILD feature=correction_bubble fire_lane=classifier \
             suggest_event=corrections://suggested accept=shift_tap undo=revert_only"
        );
        // M3 correction bubble: the last APPLIED fix, retained so a single Escape
        // within `UNDO_WINDOW_MS` reverts it (revert only — no teach-stop).
        // Disarmed on revert, on any other keystroke (implicit commit), and on
        // window timeout (watchdog).
        let mut last_correction: Option<LastCorrection> = None;
        // M3 correction bubble: the correction currently SUGGESTED (bubble up),
        // armed at seal and accepted by an isolated Shift tap. Cleared by any
        // keystroke past the word, a caret move, the ~5s timeout, or a newer
        // suggestion — so an accept only ever injects against the live caret.
        let mut pending_suggestion: Option<PendingSuggestion> = None;
        // UK English (v0.3.0): the spelling variant a suggestion should use for a
        // variant word, set from the OS locale by L5 via
        // `EngineControl::SetSpellingVariant`. Defaults to American until L5
        // reports — safe, since it only rewrites British-spelled targets and
        // leaves everything else as typed. Never persisted (Principle #8).
        let mut spelling_variant = SpellingVariant::default();
        // Count of injected events (backspaces + replacement chars) we still
        // expect to see echoed back through the L1 tap. The tap re-captures our
        // own injection (`.cgSessionEventTap` sees posted events), so each
        // correction's keystrokes stream back in; we skip exactly this many so
        // they never reach the pipeline (no re-learn, no buffer desync, and —
        // critically — they don't disarm the undo before the user's Escape).
        // Counting-based on purpose: an `eventSourceUserData` tag did NOT
        // survive the post→tap round-trip in practice. The echo arrives
        // back-to-back (~1 ms apart), far faster than a human, so it always
        // drains before the next real key.
        let mut pending_echo: u32 = 0;
        // Progress Statistics: the current day's live word/slip tally, seeded
        // from disk so a mid-day restart resumes today's counts rather than
        // resetting them. The watchdog upserts + rolls it (see `tick_progress`).
        let progress_path = progress_snapshots_path();
        let mut tally = load_daily_tally(progress_path.as_deref(), now_ms());
        tracing::info!(
            "PROGRESS_LOADED date={} words={} slips={} coord={} precis={}",
            tally.date_str(),
            tally.words,
            tally.slips,
            tally.coord,
            tally.precis
        );
        let mut last_progress_save_ms: u64 = 0;
        // L4 lexicon (Component 3a). Process-wide singleton — first touch
        // parses the ~50k-entry bundled list; subsequent reads are HashMap
        // lookups. Read-only this slice: scoring/correction come later.
        let lexicon: &'static Lexicon = Lexicon::shared();
        // Caret position in `line_buf` (char index). Anchor edit deltas
        // (p, d, i) are computed from this. Updated on inserts, backspaces,
        // and Left / Right / Home / End nav keys; mouse-click moves and
        // paste are intentionally out of scope (Component 5 AX backstop).
        let mut caret: usize = 0;
        // **Hard pause** — when true, Key/Backspace events from the
        // sidecar are dropped before model.ingest and never reach
        // tokenizer / decision / ledger / proposer. Lifecycle events
        // (Ready/PermissionRequired/Shutdown) flow through unchanged.
        // Toggled by [`EngineControl::SetInputPaused`]; transition to
        // true flushes line state (line_buf / line_dwells / caret /
        // anchors / tokenizer) so the engine wakes on a clean
        // boundary when input resumes.
        let mut input_paused: bool = false;
        // **C5 capture-health** — last heartbeat arrival from the
        // sidecar + the tap_enabled flag it carried. The watchdog in
        // commit O will read these on a timer to detect staleness;
        // commit M only emits transitions in response to incoming
        // heartbeats, so `Unknown` is the initial state (no heartbeat
        // seen yet) and only an explicit Heartbeat event can change it.
        // **Capture-health watchdog state.** `last_heartbeat_at` is
        // stamped each time the sidecar's Heartbeat event arrives;
        // the periodic watchdog tick reads it to derive staleness.
        // `respawn_attempts` is a sliding window of when we tried to
        // hard-restart the sidecar — capped at MAX_RESPAWN_ATTEMPTS
        // within RESPAWN_WINDOW so a crashing sidecar can't burn CPU
        // forever. Manual RestartCapture clears the window (the user
        // explicitly asked for a fresh start).
        let mut last_heartbeat_at: Option<Instant> = None;
        let mut current_capture_health: CaptureHealth = CaptureHealth::Unknown;
        let mut respawn_attempts: Vec<Instant> = Vec::new();
        // Last time a keystroke (Key/Backspace) arrived — drives the idle
        // line-buffer reset (desync recovery). See [`LINE_IDLE_RESET_MS`].
        let mut last_text_input_at: Instant = Instant::now();

        // **Capture-health watchdog** — ticks every 1s, reads
        // `last_heartbeat_at` to derive freshness, and drives
        // `current_capture_health` to Unhealthy / Stopped on
        // staleness. Also auto-attempts hard restart when Stopped
        // (within the attempt window) and re-emits health state
        // every ~5s so panel reloads converge without waiting for
        // the next transition.
        let mut watchdog = tokio::time::interval(std::time::Duration::from_secs(1));
        watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut watchdog_ticks: u64 = 0;

        // Menu-bar icon state, driven by the single capture-stopped signal.
        // `last_tray_icon` is the last `not_active` value actually pushed to the
        // OS (`None` until the first set), so an unchanged state is a no-op.
        let mut last_tray_icon: Option<bool> = None;

        // Debounced menu-bar "capture active?" state (drives EVT_CAPTURE_UI).
        // `non_live_since` = when health last LEFT Live (None while Live), so the
        // debounce is measured from the start of the outage; `ui_not_active` is
        // the settled flag the UI renders. `permission_ok` tracks the sidecar's
        // Accessibility grant — flipped false on a `PermissionRequired`, true on
        // any heartbeat (a heartbeat means the sidecar built a tap → grant in
        // effect) — and chooses the recovery action. `last_ui_emit` dedupes the
        // EVT_CAPTURE_UI emit.
        let mut non_live_since: Option<Instant> = None;
        // `ever_live` latches true on the first Live heartbeat. Until then the
        // menu-bar state is `NotStarted`, never `Active` — the tray must derive
        // "active" from an OBSERVED probe, not assume it (Principle #7).
        let mut ever_live = false;
        let mut permission_ok = true;
        // (a) QA-20: the sidecar's per-heartbeat Accessibility grant (from
        // `PermissionStatus`), tracked SEPARATELY from `permission_ok`. Revoking
        // Accessibility at runtime does NOT stop the already-built tap (it rides
        // on Input Monitoring), so heartbeats keep arriving and `permission_ok`
        // stays true — but corrections can no longer inject. This authoritative
        // per-grant read is what surfaces that degraded state to the tray.
        let mut accessibility_ok = true;
        let mut last_ui_emit: Option<CaptureUiEvent> = None;

        // The receive loop selects between the sidecar event stream,
        // the control channel, and the capture-health watchdog so:
        //   - keystroke events run fastest (biased ordering),
        //   - Tauri commands are processed without waiting for the
        //     next keystroke,
        //   - the watchdog detects silent capture death even when
        //     neither stream is active.
        // `biased` keeps sidecar events ahead of the other arms when
        // multiple are ready — input ordering matters, control and
        // watchdog ticks don't.
        'engine_loop: loop {
            tokio::select! {
                biased;
                event = rx.recv() => {
                    let Some(event) = event else {
                        // rx is closed but no Terminated arm fired —
                        // sidecar dropped its stdout / event stream
                        // without a clean signal. Treat as termination
                        // and try respawn through the same path.
                        tracing::warn!(
                            "sidecar event stream closed — attempting hard restart"
                        );
                        transition_capture_health(
                            &app_handle,
                            &mut current_capture_health,
                            CaptureHealth::Stopped,
                        );
                        if try_hard_restart(
                            &app_handle,
                            &mut rx,
                            &mut sidecar_child,
                            &mut respawn_attempts,
                        ) {
                            last_heartbeat_at = Some(Instant::now());
                        } else {
                            // Respawn refused or failed — rx is still
                            // the old closed receiver. Sleep before
                            // the next loop iteration so we don't
                            // tight-loop on `rx.recv() -> None`. The
                            // attempt window keeps sliding; once 30s
                            // pass since the oldest attempt, the cap
                            // releases and we can try again. The
                            // control channel stays live throughout
                            // so manual Restart capture works.
                            tokio::time::sleep(
                                std::time::Duration::from_secs(5),
                            ).await;
                        }
                        continue;
                    };
                    match event {
                CommandEvent::Stdout(bytes) => {
                    // Plugin already splits on newline; one event = one line.
                    let line = match std::str::from_utf8(&bytes) {
                        Ok(s) => s.trim(),
                        Err(_) => continue,
                    };
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(parsed) = serde_json::from_str::<InputEvent>(line) else {
                        continue;
                    };

                    // FIRE_TIMING t0 — wall-clock instant the engine received this
                    // keystroke line from the L1 sidecar. `now_ms()` is UNIX-epoch
                    // ms, the SAME clock as the webview's `Date.now()`, so the t0..t4
                    // stamps logged across Rust + JS are directly comparable. Used
                    // only by the suggestion fire site below; harmless on other
                    // events. (Measurement only — does not affect firing.)
                    let t_recv = now_ms();

                    // Hard pause: drop keystrokes at the engine boundary.
                    // Lifecycle events (Ready/PermissionRequired/Shutdown)
                    // still flow — those aren't input, they're the sidecar
                    // telling us what state it's in. Continues the outer
                    // 'engine_loop so the next sidecar event (or control
                    // command) is awaited.
                    // M3 correction Step 1 — drop our own injection echo. The
                    // tap re-captures the keystrokes we posted to apply a
                    // correction (or its undo); skip exactly the number we
                    // injected so they never enter the pipeline. Echo is NOT
                    // user input, so this is BEFORE the funnel counts it — and
                    // before the undo block, so the echo can't disarm a pending
                    // Escape undo. See `pending_echo`.
                    if pending_echo > 0
                        && matches!(
                            parsed,
                            InputEvent::Key { .. } | InputEvent::Backspace { .. }
                        )
                    {
                        pending_echo -= 1;
                        continue;
                    }

                    // M3 bubble gating: a pending suggestion stays ACCEPTABLE while
                    // the user types ON (anchored accept), but is dropped the moment
                    // they do anything that isn't a clean forward keystroke. The
                    // per-event keep/dismiss decision lives in the Key / Backspace
                    // arms (which know forward-vs-edit-vs-nav); here we only handle
                    // the simplest case — a Backspace is always an edit, so dismiss.
                    // (Bias hard toward dismiss: a missed accept is fine, a
                    // wrong-place edit is not.)

                    // Funnel L1 boundary: count every raw typing event from
                    // the sidecar BEFORE any filtering (pause / modifier /
                    // non-text), so `received` is the true denominator.
                    if matches!(
                        parsed,
                        InputEvent::Key { .. } | InputEvent::Backspace { .. }
                    ) {
                        funnel.keystrokes_received += 1;
                        // Mark activity for the idle line-buffer reset: any
                        // keystroke means the user is present, so the buffer
                        // is not stale yet.
                        last_text_input_at = Instant::now();
                    }
                    // QA-15 space-drop reconciliation (observe-only): a space the
                    // engine RECEIVED here is matched against the tap's keyDown
                    // count (the `SpaceObserved` arm below). Counted pre-filter so
                    // it reconciles at the same L1 boundary as `received`.
                    if let InputEvent::Key { key, .. } = &parsed {
                        if key == " " {
                            funnel.note_space_accepted();
                        }
                    }

                    if input_paused
                        && matches!(
                            parsed,
                            InputEvent::Key { .. } | InputEvent::Backspace { .. }
                        )
                    {
                        tracing::info!("PAUSED-DROP kind={:?} input_paused={}", parsed, input_paused);
                        continue;
                    }

                    // **C5c Layer A** — probe the motor baseline for this
                    // keystroke's anomaly BEFORE ingest mutates the
                    // baseline (otherwise the score reflects "the user
                    // including this keystroke," not "the user's prior
                    // model"). Lifecycle/Backspace events have no key
                    // to score → no event emitted.
                    let motor_keystroke_event: Option<MotorKeystrokeEvent> =
                        if let InputEvent::Key {
                            key,
                            timestamp_ms,
                            dwell_ms,
                            ..
                        } = &parsed
                        {
                            let (hand, finger) = match volatility_map::finger_for(key) {
                                Some((h, f)) => (Some(h), Some(f)),
                                None => (None, None),
                            };
                            let anomaly = model.motor_baseline.keystroke_anomaly(
                                key,
                                *timestamp_ms,
                                *dwell_ms,
                            );
                            Some(MotorKeystrokeEvent {
                                key: key.clone(),
                                timestamp_ms: *timestamp_ms,
                                dwell_ms: *dwell_ms,
                                hand,
                                finger,
                                anomaly,
                            })
                        } else {
                            None
                        };

                    // Observe-only L2 dispatch. Only Key/Backspace flow into
                    // the model — sidecar lifecycle events (Ready/Shutdown/…)
                    // aren't keystrokes. Measured on its own so the debug view
                    // can show what L2 is costing per keystroke.
                    let ingest_latency_ms = if matches!(
                        parsed,
                        InputEvent::Key { .. } | InputEvent::Backspace { .. }
                    ) {
                        let t_ingest = Instant::now();
                        model.ingest(&parsed);
                        let lat = t_ingest.elapsed().as_secs_f64() * 1000.0;
                        // Drain any slips L2 confirmed during this ingest
                        // and broadcast them — one event per slip so the
                        // panel can mark each in the feed in order.
                        for slip in model.take_new_slips() {
                            let _ = app_handle.emit(EVT_SLIP, slip);
                        }
                        // Broadcast the new model state so the debug "Model
                        // state" tables update live as the user types.
                        let _ = app_handle.emit(EVT_MODEL_SNAPSHOT, model.snapshot());
                        // C5c Layer A — separate, finer-grained events
                        // for the motor section. Panels that don't care
                        // about motor data can ignore these without
                        // re-parsing the full model snapshot.
                        if let Some(ev) = motor_keystroke_event {
                            let _ = app_handle.emit(EVT_MOTOR_KEYSTROKE, ev);
                        }
                        let _ = app_handle.emit(
                            EVT_MOTOR_BASELINE,
                            model.motor_baseline.snapshot(),
                        );
                        lat
                    } else {
                        0.0
                    };

                    match parsed {
                        InputEvent::Ready => {
                            tracing::info!("sidecar ready — engine listening");
                        }
                        InputEvent::AutorepeatDropped => {
                            // L1 dropped a non-backspace auto-repeat keystroke.
                            // Count it so capture loss is visible (Principle #7);
                            // it carries no content and feeds no pipeline stage.
                            funnel.autorepeat_dropped += 1;
                        }
                        InputEvent::SpaceObserved { total } => {
                            // QA-15 space-drop detector (Step 1, OBSERVE-ONLY).
                            // The tap saw `total` deliberate space keyDowns; if the
                            // engine has received fewer space Keys than that (beyond
                            // one legitimately in flight), a space was dropped in
                            // capture — the suspected cause of the Google-Docs weld.
                            // Log it, content-free, so the flags can be correlated
                            // against the welds the user actually sees. CHANGES
                            // NOTHING — no compensation, no inject change.
                            if let Some(suspected) = funnel.note_space_observed(total) {
                                tracing::info!(
                                    "SPACE_DROP_SUSPECTED observed={} accepted={} drops_suspected={}",
                                    funnel.space_observed,
                                    funnel.space_accepted,
                                    suspected,
                                );
                            }
                        }
                        InputEvent::InjectionZone { dead } => {
                            // TF-08 host-scoped suppression: L1 reports whether the
                            // focused field is an injection dead zone (Safari
                            // web/contenteditable on Intel — phantom AX caret).
                            // Level signal, so only a flip is logged. Drives the
                            // watch-only fire gate; ingests nothing, counts nothing.
                            if dead != injection_dead_zone {
                                injection_dead_zone = dead;
                                tracing::info!("INJECTION_ZONE dead={dead}");
                            }
                        }
                        InputEvent::PermissionRequired => {
                            tracing::warn!(
                                "sidecar reports a capture permission missing — grant in \
                                 System Settings › Privacy & Security › Accessibility AND \
                                 Input Monitoring (capture needs both; an update can revoke \
                                 either independently)"
                            );
                            // A required grant is gone (Accessibility OR Input
                            // Monitoring — the tap needs Input Monitoring, the AX
                            // API needs Accessibility), revoked or never granted.
                            // Remember it so the not-active menu surfaces
                            // "Reconnect…" rather than "Restart capture" — only
                            // re-granting can recover.
                            permission_ok = false;
                        }
                        InputEvent::Heartbeat { tap_enabled, .. } => {
                            // Capture-health proof-of-life. Stamp the
                            // arrival time + tap_enabled flag and
                            // transition health via the shared helper
                            // (which only emits on actual transitions).
                            // Don't touch the FEED; heartbeats aren't
                            // input. The watchdog tick uses
                            // last_heartbeat_at to drive staleness
                            // transitions when this arm isn't firing.
                            last_heartbeat_at = Some(Instant::now());
                            // A heartbeat means the sidecar is alive and built a
                            // CGEventTap — which requires BOTH the Accessibility
                            // and Input Monitoring grants — so permissions are in
                            // effect again (clears a prior revoke once the
                            // re-grant(s) take hold).
                            permission_ok = true;
                            let new_health = if tap_enabled {
                                CaptureHealth::Live
                            } else {
                                CaptureHealth::Unhealthy
                            };
                            transition_capture_health(
                                &app_handle,
                                &mut current_capture_health,
                                new_health,
                            );
                        }
                        InputEvent::PermissionStatus {
                            accessibility,
                            input_monitoring,
                        } => {
                            // Per-grant snapshot from the sidecar (read-only AX +
                            // IOHID checks). Forward it verbatim to the UI so the
                            // onboarding / Reconnect permission rows can tick each
                            // grant the moment ITS own permission lands. Emitted on
                            // every receipt (not transition-gated) so a window that
                            // opens mid-flow converges to truth within a poll cycle.
                            // Independent of capture-health: `live` (both grants +
                            // tap armed) remains the gate for "capture is running".
                            // Not a keystroke — nothing to count in the funnel.
                            let _ = app_handle.emit(
                                EVT_PERMISSION_STATUS,
                                PermissionStatusEvent {
                                    accessibility,
                                    input_monitoring,
                                },
                            );
                            // (a) QA-20: latch the authoritative Accessibility grant
                            // so the capture-UI can surface a runtime revoke even
                            // while heartbeats (and thus `permission_ok`) stay healthy
                            // on the surviving Input-Monitoring tap.
                            accessibility_ok = accessibility;
                        }
                        InputEvent::Shutdown => break 'engine_loop,
                        InputEvent::CaretMoved { reason } => {
                            // Fix-B: the L1 adapter saw a gesture that can move
                            // the caret somewhere we can't dead-reckon — a
                            // mouse/trackpad click or a focus/app change. Reset
                            // the line model so a live correction can't fire
                            // against a stale buffer. Content-free event: nothing
                            // to ingest, nothing to count in the funnel.
                            let trigger = reason.as_deref().unwrap_or("caret");
                            reset_line_for_caret_move(
                                &app_handle,
                                &mut tokenizer,
                                &mut line_buf,
                                &mut line_dwells,
                                &mut caret,
                                &mut anchors,
                                &mut last_correction,
                                trigger,
                            );
                            // Watch Dog re-arm: an app/focus change means a
                            // possibly different corrector regime, so hard-reset
                            // the competitor sense and fire normally again (fail
                            // toward firing). A mouse click or up/down within the
                            // same app keeps the sense (the same competitor is
                            // still active). Content-free.
                            //
                            // STEP 1 (observe-only): the single `"focus"` reason
                            // is now split at the L1 boundary into `"app"` (a real
                            // application switch) and `"focus-element"` (an in-app
                            // focused-element re-publish — noisy in rich-text/web
                            // surfaces). Behaviour is UNCHANGED here: both tags
                            // reset the sense exactly as the old `"focus"` did, so
                            // we can first MEASURE how often each fires before
                            // Step 2 narrows this to `"app"` only.
                            if trigger == "app" || trigger == "focus-element" {
                                competitor_sense.reset();
                            }
                            // The caret moved off the boundary — a pending bubble
                            // could no longer inject safely. Drop it and hide it.
                            if pending_suggestion.take().is_some() {
                                let _ = app_handle.emit(EVT_CORRECTION_DISMISSED, ());
                            }
                        }
                        InputEvent::ShiftTap { .. } => {
                            // M3 bubble ACCEPT — an isolated Shift tap applies the
                            // pending suggestion. This is the ONLY path that
                            // injects (seal no longer auto-applies). Valid only
                            // while the suggestion is still armed (caret at the
                            // word boundary), so the deferred delete+retype lands
                            // in the right place. No suggestion → a bare Shift is a
                            // harmless no-op.
                            if let Some(ps) = pending_suggestion.take() {
                                // `after_len` = chars from the word's end to the
                                // caret now (the boundary + anything typed since).
                                // 1 = immediate (caret still at the boundary);
                                // >1 = the caret moved on → anchored replace.
                                let after_len = caret.saturating_sub(ps.word_end);
                                // Fail-safe accept gate. Garbling the user's text is
                                // the one outcome we refuse, so Shift injects ONLY
                                // when the edit can be placed exactly:
                                //   * IMMEDIATE (after_len == 1) — caret still right
                                //     after the word; the proven no-arrows delete+
                                //     retype. Always allowed.
                                //   * ANCHORED (caret typed past the word) — allowed
                                //     only when ANCHORED_ACCEPT_ENABLED *and* the line
                                //     model still holds the original word at the
                                //     anchored position. Currently DISABLED (a drifted
                                //     dead-reckoned caret doubled a word); see
                                //     ANCHORED_ACCEPT_ENABLED / word_at_anchor.
                                // Anything else dismisses — a missed accept is fine.
                                // QA-17: the caret-at-rest distance is the boundary
                                // PLUS any closing punct beyond the word (a trailing
                                // `"` makes it 2, not 1) — so `immediate` keys off
                                // `outer_trail_len + 1`, not a bare 1. Without this a
                                // quoted word read as "anchored" and (anchored being
                                // off) was silently declined.
                                let rest_after_len = ps.outer_trail_len + 1;
                                let immediate = after_len == rest_after_len;
                                let anchored_ok = ANCHORED_ACCEPT_ENABLED
                                    && caret > ps.word_end
                                    && after_len > rest_after_len
                                    && after_len <= MAX_ANCHOR_CHARS
                                    && word_at_anchor(
                                        &line_buf,
                                        ps.word_end,
                                        ps.word_len,
                                        &ps.typed,
                                    );
                                // (c) QA-20: verify Accessibility is still granted
                                // BEFORE confirming. Injection posts synthetic
                                // CGEvents, which require Accessibility; if it was
                                // revoked at runtime the post silently no-ops while
                                // the pipe write "succeeds", so we'd falsely report
                                // "Fixed" over an unchanged field (the trust-breaker).
                                // A no-prompt `AXIsProcessTrusted` read (content-free,
                                // Principle #8) is the honest proxy — a field read-back
                                // would violate content-blindness. On loss: abort, hide
                                // the bubble (never claim success), and let the
                                // capture-UI permission-revoked path surface Reconnect.
                                let ax_ok = accessibility_granted();
                                if !ax_ok {
                                    tracing::warn!(
                                        "CORRECTION_ACCEPT_ABORTED reason=accessibility_revoked typed={:?} target={:?}",
                                        ps.typed,
                                        ps.target
                                    );
                                    let _ = app_handle.emit(EVT_CORRECTION_DISMISSED, ());
                                } else if !immediate && !anchored_ok {
                                    // Stale / beyond-cap / anchored-disabled / model
                                    // mismatch — NEVER inject. A wrong-place edit is
                                    // the one outcome we refuse.
                                    tracing::info!(
                                        "CORRECTION_ACCEPT_DECLINED typed={:?} after_len={} word_end={} caret={} anchored_enabled={}",
                                        ps.typed,
                                        after_len,
                                        ps.word_end,
                                        caret,
                                        ANCHORED_ACCEPT_ENABLED
                                    );
                                    let _ = app_handle.emit(EVT_CORRECTION_DISMISSED, ());
                                } else {
                                    // Capture the delete_count we actually emit so the
                                    // CORRECTION_APPLIED line can show it (Principle #7:
                                    // an over-delete must be VISIBLE in the funnel, never
                                    // inferred). The immediate path's `+ 1` boundary char
                                    // is the suspect in contenteditable hosts (Google Docs
                                    // welded `and·perseverence` → `andperseverance`); the
                                    // root cause is its own — see
                                    // docs/qa-15-contenteditable-boundary-overdelete.md.
                                    // Logging delete_count beside `typed` + `boundary`
                                    // makes a leading/trailing boundary over-delete
                                    // self-evident without re-deriving it by hand.
                                    // Assigned unconditionally in both accept arms below
                                    // before the CORRECTION_APPLIED read — no seed value.
                                    let delete_count_emitted: u32;
                                    let applied = if immediate {
                                        // Immediate accept — caret at rest just past the
                                        // word (+ any trailing punct). Proven no-arrow
                                        // path: delete word + closing punct + boundary,
                                        // retype the punctuation re-wrapped around target
                                        // (QA-17). Reduces to word+boundary / target+
                                        // boundary when there is no surrounding punct.
                                        let (delete_count, replacement) =
                                            immediate_accept_injection(&ps);
                                        delete_count_emitted = delete_count;
                                        let echo_len =
                                            delete_count + replacement.chars().count() as u32;
                                        if send_inject_correction(
                                            &mut sidecar_child,
                                            delete_count,
                                            replacement,
                                        ) {
                                            pending_echo += echo_len;
                                            true
                                        } else {
                                            false
                                        }
                                    } else {
                                        // Anchored accept — the caret moved on. Reached
                                        // only when anchored_ok verified the word is
                                        // still at the anchored position. Positional
                                        // replace: Left × after_len (to just after the
                                        // word), Backspace × word_len, type target (no
                                        // boundary), Right × after_len (restore caret).
                                        let left = after_len as u32;
                                        let delete_count = ps.word_len as u32;
                                        delete_count_emitted = delete_count;
                                        let right = after_len as u32;
                                        let echo_len = left
                                            + delete_count
                                            + ps.target.chars().count() as u32
                                            + right;
                                        if send_inject_anchored(
                                            &mut sidecar_child,
                                            left,
                                            delete_count,
                                            ps.target.clone(),
                                            right,
                                        ) {
                                            pending_echo += echo_len;
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    if applied {
                                        funnel.corrections_applied += 1;
                                        tracing::info!(
                                            "CORRECTION_APPLIED typed={:?} target={:?} delete_count={} boundary={:?} after_len={}",
                                            ps.typed,
                                            ps.target,
                                            delete_count_emitted,
                                            ps.boundary,
                                            after_len
                                        );
                                        let _ = app_handle.emit(
                                            EVT_CORRECTION_APPLIED,
                                            CorrectionAppliedEvent {
                                                typed: ps.typed.clone(),
                                                target: ps.target.clone(),
                                                undo: false,
                                            },
                                        );
                                        // Arm the 6s Esc-undo (revert only). Carry the
                                        // punct (QA-17) + immediate flag so the revert
                                        // mirrors the accept and restores the quotes.
                                        last_correction = Some(LastCorrection {
                                            typed: ps.typed,
                                            target: ps.target,
                                            boundary: ps.boundary,
                                            lead: ps.lead,
                                            trail: ps.trail,
                                            immediate,
                                            after_len,
                                            fired_at_ms: now_ms(),
                                        });
                                        // The tap suppresses the injection echo, so the
                                        // engine never sees the text change — reset the
                                        // line to mirror the corrected state. The reset
                                        // re-bases the coordinate system; the anchored
                                        // accept uses RELATIVE distances (after_len), so
                                        // future accepts stay correct from the new base.
                                        tokenizer.reset_line();
                                        line_buf.clear();
                                        line_dwells.clear();
                                        caret = 0;
                                        anchors.clear();
                                        let _ = app_handle.emit(EVT_LINE_RESET, ());
                                        let snap = anchors.snapshot();
                                        let _ = app_handle.emit(
                                            EVT_ANCHOR_SNAPSHOT,
                                            anchor_emit_payload(&snap, &line_buf),
                                        );
                                    }
                                }
                            }
                        }
                        InputEvent::Backspace { .. } => {
                            // M3 bubble: a backspace is always an edit — drop any
                            // pending suggestion and hide the bubble (bias toward
                            // dismiss; we can't safely tell a source-word edit from
                            // a forward fix-up, and a wrong-place accept is worse
                            // than a missed one).
                            if pending_suggestion.take().is_some() {
                                let _ = app_handle.emit(EVT_CORRECTION_DISMISSED, ());
                            }
                            // Funnel: a backspace passed the pause filter and
                            // enters the edit pipeline (not a modifier drop).
                            funnel.keystrokes_accepted += 1;
                            // Emit so the debug feed shows backspaces — they're
                            // signal, not noise (CLAUDE.md: self-corrections).
                            let _ = app_handle.emit(
                                EVT_KEYSTROKE,
                                KeystrokePayload {
                                    key: "⌫".to_string(),
                                    dwell_ms: 0,
                                    ingest_latency_ms,
                                },
                            );

                            // Anchor delta — apply BEFORE mutating line_buf
                            // so we can read the deleted char (needed for
                            // Merge detection on boundary whitespace).
                            if caret > 0 && caret <= line_buf.len() {
                                let p = caret - 1;
                                let deleted = line_buf[p];
                                anchors.apply_delete(p, deleted);
                                line_buf.remove(p);
                                // Keep dwell buffer in lock-step with line_buf so
                                // the C4 motor-evidence gate stays accurate.
                                if p < line_dwells.len() {
                                    line_dwells.remove(p);
                                }
                                caret -= 1;
                            }

                            // Tokenizer is forward-only; rebuild the line
                            // and re-emit tokens. `try_register` dedupes
                            // replay so the anchor list isn't disturbed —
                            // the deltas above already updated existing
                            // anchors to their new positions.
                            tokenizer.reset_line();
                            let _ = app_handle.emit(EVT_LINE_RESET, ());
                            let replay: Vec<char> = line_buf.clone();
                            tracing::info!("REPLAY-BACKSPACE input_paused={} tokens_to_replay={}", input_paused, replay.len());
                            for c in replay {
                                if let Some(tok) = tokenizer.observe_char(c) {
                                    emit_sealed_token(
                                        &app_handle,
                                        tok,
                                        &mut anchors,
                                        lexicon,
                                        model.slip_detector.map(),
                                        &mut ledger,
                                        &mut motor_ledger,
                                        &line_dwells,
                                        &mut proposer,
                                        &mut funnel,
                                    );
                                }
                            }
                            // C5a outcome resolver tick. Backspaces are
                            // the canonical "user is correcting" signal;
                            // we must run the resolver immediately so
                            // Pending records flip the moment the user
                            // arrives at their final content.
                            tick_resolver(
                                &app_handle,
                                &mut resolver,
                                &mut motor_resolver,
                                &anchors,
                                &line_buf,
                                caret,
                                &mut ledger,
                                &mut motor_ledger,
                                &mut proposer,
                                &mut motor_map,
                                &mut word_freq,
                                prompted_capture_active,
                                &mut word_patterns,
                                &mut guess_ledger,
                                &mut funnel,
                                &mut tally,
                                &mut competitor_sense,
                            );
                            let snap = anchors.snapshot();
                            let _ = app_handle.emit(
                                EVT_ANCHOR_SNAPSHOT,
                                anchor_emit_payload(&snap, &line_buf),
                            );
                        }
                        InputEvent::Key { key, dwell_ms, modifiers, timestamp_ms, .. } => {
                            // M3 bubble gating: a pending suggestion survives this
                            // keystroke ONLY if it's a clean FORWARD character append
                            // (typing on). Set true in that one branch below; the
                            // end-of-arm check (also enforcing the distance cap)
                            // dismisses otherwise — nav keys, mid-line edits, the
                            // Escape that isn't an undo, shortcuts all drop it.
                            let mut keep_pending = false;
                            // Diagnostic log of the raw incoming codepoint(s)
                            // and modifier flags, so we can verify what the
                            // sidecar is actually emitting. DEBUG level: this
                            // prints the raw keystroke, so it must stay below the
                            // default log level — never on the console in normal
                            // operation, only under RUST_LOG=debug (privacy).
                            tracing::debug!(
                                "key in: {:?} chars=[{}] fn={} shift={} ctrl={} opt={} cmd={}",
                                key,
                                key.chars()
                                    .map(|c| format!("U+{:04X}", c as u32))
                                    .collect::<Vec<_>>()
                                    .join(","),
                                modifiers.function,
                                modifiers.shift,
                                modifiers.control,
                                modifiers.option,
                                modifiers.command,
                            );
                            let _ = app_handle.emit(
                                EVT_KEYSTROKE,
                                KeystrokePayload {
                                    key: key.clone(),
                                    dwell_ms,
                                    ingest_latency_ms,
                                },
                            );

                            // macOS reports navigation / function keys as
                            // either C0 control codes (U+001C–U+001F for
                            // arrows on US English layout) or AppKit
                            // private-use codes (U+F700..=U+F8FF, with Fn
                            // on or other layouts). Neither form is text —
                            // they must never reach line_buf, the tokenizer
                            // core, or the skeleton word buffer (otherwise
                            // they render as box glyphs inside tokens).
                            // Filter first, then map the known nav codes.
                            let single_char = {
                                let mut it = key.chars();
                                match (it.next(), it.next()) {
                                    (Some(c), None) => Some(c),
                                    _ => None,
                                }
                            };
                            let is_non_text = matches!(
                                single_char,
                                Some(c) if is_non_text_key(c)
                            );

                            // Diagnostic chords (Principle #7) — consumed here,
                            // never reach the tokenizer / line buffer:
                            //   Cmd+Shift+F → dump the funnel, then auto-reset
                            //                 (closes this run, opens a fresh one).
                            //   Cmd+Shift+R → reset only (start a run from zero).
                            if modifiers.command && modifiers.shift {
                                if matches!(single_char, Some('f') | Some('F')) {
                                    funnel.dump();
                                    dump_classifications(&word_patterns, &motor_map);
                                    funnel.reset(now_ms());
                                    continue;
                                }
                                if matches!(single_char, Some('r') | Some('R')) {
                                    funnel.reset(now_ms());
                                    continue;
                                }
                            }

                            // M3 correction Step 1 — Escape-windowed undo +
                            // teach-stop. While a correction is armed, the FIRST
                            // real user keystroke decides its fate (the tap
                            // suppresses our own injection echo, so the next key
                            // we see is genuinely the user's): a bare Escape
                            // within the window reverts the fix and removes the
                            // pattern from the allow-list so it won't recur;
                            // anything else is an implicit accept and just
                            // disarms. `take()` disarms in every branch.
                            if let Some(lc) = last_correction.take() {
                                let expired =
                                    now_ms().saturating_sub(lc.fired_at_ms) > UNDO_WINDOW_MS;
                                let is_escape = matches!(single_char, Some(KEY_ESCAPE))
                                    && !modifiers.command
                                    && !modifiers.control;
                                if !expired && is_escape {
                                    // Revert, mirroring the accept. `last_correction`
                                    // disarms on any non-Esc key, so the caret is
                                    // exactly where the accept left it.
                                    let reverted = if lc.immediate {
                                        // Immediate accept → caret at rest: delete the
                                        // injected target (+ re-wrapped punct + boundary),
                                        // retype the original word with its punctuation
                                        // (QA-17). Exact inverse of the accept geometry.
                                        let (delete_count, replacement) =
                                            immediate_revert_injection(&lc);
                                        let echo_len =
                                            delete_count + replacement.chars().count() as u32;
                                        if send_inject_correction(
                                            &mut sidecar_child,
                                            delete_count,
                                            replacement,
                                        ) {
                                            pending_echo += echo_len;
                                            true
                                        } else {
                                            false
                                        }
                                    } else {
                                        // Anchored accept → caret moved on: anchored
                                        // revert. Left × after_len, Backspace ×
                                        // target_len, type typed (no boundary), Right
                                        // × after_len. (Gated off; QA-17 punct re-wrap
                                        // not threaded here — see ANCHORED_ACCEPT_ENABLED.)
                                        let target_len = lc.target.chars().count() as u32;
                                        let left = lc.after_len as u32;
                                        let right = lc.after_len as u32;
                                        let echo_len = left
                                            + target_len
                                            + lc.typed.chars().count() as u32
                                            + right;
                                        if send_inject_anchored(
                                            &mut sidecar_child,
                                            left,
                                            target_len,
                                            lc.typed.clone(),
                                            right,
                                        ) {
                                            pending_echo += echo_len;
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    if reverted {
                                        funnel.corrections_undone += 1;
                                        tracing::info!(
                                            "CORRECTION_UNDONE typed={:?} target={:?} after_len={}",
                                            lc.typed,
                                            lc.target,
                                            lc.after_len
                                        );
                                        // The bubble shows the revert direction.
                                        let _ = app_handle.emit(
                                            EVT_CORRECTION_APPLIED,
                                            CorrectionAppliedEvent {
                                                typed: lc.target.clone(),
                                                target: lc.typed.clone(),
                                                undo: true,
                                            },
                                        );
                                    }
                                    // Esc does ONE predictable thing: revert. It no
                                    // longer teach-stops the pattern — backing off a
                                    // pattern comes from the user IGNORING the
                                    // suggestion (not accepting), not from undo.
                                    // The revert's echo is suppressed too; reset
                                    // the line to mirror the restored text.
                                    tokenizer.reset_line();
                                    line_buf.clear();
                                    line_dwells.clear();
                                    caret = 0;
                                    anchors.clear();
                                    let _ = app_handle.emit(EVT_LINE_RESET, ());
                                    let snap = anchors.snapshot();
                                    let _ = app_handle.emit(
                                        EVT_ANCHOR_SNAPSHOT,
                                        anchor_emit_payload(&snap, &line_buf),
                                    );
                                    continue;
                                }
                                // Not an undo — implicit accept. `lc` is dropped
                                // (disarmed); fall through to process this key
                                // normally.
                            }

                            if is_non_text {
                                // Caret-only handling. Left/Right/Home/End
                                // map to caret moves; Up/Down and every
                                // other non-text codepoint are intentional
                                // no-ops for the single-line model. No
                                // edit, no token feed — and the buffer is
                                // never written.
                                let c = single_char.unwrap();
                                if is_vertical_nav(c) {
                                    // Fix-B: Up/Down/PageUp/PageDown move the
                                    // caret to another line/screen the single-
                                    // line model can't follow. `nav_action`
                                    // returns None for these (caret stays put =
                                    // silent desync); reset the line instead.
                                    tracing::info!(
                                        "non-text key U+{:04X} — vertical nav, line reset",
                                        c as u32
                                    );
                                    reset_line_for_caret_move(
                                        &app_handle,
                                        &mut tokenizer,
                                        &mut line_buf,
                                        &mut line_dwells,
                                        &mut caret,
                                        &mut anchors,
                                        &mut last_correction,
                                        "updown",
                                    );
                                } else {
                                    tracing::info!(
                                        "non-text key U+{:04X} — caret-only handling",
                                        c as u32
                                    );
                                    // ONE coherent handler for the horizontal
                                    // nav keys. Anything not mapped (F1–F12,
                                    // other PU codepoints) returns `None` and
                                    // we no-op. The Command flag distinguishes
                                    // Cmd+Left/Right (= Home/End on macOS) from
                                    // plain arrow navigation.
                                    if let Some(new_caret) =
                                        nav_action(c, caret, line_buf.len(), modifiers.command)
                                    {
                                        caret = new_caret;
                                    }
                                    // Anchor positions don't move on pure
                                    // navigation, so no snapshot emit needed.
                                }
                            } else if modifiers.command || modifiers.control {
                                // System shortcut (Cmd+X, Ctrl+X, Cmd+Shift+D,
                                // etc.) — the key's character is the shortcut
                                // letter, not text the user means to type.
                                // Drop before the tokenizer / line_buf /
                                // anchor path so it can't seal as a literal
                                // character (the original bug: Cmd+Shift+D
                                // toggling the panel produced `tok_core =
                                // "dalpha"` because the "d" reached the
                                // line buffer).
                                //
                                // Scope: line_buf / tokenizer / anchors only.
                                // EVT_KEYSTROKE was already emitted upstream
                                // so the FEED still reflects the press with
                                // its modifier flags. The motor baseline
                                // probe also ran upstream and continues to
                                // observe the dwell — the press IS real
                                // biomechanical data, just not text. Option
                                // (Alt) is NOT filtered: on macOS,
                                // Option+letter is a text-producing dead-key
                                // sequence (Option+e then a → á). Shift and
                                // Caps Lock are text modifiers and stay.
                                tracing::info!(
                                    "SHORTCUT-DROP key={:?} cmd={} ctrl={}",
                                    key,
                                    modifiers.command,
                                    modifiers.control,
                                );
                            } else {
                                // Feed the tokenizer + drive the anchor tracker.
                                // Component 1 (token) and Component 2 (anchor)
                                // both live downstream of this block.
                                let mut ch_iter = key.chars();
                                match (ch_iter.next(), ch_iter.next()) {
                                    (Some(c), None) => {
                                        if c == '\n' || c == '\r' {
                                            // Funnel: newline accepted as input.
                                            funnel.keystrokes_accepted += 1;
                                            // True line reset. Tokens panel clears,
                                            // anchors are dropped. Any token the
                                            // newline sealed goes through the same
                                            // helper as every other emission so the
                                            // lexicon panel sees that final word.
                                            // Note: we emit BEFORE clearing line_dwells
                                            // so the motor-evidence gate sees the
                                            // pre-newline char dwells.
                                            if let Some(tok) = tokenizer.observe_char(c) {
                                                emit_sealed_token(
                                                    &app_handle,
                                                    tok,
                                                    &mut anchors,
                                                    lexicon,
                                                    model.slip_detector.map(),
                                                    &mut ledger,
                                                    &mut motor_ledger,
                                                    &line_dwells,
                                                    &mut proposer,
                                                    &mut funnel,
                                                );
                                            }
                                            line_buf.clear();
                                            line_dwells.clear();
                                            caret = 0;
                                            anchors.clear();
                                            let _ = app_handle.emit(EVT_LINE_RESET, ());
                                            let snap = anchors.snapshot();
                                            let _ = app_handle.emit(
                                                EVT_ANCHOR_SNAPSHOT,
                                                anchor_emit_payload(&snap, &line_buf),
                                            );
                                        } else {
                                            // Normal char insert. The single
                                            // `insert_text_char` entry point
                                            // guards the buffer against PU
                                            // codepoints — anchor delta and
                                            // tokenizer feed only run if the
                                            // char was actually accepted.
                                            let was_end_of_line = caret == line_buf.len();
                                            // Mirror the insert position used inside
                                            // insert_text_char so line_dwells stays
                                            // in lock-step with line_buf.
                                            let insert_pos = caret.min(line_buf.len());
                                            let Some(new_caret) =
                                                insert_text_char(&mut line_buf, caret, c)
                                            else {
                                                continue;
                                            };
                                            // Funnel: a text char was accepted into
                                            // the line buffer (past all filters).
                                            funnel.keystrokes_accepted += 1;
                                            // C4 motor-evidence proxy: store this
                                            // char's dwell at the same index. A real
                                            // keystroke carries a non-zero dwell;
                                            // pasted / synthetic chars come through
                                            // with dwell == 0 and fail the gate.
                                            line_dwells.insert(insert_pos, dwell_ms);
                                            anchors.apply_insert(caret, c);
                                            caret = new_caret;

                                            // Always false now: the bubble defers
                                            // injection to the Shift-accept, so a
                                            // seal never resets the line and the
                                            // trailing resolver tick always runs.
                                            let corrected = false;
                                            if was_end_of_line {
                                                // Clean forward append (typing on) —
                                                // a pending suggestion survives this
                                                // keystroke, subject to the distance
                                                // cap enforced at the end of the arm.
                                                keep_pending = true;
                                                if let Some(tok) = tokenizer.observe_char(c) {
                                                    // M3 correction Step 1: a word
                                                    // just completed at a boundary
                                                    // (space / punctuation — NOT
                                                    // Return; see note below). If
                                                    // the master gate is on and the
                                                    // word is an enabled allow-list
                                                    // pattern, fire a correction.
                                                    // Capture what injection needs
                                                    // BEFORE `tok` moves into
                                                    // emit_sealed_token.
                                                    // M3 bubble: the ENGINE decides what to suggest — the dictionary/bold
                                                    // lane (a non-word converging on exactly one dictionary word, suggested
                                                    // immediately) then the learned classifier. This is the SAME decision the
                                                    // shadow log records below; the manual allow-list no longer gates a fire
                                                    // (its per-pattern enable was retired, so it's always empty). The master
                                                    // gate (`correction_enabled`) still arms the whole feature. Capture what
                                                    // the bubble needs BEFORE `tok` moves into emit_sealed_token.
                                                    let mut fire: Option<PendingSuggestion> = None;
                                                    // FIRE_TIMING t1 — set at the fire decision below; hoisted here
                                                    // so it stays in scope for the t2 emit log (measurement only).
                                                    let mut t1: u64 = 0;
                                                    if matches!(tok.kind, TokenKind::Word) {
                                                        let word_len = tok.end - tok.start;
                                                        let word_end = tok.end;
                                                        // QA-17: match on the word with any surrounding punctuation
                                                        // stripped, then re-attach it on inject. `inner`/`q_lead`/
                                                        // `q_trail` peel a matched straight-quote pair off the core
                                                        // (5a: `'waht'` → `waht` + `'`/`'`); `outer_trail` is the
                                                        // tokenizer's closing punct beyond the core (5b: the `"` in
                                                        // `"beleive"`), captured before `tok` moves into the seal.
                                                        let (inner, q_lead, q_trail) =
                                                            strip_matched_quotes(&tok.core);
                                                        let outer_trail = tok.trailing.clone();
                                                        // Lever 2 — LEARNED lane first. `shadow_suggestion` is a
                                                        // cheap lookup over the user's own confirmed `typed → target`
                                                        // patterns; only when it has nothing do we run the dictionary
                                                        // convergence scan (itself O(word) after Lever 1). Reordering
                                                        // keeps the hot path minimal for repeat slips AND lets a
                                                        // pattern the user has taught us win a tie over a generic
                                                        // dictionary convergence. (Before, convergence won the tie;
                                                        // for the same word both lanes almost always agree on the
                                                        // target — where they differ, the learned pattern is the more
                                                        // personal signal.) Raw suggestion: `inner` keeps the user's
                                                        // case (punctuation stripped, QA-17), target lowercase.
                                                        let raw: Option<(String, String)> = if let Some(sh) =
                                                            shadow_suggestion(&inner, &word_patterns, lexicon, &motor_map)
                                                        {
                                                            if let Some(p) = shadow_log_path() {
                                                                append_shadow_log(&p, now_ms(), &sh);
                                                            }
                                                            Some((inner.clone(), sh.target.clone()))
                                                        } else {
                                                            let scan = correction_engine::shadow_convergence_scan(
                                                                &inner,
                                                                lexicon,
                                                                &motor_map,
                                                            );
                                                            if scan.convergent {
                                                                if let Some(p) = shadow_log_path() {
                                                                    append_shadow_bold_log(&p, now_ms(), &inner, &scan);
                                                                }
                                                                // convergent ⇒ exactly one fire target (the lone
                                                                // candidate, or the frequency-dominant one).
                                                                scan.target.as_ref().map(|t| (inner.clone(), t.clone()))
                                                            } else {
                                                                None
                                                            }
                                                        };
                                                        // Contraction filter: drop apostrophe-only /
                                                        // possessive fixes — Auto-Correct's job, not
                                                        // ours (we own motor garbles).
                                                        let apostrophe_suppressed = raw
                                                            .as_ref()
                                                            .is_some_and(|(t, g)| is_apostrophe_fix(t, g));
                                                        let suggestion = if apostrophe_suppressed { None } else { raw };
                                                        // Watch Dog Not Attack Dog (M3 autocorrect
                                                        // coexistence): when a competing corrector is
                                                        // behaviourally sensed AND this is a
                                                        // host-redundant (common) fix, STAND DOWN — let
                                                        // the host make the fix rather than double-correct
                                                        // it. Idiosyncratic fixes are never host-redundant,
                                                        // so they fall straight through and fire (his core
                                                        // value — never suppressed). No app/settings read
                                                        // (Principle #8); the shadow intent log above still
                                                        // recorded what he WOULD suggest.
                                                        let competitor_deferred = competitor_sense
                                                            .is_deferring(now_ms())
                                                            && suggestion.as_ref().is_some_and(|(t, g)| {
                                                                correction_engine::is_host_redundant(t, g, lexicon)
                                                            });
                                                        // FIRE_TIMING t1 — the fire decision is resolved (Some/None).
                                                        t1 = now_ms();
                                                        // The master gate turns a suggestion into a live bubble. Log the
                                                        // decision so a no-fire is diagnosable from the dev Terminal.
                                                        // `competitor_deferred` is a content-free verdict bool
                                                        // (Principle #8): it never names the app or the reason's source.
                                                        tracing::info!(
                                                            "FIRE_DECISION word={:?} gate_on={} suggestion={:?} apostrophe_suppressed={} competitor_deferred={} dead_zone={} t1={} t1_minus_t0_ms={}",
                                                            tok.core,
                                                            allow_list.correction_enabled,
                                                            suggestion.as_ref().map(|(t, g)| format!("{t}->{g}")),
                                                            apostrophe_suppressed,
                                                            competitor_deferred,
                                                            injection_dead_zone,
                                                            t1,
                                                            t1.saturating_sub(t_recv),
                                                        );
                                                        if allow_list.correction_enabled {
                                                            if injection_dead_zone {
                                                                // TF-08 dead zone (Safari-web/Intel
                                                                // phantom caret): a synthetic fix can't
                                                                // land, so WATCH-ONLY — withhold the bubble
                                                                // entirely (silent, no cue) while the
                                                                // observation/learning below runs as normal.
                                                                // Supersedes the competitor deferral (we
                                                                // inject nothing, so nothing double-corrects).
                                                                // A deliberate, counted drop (Principle #7);
                                                                // `fire` stays None.
                                                                funnel.dead_zone_suppressed += 1;
                                                            } else if competitor_deferred {
                                                                // A live bubble withheld to avoid
                                                                // double-correcting the host — a deliberate,
                                                                // counted drop (Principle #7). `fire` stays
                                                                // None.
                                                                funnel.competitor_deferred += 1;
                                                            } else {
                                                                // Preserve the user's capitalisation in the
                                                                // target — never change their casing. Build the
                                                                // full pending here (QA-17 punct carried through
                                                                // `lead`/`trail`/`outer_trail_len`) while the
                                                                // stripped-off quotes are still in scope.
                                                                fire = suggestion.map(|(t, g)| {
                                                                    // UK English (v0.3.0): rewrite the
                                                                    // suggested spelling to the system
                                                                    // locale's variant (color↔colour) —
                                                                    // suggestion only; membership already
                                                                    // accepts both. No-op for non-variant
                                                                    // words. This is the SINGLE place the
                                                                    // target spelling is finalised, so the
                                                                    // injected text and the Esc-undo (which
                                                                    // both read `ps.target`) stay in lockstep.
                                                                    let g = correction_engine::localize_spelling(
                                                                        &g,
                                                                        spelling_variant,
                                                                    )
                                                                    .unwrap_or(g);
                                                                    let g = match_source_case(&t, &g);
                                                                    PendingSuggestion {
                                                                        typed: t,
                                                                        target: g,
                                                                        boundary: c,
                                                                        word_len,
                                                                        word_end,
                                                                        lead: q_lead.clone(),
                                                                        trail: format!("{q_trail}{outer_trail}"),
                                                                        outer_trail_len: outer_trail
                                                                            .chars()
                                                                            .count(),
                                                                        armed_at_ms: now_ms(),
                                                                    }
                                                                });
                                                            }
                                                        }
                                                    }
                                                    emit_sealed_token(
                                                        &app_handle,
                                                        tok,
                                                        &mut anchors,
                                                        lexicon,
                                                        model.slip_detector.map(),
                                                        &mut ledger,
                                                        &mut motor_ledger,
                                                        &line_dwells,
                                                        &mut proposer,
                                                        &mut funnel,
                                                    );
                                                    if let Some(ps) = fire {
                                                        // M3 bubble: do NOT inject at seal — SUGGEST. The bubble shows
                                                        // `typed → target`; nothing changes on screen until the user
                                                        // accepts with an isolated Shift tap. Armed against the current
                                                        // caret (end of line here) and cleared by the next keystroke, a
                                                        // caret move, or the ~5s timeout — so an accept only ever injects
                                                        // while the caret is still at this word's boundary.
                                                        // Indices are case-independent (positions are
                                                        // the same whatever the casing) — compare the
                                                        // lowercased forms so a leading capital doesn't
                                                        // throw the alignment off.
                                                        let highlight = correction_engine::corrected_target_indices(
                                                            &ps.typed.to_lowercase(),
                                                            &ps.target.to_lowercase(),
                                                        );
                                                        let _ = app_handle.emit(
                                                            EVT_CORRECTION_SUGGESTED,
                                                            CorrectionSuggestedEvent {
                                                                typed: ps.typed.clone(),
                                                                target: ps.target.clone(),
                                                                highlight,
                                                            },
                                                        );
                                                        // FIRE_TIMING t2 — EVT_CORRECTION_SUGGESTED handed to Tauri.
                                                        // One self-contained line carries the engine-side breakdown:
                                                        //   t0 = keystroke received from L1, t1 = fire decided,
                                                        //   t2 = event emitted; dwell_ms = the boundary key's
                                                        //   press→release hold (emitted on keyUp, so this hold is
                                                        //   part of physical-press → t0). os_ts is the CGEvent clock
                                                        //   (uptime-based, NOT comparable to t0..t2 — context only).
                                                        let t2 = now_ms();
                                                        tracing::info!(
                                                            "FIRE_TIMING typed={:?} target={:?} word_len={} t0_recv={} t1_decision={} t2_emit={} d_t0_t2_ms={} dwell_ms={} os_ts={}",
                                                            ps.typed,
                                                            ps.target,
                                                            ps.word_len,
                                                            t_recv,
                                                            t1,
                                                            t2,
                                                            t2.saturating_sub(t_recv),
                                                            dwell_ms,
                                                            timestamp_ms,
                                                        );
                                                        pending_suggestion = Some(ps);
                                                    }
                                                }
                                            } else {
                                                // Mid-line insert: forward
                                                // streaming would produce
                                                // tokens whose cores don't
                                                // match the line. Rebuild
                                                // from line_buf. `try_register`
                                                // dedupes; the apply_insert
                                                // above already updated the
                                                // existing anchors' spans.
                                                tokenizer.reset_line();
                                                let _ = app_handle.emit(EVT_LINE_RESET, ());
                                                let replay: Vec<char> = line_buf.clone();
                                                tracing::info!("REPLAY-MIDLINE input_paused={} tokens_to_replay={}", input_paused, replay.len());
                                                for c in replay {
                                                    if let Some(tok) = tokenizer.observe_char(c) {
                                                        emit_sealed_token(
                                                            &app_handle,
                                                            tok,
                                                            &mut anchors,
                                                            lexicon,
                                                            model.slip_detector.map(),
                                                            &mut ledger,
                                                            &mut motor_ledger,
                                                            &line_dwells,
                                                            &mut proposer,
                                                            &mut funnel,
                                                        );
                                                    }
                                                }
                                            }

                                            // C5a resolver tick. Covers
                                            // both the fresh-anchor case
                                            // (a newly-sealed token's
                                            // debounce timer starts now)
                                            // and the revisit case (a
                                            // mid-line edit may have
                                            // flipped a previously-resolved
                                            // record). Skipped when a
                                            // correction just reset the line
                                            // (it emitted its own snapshot).
                                            if !corrected {
                                                tick_resolver(
                                                    &app_handle,
                                                    &mut resolver,
                                                    &mut motor_resolver,
                                                    &anchors,
                                                    &line_buf,
                                                    caret,
                                                    &mut ledger,
                                                    &mut motor_ledger,
                                                    &mut proposer,
                                                    &mut motor_map,
                                                    &mut word_freq,
                                                    prompted_capture_active,
                                                    &mut word_patterns,
                                                    &mut guess_ledger,
                                                    &mut funnel,
                                                    &mut tally,
                                                    &mut competitor_sense,
                                                );
                                                let snap = anchors.snapshot();
                                                let _ = app_handle.emit(
                                                    EVT_ANCHOR_SNAPSHOT,
                                                    anchor_emit_payload(&snap, &line_buf),
                                                );
                                            }
                                        }
                                    }
                                    _ => {
                                        // Multi-char / unknown special key —
                                        // abandon current line context entirely.
                                        // This is also the engine-level paste
                                        // arm (e.g. a Cmd+V that arrives as a
                                        // single Key with the full pasted
                                        // string): nothing reaches the ledger
                                        // because no token is sealed here.
                                        tokenizer.reset_line();
                                        line_buf.clear();
                                        line_dwells.clear();
                                        caret = 0;
                                        anchors.clear();
                                        let _ = app_handle.emit(EVT_LINE_RESET, ());
                                        let snap = anchors.snapshot();
                                        let _ = app_handle.emit(
                                            EVT_ANCHOR_SNAPSHOT,
                                            anchor_emit_payload(&snap, &line_buf),
                                        );
                                    }
                                }
                            }

                            // No parallel word-buffer / boundary detection:
                            // the tokenizer above already sealed any token
                            // that this keystroke triggered, and
                            // `emit_sealed_token` ran the full L4 pipeline
                            // (lexicon → candidates → score → decide)
                            // emitting per-stage panel events. The skeleton
                            // tge→the lookup is retired.

                            // M3 bubble gating: keep the pending suggestion ONLY if
                            // this was a clean forward keystroke (keep_pending) AND
                            // the caret is still PAST the word and within the
                            // distance cap; otherwise drop + hide it. Catches nav
                            // keys, mid-line edits, a non-undo Escape, shortcuts,
                            // and the over-the-cap case in one place. Bias toward
                            // dismiss — a missed accept is fine, a wrong-place edit
                            // is not.
                            if let Some(ps) = &pending_suggestion {
                                // Drop the bubble the moment the caret leaves the
                                // accept-reachable rest position — a visible bubble
                                // must always mean Shift will work. The rest position
                                // is `outer_trail_len + 1` (boundary + any closing
                                // punct), so a trailing-quoted word (QA-17 5b) is not
                                // dropped the instant it seals. See `pending_survives`.
                                if !pending_survives(
                                    keep_pending,
                                    caret,
                                    ps.word_end,
                                    ps.outer_trail_len,
                                    ANCHORED_ACCEPT_ENABLED,
                                ) {
                                    pending_suggestion = None;
                                    let _ = app_handle.emit(EVT_CORRECTION_DISMISSED, ());
                                }
                            }
                        }
                    }
                }
                CommandEvent::Stderr(bytes) => {
                    if let Ok(s) = std::str::from_utf8(&bytes) {
                        let trimmed = s.trim_end();
                        if !trimmed.is_empty() {
                            tracing::info!(target: "sidecar", "{trimmed}");
                        }
                    }
                }
                CommandEvent::Error(msg) => {
                    tracing::error!("sidecar error: {msg}");
                }
                CommandEvent::Terminated(payload) => {
                    tracing::warn!(
                        "sidecar terminated (code={:?}, signal={:?}) — attempting hard restart",
                        payload.code,
                        payload.signal
                    );
                    // Always mark Stopped first so the panel reflects
                    // truth even if the respawn succeeds quickly —
                    // the user momentarily sees red, then green when
                    // the new sidecar's first heartbeat arrives.
                    transition_capture_health(
                        &app_handle,
                        &mut current_capture_health,
                        CaptureHealth::Stopped,
                    );
                    if try_hard_restart(
                        &app_handle,
                        &mut rx,
                        &mut sidecar_child,
                        &mut respawn_attempts,
                    ) {
                        // Reset heartbeat clock so the watchdog
                        // doesn't immediately fire on staleness while
                        // the new sidecar is booting.
                        last_heartbeat_at = Some(Instant::now());
                    } else {
                        // Respawn refused (cap hit) or failed —
                        // terminal Stopped. Engine task LIVES so the
                        // control channel stays open; manual
                        // RestartCapture clears the attempt window
                        // and tries again. Sleep before the next
                        // loop iteration so the now-closed rx doesn't
                        // immediately tight-loop on .recv() → None.
                        tracing::error!(
                            "sidecar respawn unavailable — capture is terminal Stopped \
                             until manual Restart capture"
                        );
                        tokio::time::sleep(
                            std::time::Duration::from_secs(5),
                        ).await;
                    }
                }
                _ => {}
                    }
                }
                cmd = control_rx.recv() => {
                    let Some(cmd) = cmd else { break 'engine_loop; };
                    match cmd {
                        EngineControl::ResetLexicon => {
                            // True engine-side wipe — distinct from a
                            // panel-side "Clear" (panel mirrors only,
                            // engine retained the truth). After this:
                            // proposer state empty, lex.learned empty,
                            // is_known back to bundled-only.
                            proposer.reset_all();
                            let _ = app_handle.emit(EVT_LEXICON_RESET, ());
                            let _ = app_handle.emit(
                                EVT_LEARNED_SNAPSHOT,
                                proposer.learned_snapshot(),
                            );
                            tracing::info!("LEXICON reset by Tauri command");
                        }
                        EngineControl::SetLearningPaused(paused) => {
                            // Echo the engine's actual flag back to
                            // the panel after the set — covers the
                            // case where the panel's optimistic state
                            // diverges (e.g. webview reload restored
                            // a stale view of the flag).
                            proposer.set_credit_paused(paused);
                            let _ = app_handle.emit(
                                EVT_LEARNING_PAUSED,
                                LearningPausedEvent { paused: proposer.credit_paused() },
                            );
                        }
                        EngineControl::RestartCapture => {
                            // Manual recovery path. Per the design
                            // contract: clear the respawn attempt
                            // window FIRST — the user explicitly
                            // asked, so this is a fresh start signal
                            // even if we were terminal-Stopped.
                            respawn_attempts.clear();
                            // Try soft restart by writing to the
                            // sidecar's stdin. Common case: sidecar
                            // alive but tap stuck. Soft is enough.
                            const RESTART_LINE: &[u8] =
                                b"{\"type\":\"restart_tap\"}\n";
                            match sidecar_child.write(RESTART_LINE) {
                                Ok(_) => {
                                    tracing::info!(
                                        "soft capture restart requested"
                                    );
                                }
                                Err(e) => {
                                    // Soft restart failed — sidecar's
                                    // stdin is closed, which means
                                    // the sidecar process is dead.
                                    // Escalate to hard restart
                                    // immediately rather than waiting
                                    // for the watchdog.
                                    tracing::warn!(
                                        "soft restart failed ({e}) — escalating to hard restart"
                                    );
                                    transition_capture_health(
                                        &app_handle,
                                        &mut current_capture_health,
                                        CaptureHealth::Stopped,
                                    );
                                    if try_hard_restart(
                                        &app_handle,
                                        &mut rx,
                                        &mut sidecar_child,
                                        &mut respawn_attempts,
                                    ) {
                                        last_heartbeat_at = Some(Instant::now());
                                    }
                                }
                            }
                        }
                        EngineControl::SetInputPaused(paused) => {
                            let was_paused = input_paused;
                            input_paused = paused;
                            if !was_paused && paused {
                                // Pause-on transition: flush line state
                                // so the engine wakes on a clean boundary
                                // when input resumes. Same shape as the
                                // multi-char/paste reset path elsewhere
                                // in the loop. Ledger pendings are left
                                // alone — they'd just linger without
                                // anchors, same as any unmonitored gap.
                                tracing::info!(
                                    "PAUSE-ON-PRE line_buf.len={} line_dwells.len={} anchors.count={} caret={}",
                                    line_buf.len(),
                                    line_dwells.len(),
                                    anchors.anchors().len(),
                                    caret,
                                );
                                tokenizer.reset_line();
                                line_buf.clear();
                                line_dwells.clear();
                                caret = 0;
                                anchors.clear();
                                tracing::info!(
                                    "PAUSE-ON-POST line_buf.len={} line_dwells.len={} anchors.count={} caret={}",
                                    line_buf.len(),
                                    line_dwells.len(),
                                    anchors.anchors().len(),
                                    caret,
                                );
                                let _ = app_handle.emit(EVT_LINE_RESET, ());
                                let snap = anchors.snapshot();
                                let _ = app_handle.emit(
                                    EVT_ANCHOR_SNAPSHOT,
                                    anchor_emit_payload(&snap, &line_buf),
                                );
                            }
                            let _ = app_handle.emit(
                                EVT_INPUT_PAUSED,
                                InputPausedEvent { paused: input_paused },
                            );
                            tracing::info!(
                                "engine input pause set to {input_paused}"
                            );
                        }
                        EngineControl::RequestMotorStability => {
                            // On-demand read-model emit (Practice pulls fresh
                            // weakest-keys at session start). Read-only.
                            let report: StabilityReport =
                                motor_map.stability_report(WEAKEST_PREVIEW_N);
                            let _ = app_handle.emit(EVT_MOTOR_STABILITY, report);
                        }
                        EngineControl::RequestPracticeTrend { keys } => {
                            // Reconstruct the per-key trend from daily snapshot
                            // archives. Read-only history scan; the Practice
                            // snapshot renders a key only if it has ≥2 points.
                            let trend = build_practice_trend(
                                snapshots_dir.as_deref(),
                                &keys,
                                now_ms(),
                            );
                            let _ = app_handle.emit(EVT_PRACTICE_TREND, trend);
                        }
                        EngineControl::SetPromptedCaptureActive(active) => {
                            // C5e: gate the word-freq tally while prompted
                            // (warm-up / Practice) text is on screen. Motor-map
                            // observation is untouched. Content-blind — a bare
                            // on/off, no app id, no text.
                            prompted_capture_active = active;
                            tracing::info!("PROMPTED_CAPTURE active={active}");
                        }
                        EngineControl::SetSpellingVariant(variant) => {
                            // UK English (v0.3.0): L5 resolved the OS locale to a
                            // spelling variant; adopt it for future suggestions.
                            // Content-free (a two-value enum), never persisted.
                            spelling_variant = variant;
                            // Flip the lexicon's locale gate: the opposite-locale
                            // spelling of every VarCon pair becomes unknown, so a
                            // garble converges only on the locale spelling and a
                            // cleanly-typed opposite variant routes through the
                            // slip engine toward it (LOCALE wins over incidental
                            // learning). Single source of truth for membership.
                            Lexicon::shared().set_spelling_variant(variant);
                            tracing::info!("SPELLING_VARIANT set to {variant:?}");
                        }
                        EngineControl::SetCorrectionEnabled(enabled) => {
                            // M3 Step 1 master gate — the instant global on/off.
                            // Persist + echo so the tray check and panel switch
                            // converge on the engine's authoritative state. Turn
                            // OFF disarms any pending undo (no fix to revert).
                            if allow_list.set_enabled(enabled) {
                                if !enabled {
                                    last_correction = None;
                                    if pending_suggestion.take().is_some() {
                                        let _ = app_handle.emit(EVT_CORRECTION_DISMISSED, ());
                                    }
                                }
                                tracing::info!("CORRECTION_GATE enabled={enabled}");
                                persist_and_emit_allow_list(
                                    &app_handle,
                                    &allow_list,
                                    allow_list_path.as_deref(),
                                );
                            } else {
                                // No change, but still echo so an optimistic UI
                                // that diverged reconverges.
                                let _ = app_handle.emit(EVT_CORRECTION_STATE, allow_list.clone());
                            }
                        }
                        EngineControl::RequestAllowList => {
                            // Panel/tray pull fresh state on open. Read-only.
                            let _ = app_handle.emit(EVT_CORRECTION_STATE, allow_list.clone());
                        }
                        EngineControl::RestoreData { path } => {
                            // Sole-writer restore. Validate-all-then-apply; the
                            // helper swaps the in-memory maps on success.
                            match restore_from_backup(
                                &path,
                                &mut motor_map,
                                &mut word_freq,
                                &mut word_patterns,
                            ) {
                                Ok(()) => {
                                    // The runtime lexicon isn't carried in the
                                    // bundle — start it clean so is_known relearns
                                    // rather than carrying the pre-restore session.
                                    proposer.reset_all();
                                    tracing::info!("DATA_RESTORED from {path}");
                                    let _ = app_handle.emit(
                                        EVT_DATA_RESTORED,
                                        serde_json::json!({ "ok": true, "message": "" }),
                                    );
                                    let _ = app_handle
                                        .emit(EVT_LEARNED_SNAPSHOT, proposer.learned_snapshot());
                                    let _ = app_handle.emit(
                                        EVT_MOTOR_STABILITY,
                                        motor_map.stability_report(WEAKEST_PREVIEW_N),
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!("DATA_RESTORE failed: {e}");
                                    let _ = app_handle.emit(
                                        EVT_DATA_RESTORED,
                                        serde_json::json!({ "ok": false, "message": e }),
                                    );
                                }
                            }
                        }
                        EngineControl::DeleteAllData => {
                            // Sole-writer delete → a true first-launch-clean state.
                            match delete_all_local_data(
                                &mut motor_map,
                                &mut word_freq,
                                &mut word_patterns,
                            ) {
                                Ok(()) => {
                                    proposer.reset_all();
                                    // Genuine clean slate: corrections back to off
                                    // (default) + allow-list cleared, persisted and
                                    // echoed so the tray + Settings toggle converge.
                                    allow_list.set_enabled(false);
                                    allow_list.patterns.clear();
                                    last_correction = None;
                                    if pending_suggestion.take().is_some() {
                                        let _ =
                                            app_handle.emit(EVT_CORRECTION_DISMISSED, ());
                                    }
                                    persist_and_emit_allow_list(
                                        &app_handle,
                                        &allow_list,
                                        allow_list_path.as_deref(),
                                    );
                                    tracing::info!("DATA_DELETED — fresh start");
                                    let _ = app_handle.emit(
                                        EVT_DATA_DELETED,
                                        serde_json::json!({ "ok": true, "message": "" }),
                                    );
                                    let _ = app_handle
                                        .emit(EVT_LEARNED_SNAPSHOT, proposer.learned_snapshot());
                                    let _ = app_handle.emit(
                                        EVT_MOTOR_STABILITY,
                                        motor_map.stability_report(WEAKEST_PREVIEW_N),
                                    );
                                }
                                Err(e) => {
                                    tracing::warn!("DATA_DELETE failed: {e}");
                                    let _ = app_handle.emit(
                                        EVT_DATA_DELETED,
                                        serde_json::json!({ "ok": false, "message": e }),
                                    );
                                }
                            }
                        }
                    }
                }
                _ = watchdog.tick() => {
                    // Capture-health watchdog. Reads
                    // `last_heartbeat_at` to derive heartbeat
                    // staleness, transitions health state, attempts
                    // hard restart when Stopped, and re-emits current
                    // state every HEALTH_REPEAT_TICKS so panel
                    // reloads converge to truth without waiting for a
                    // transition.
                    watchdog_ticks = watchdog_ticks.wrapping_add(1);

                    // M3 bubble: drop a suggestion the user neither accepted (Shift)
                    // nor typed past within the timeout — the UI auto-fades on the
                    // same ~5s budget. Backstop for the pure-pause case; an active
                    // typist clears it eagerly on the next keystroke.
                    if let Some(ps) = &pending_suggestion {
                        if now_ms().saturating_sub(ps.armed_at_ms) > SUGGESTION_TIMEOUT_MS {
                            pending_suggestion = None;
                            let _ = app_handle.emit(EVT_CORRECTION_DISMISSED, ());
                        }
                    }

                    // C5a verdict state machine: Kept / Abandoned fire on
                    // elapsed idle, so the resolver must tick even when no
                    // keystroke arrives (the user paused after a gesture).
                    // The keystroke handlers tick it on edits; this is the
                    // idle driver. Cheap — walks the bounded ledger and
                    // recomputes per-record observations.
                    tick_resolver(
                        &app_handle,
                        &mut resolver,
                        &mut motor_resolver,
                        &anchors,
                        &line_buf,
                        caret,
                        &mut ledger,
                        &mut motor_ledger,
                        &mut proposer,
                        &mut motor_map,
                        &mut word_freq,
                        prompted_capture_active,
                        &mut word_patterns,
                        &mut guess_ledger,
                        &mut funnel,
                        &mut tally,
                        &mut competitor_sense,
                    );

                    // Watch Dog re-arm: zero stale competitor evidence once a
                    // full idle window has passed with no footprint (the host
                    // corrector went quiet), so a fresh competitor must clear the
                    // N-footprint threshold again. The 1s watchdog is the idle
                    // driver — `is_deferring`'s own time guard already lapses
                    // deferral at the window edge; this keeps the counter honest.
                    competitor_sense.decay(now_ms());

                    // Idle line-buffer reset (desync recovery). tick_resolver
                    // above already fired any due Kept/Abandoned verdicts on
                    // this line, so clearing now drops only a stale, fully-
                    // resolved buffer. The next keystroke starts a clean line —
                    // breaking any desync that left a residual tail to the
                    // right of the caret (the "edndd" cascade). Mirrors the
                    // newline reset; the buffer is transient (Principle #7).
                    if !line_buf.is_empty()
                        && Instant::now()
                            .duration_since(last_text_input_at)
                            .as_millis() as u64
                            >= LINE_IDLE_RESET_MS
                    {
                        tracing::info!(
                            "IDLE-LINE-RESET cleared {} chars after >= {}ms idle",
                            line_buf.len(),
                            LINE_IDLE_RESET_MS
                        );
                        tokenizer.reset_line();
                        line_buf.clear();
                        line_dwells.clear();
                        caret = 0;
                        anchors.clear();
                        let _ = app_handle.emit(EVT_LINE_RESET, ());
                        let snap = anchors.snapshot();
                        let _ = app_handle.emit(
                            EVT_ANCHOR_SNAPSHOT,
                            anchor_emit_payload(&snap, &line_buf),
                        );
                    }

                    // Component 5c: maintain the live motor-map file on a
                    // time cadence (not only every 100 obs / on shutdown,
                    // which under `tauri dev` may never run — the task is
                    // aborted at this await on app exit). Bounds force-quit
                    // loss to ~MOTOR_FLUSH_INTERVAL_MS of observations.
                    if flush_motor_map(
                        &mut motor_map,
                        motor_map_path.as_deref(),
                        now_ms(),
                        &mut last_motor_save_ms,
                    ) {
                        funnel.motor_saves += 1;
                    }

                    // C5d: same cadence for the word-pattern store's own file.
                    if flush_word_patterns(
                        &mut word_patterns,
                        word_patterns_path.as_deref(),
                        now_ms(),
                        &mut last_word_patterns_save_ms,
                    ) {
                        funnel.word_pattern_saves += 1;
                    }

                    // C5e: same cadence for the local vocabulary tally's own
                    // file. Observe-only — no funnel counter (it's a downstream
                    // tally, not a capture stage that can silently drop data).
                    flush_word_freq(
                        &mut word_freq,
                        word_freq_path.as_deref(),
                        now_ms(),
                        &mut last_word_freq_save_ms,
                    );

                    // Phase 1 guesser accuracy scoreboard: same cadence, its own
                    // file. Observe-only — measurement, no injection.
                    flush_guess_ledger(
                        &mut guess_ledger,
                        guess_accuracy_path.as_deref(),
                        now_ms(),
                        &mut last_guess_ledger_save_ms,
                    );

                    // Progress Statistics: persist today's tally on the same
                    // cadence and roll it at the calendar-day boundary. Watchdog
                    // runs every 1s, so even an idle day rolls over promptly.
                    tick_progress(
                        &mut tally,
                        progress_path.as_deref(),
                        now_ms(),
                        &mut last_progress_save_ms,
                    );

                    // Capture-integrity funnel (Principle #7): auto-dump
                    // every 60s (watchdog ticks every 1s) so the funnel is
                    // reconstructable from logs after the fact, in addition
                    // to the on-demand Cmd+Shift+F chord.
                    if watchdog_ticks % 60 == 0 {
                        funnel.dump();
                        // Read-only kill-switch observability (no injection).
                        dump_classifications(&word_patterns, &motor_map);
                        // Observe-only guesser accuracy readout (no injection).
                        dump_guess_accuracy(&guess_ledger);
                    }

                    // C5c motor stability: periodic read-model emit so a
                    // passive consumer (debug panel / future Practice) stays
                    // current. Skip while the map is empty (nothing to say).
                    if watchdog_ticks % MOTOR_STABILITY_EMIT_TICKS == 0 && !motor_map.is_empty() {
                        let report: StabilityReport =
                            motor_map.stability_report(WEAKEST_PREVIEW_N);
                        let _ = app_handle.emit(EVT_MOTOR_STABILITY, report);
                    }

                    // Compute desired state from heartbeat freshness.
                    let desired_health = match last_heartbeat_at {
                        // Still cold-start — no heartbeat yet. Leave
                        // current state (typically Unknown) alone.
                        None => current_capture_health,
                        Some(prev) => {
                            let elapsed_ms = Instant::now()
                                .duration_since(prev)
                                .as_millis();
                            if elapsed_ms >= HEARTBEAT_STOPPED_MS {
                                CaptureHealth::Stopped
                            } else if elapsed_ms >= HEARTBEAT_STALE_MS {
                                CaptureHealth::Unhealthy
                            } else {
                                // Fresh heartbeat. Don't override the
                                // heartbeat-driven state — that arm
                                // already set Live or Unhealthy
                                // correctly based on tap_enabled.
                                current_capture_health
                            }
                        }
                    };

                    let transitioned = transition_capture_health(
                        &app_handle,
                        &mut current_capture_health,
                        desired_health,
                    );

                    // Auto-respawn on Stopped. The respawn helper
                    // throttles via the attempt window so a crashing
                    // sidecar can't churn forever. If the cap is hit,
                    // we stay Stopped and wait for the user to click
                    // Restart capture (which clears the window).
                    if matches!(current_capture_health, CaptureHealth::Stopped)
                        && transitioned
                    {
                        if try_hard_restart(
                            &app_handle,
                            &mut rx,
                            &mut sidecar_child,
                            &mut respawn_attempts,
                        ) {
                            last_heartbeat_at = Some(Instant::now());
                        }
                    }

                    // Periodic re-emit so a freshly-mounted panel
                    // sees the current state within seconds even if
                    // no transition has fired since it mounted.
                    if watchdog_ticks % HEALTH_REPEAT_TICKS == 0 {
                        let _ = app_handle.emit(
                            EVT_CAPTURE_HEALTH,
                            CaptureHealthEvent { state: current_capture_health },
                        );
                    }

                    // ---- Debounced menu-bar "capture active?" signal. --------
                    // The raw health above flips on every transient blip; the
                    // menu-bar icon/status must NOT strobe, so we settle it: not
                    // active only once capture has been continuously non-Live
                    // past NOT_ACTIVE_DEBOUNCE_MS (the self-heal window). Going
                    // back to Live flips active true immediately — good news
                    // isn't debounced.
                    let live_now = matches!(current_capture_health, CaptureHealth::Live);
                    if live_now {
                        non_live_since = None;
                        ever_live = true;
                    } else if non_live_since.is_none() {
                        non_live_since = Some(Instant::now());
                    }
                    let settled_stopped = non_live_since.is_some_and(|since| {
                        Instant::now().duration_since(since).as_millis() >= NOT_ACTIVE_DEBOUNCE_MS
                    });
                    // (a) QA-20: a runtime Accessibility revoke leaves capture
                    // reading (Input Monitoring keeps the tap alive → health stays
                    // Live) but corrections can no longer inject. Surface it as a
                    // settled Stop so the tray slashes and the menu offers Reconnect
                    // (re-granting is the only recovery) — no debounce, since an AX
                    // revoke is a deliberate Settings action, not a transient blip.
                    // Only once capture has actually started (`ever_live`), so a
                    // pre-grant boot still reads as NotStarted, not a false alarm.
                    let ax_revoked = ever_live && !accessibility_ok;
                    // Active only from an observed Live (or a transient drop still
                    // inside the anti-strobe debounce). Never-Live-yet is
                    // NotStarted, not Active; a settled outage — or an AX revoke — is
                    // Stopped.
                    let ui_state = if ax_revoked {
                        CaptureUiState::Stopped
                    } else if live_now || (ever_live && !settled_stopped) {
                        CaptureUiState::Active
                    } else if settled_stopped {
                        CaptureUiState::Stopped
                    } else {
                        CaptureUiState::NotStarted
                    };

                    // Push the menu-bar icon: the capture-stopped slash ONLY for a
                    // settled Stop. NotStarted keeps the neutral hand (no slash
                    // flicker while a normal boot is still reaching its first
                    // heartbeat). No-op when unchanged.
                    apply_tray_icon(
                        &app_handle,
                        matches!(ui_state, CaptureUiState::Stopped),
                        &mut last_tray_icon,
                    );

                    // Emit the settled UI state on change, and periodically so a
                    // freshly-registered listener (the tray) converges. The
                    // recovery action only matters while Stopped.
                    let ui = CaptureUiEvent {
                        state: ui_state,
                        // Reconnect (not Restart) whenever a required grant is gone:
                        // the tap-dead case (`!permission_ok`) OR a runtime AX revoke
                        // that left the tap alive (`ax_revoked`, where permission_ok
                        // is still true). (a) QA-20.
                        permission_revoked: matches!(ui_state, CaptureUiState::Stopped)
                            && (!permission_ok || ax_revoked),
                    };
                    if last_ui_emit != Some(ui) || watchdog_ticks % HEALTH_REPEAT_TICKS == 0 {
                        let _ = app_handle.emit(EVT_CAPTURE_UI, ui);
                        last_ui_emit = Some(ui);
                    }

                    // Component 5c: daily motor-map snapshot (Principle #6).
                    // The watchdog ticks every 1s and is the only periodic timer,
                    // so it stands in for "the first event of a new calendar day":
                    // when the date rolls over from the one we last handled, write
                    // that day's snapshot. Dated files are NEVER overwritten — only
                    // written if absent — so snapshots accumulate. The in-memory
                    // date guard keeps this to a free compare on the common path
                    // (a stat only when the day actually changes). No-op when HOME
                    // is unset (snapshots_dir None).
                    if let Some(dir) = snapshots_dir.as_deref() {
                        let today = ymd_from_epoch_ms(now_ms());
                        if last_snapshot_date != Some(today) {
                            let path =
                                dir.join(format!("{:04}-{:02}-{:02}.json", today.0, today.1, today.2));
                            if !path.exists() {
                                match motor_map.write_snapshot(&path) {
                                    Ok(()) => tracing::info!("MOTOR_SNAPSHOT_DAILY path={path:?}"),
                                    Err(e) => tracing::warn!("motor map snapshot failed: {e}"),
                                }
                            }
                            last_snapshot_date = Some(today);
                        }
                    }

                    // C5e: daily word-tally snapshot (Principle #6), same
                    // calendar-roll logic as the motor map, its own directory +
                    // date guard. Never overwrites an existing dated file.
                    if let Some(dir) = word_freq_snapshots_dir.as_deref() {
                        let today = ymd_from_epoch_ms(now_ms());
                        if last_word_freq_snapshot_date != Some(today) {
                            let path =
                                dir.join(format!("{:04}-{:02}-{:02}.json", today.0, today.1, today.2));
                            if !path.exists() {
                                let _ = std::fs::create_dir_all(dir);
                                match word_freq.write_snapshot(&path) {
                                    Ok(()) => tracing::info!("WORD_FREQ_SNAPSHOT_DAILY path={path:?}"),
                                    Err(e) => tracing::warn!("word-freq snapshot failed: {e}"),
                                }
                            }
                            last_word_freq_snapshot_date = Some(today);
                        }
                    }
                }
            }
        }

        // ---- Sidecar lifetime guard — DO NOT REMOVE ---------------------
        //
        // Keep this drop here, at the END of the event loop. It is the
        // mechanism that ties the Swift sidecar's lifetime to this async
        // task's lifetime.
        //
        // `sidecar_child` owns the parent-side write-end of the sidecar's
        // stdin pipe. When it drops, that pipe closes; the sidecar's
        // input thread (`adapters/macos/.../Bridge.swift::runInputLoop`)
        // reads EOF on `stdin.availableData`, dispatches `.shutdown`,
        // and the sidecar exits cleanly via `EventTap::handleCommand`
        // → `exit(0)`. That EOF-as-shutdown path IS the intended
        // graceful teardown — firing it here when the loop ends (Tauri
        // quitting, panic unwind) means the sidecar dies with us
        // instead of leaking.
        //
        // **C5 commit O update.** The engine task now SURVIVES
        // CommandEvent::Terminated (it respawns the sidecar via
        // try_hard_restart). The loop end is no longer the ordinary
        // path — it's only reached on Tauri app shutdown / panic
        // unwind. The drop's job is unchanged, but now it ONLY runs
        // at app shutdown rather than mid-session. The respawn path
        // overwrites `sidecar_child` in place — the OLD CommandChild's
        // Drop runs at that point and closes the old stdin pipe (the
        // sidecar may already be dead, in which case the close is a
        // no-op). Net effect: NO orphaned sidecars on shutdown, NO
        // mid-session shutdown of a still-working sidecar.
        //
        // Why this can't move earlier or disappear:
        //   * Drop it before the loop runs (e.g. let it fall out of the
        //     outer `spawn()` function) → sidecar sees EOF instantly,
        //     `exit(0)` before any keystroke arrives, KEYS stays at 0
        //     forever. This was the 3c-2 regression — the binding
        //     looked unused, so it died with the outer function.
        //   * Replace with `let _ = sidecar_child;` or rename to
        //     `_sidecar_child` → same problem the moment a future
        //     refactor removes the "unused" line.
        //
        // If you're here to retire this drop, FIRST check that the
        // sidecar has another mechanism for staying alive — and read
        // `Bridge.swift` to understand the EOF=shutdown contract.
        drop(sidecar_child);

        // Component 5c: graceful-shutdown save. The 100-observation cadence
        // can leave a tail of recent observations uncommitted; flush them so
        // the session's learning survives app quit. Only reached on real
        // shutdown / panic unwind (the loop survives sidecar restarts).
        if let Some(path) = motor_map_path.as_deref() {
            match motor_map.save_to(path) {
                Ok(()) => tracing::info!(
                    "MOTOR_MAP_SAVED (shutdown) obs={} path={:?}",
                    motor_map.total_observations(),
                    path
                ),
                Err(e) => tracing::warn!("motor map shutdown save failed: {e}"),
            }
        }
        // C5d word-pattern store: same graceful-shutdown flush.
        if let Some(path) = word_patterns_path.as_deref() {
            match word_patterns.save_to(path) {
                Ok(()) => tracing::info!(
                    "WORD_PATTERNS_SAVED (shutdown) patterns={} obs={} path={:?}",
                    word_patterns.len(),
                    word_patterns.total_observations(),
                    path
                ),
                Err(e) => tracing::warn!("word-pattern store shutdown save failed: {e}"),
            }
        }
        // Guesser accuracy scoreboard (M3 Phase 1): same graceful-shutdown flush.
        if let Some(path) = guess_accuracy_path.as_deref() {
            match guess_ledger.save_to(path) {
                Ok(()) => tracing::info!(
                    "GUESS_LEDGER_SAVED (shutdown) patterns={} tries={} hits={} path={:?}",
                    guess_ledger.len(),
                    guess_ledger.overall().tries,
                    guess_ledger.overall().hits,
                    path
                ),
                Err(e) => tracing::warn!("guess-accuracy ledger shutdown save failed: {e}"),
            }
        }
        // Progress Statistics: flush today's tally too, so a clean quit commits
        // the tail of today's words/slips (best-effort — the watchdog's ~2s
        // cadence is the primary durability, since this may not run under
        // `tauri dev`). Forced by zeroing the save clock.
        if tally.dirty {
            last_progress_save_ms = 0;
            tick_progress(
                &mut tally,
                progress_path.as_deref(),
                now_ms(),
                &mut last_progress_save_ms,
            );
        }
    });

    Ok(control_tx)
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- QA-17: quote/punctuation strip + re-attach --------------------------

    /// Straight-quote matched-pair stripping (5a). Only a WRAPPING pair peels;
    /// one-sided apostrophes (contractions, possessives) stay put.
    #[test]
    fn strip_matched_quotes_only_peels_wrapping_pairs() {
        // Wrapping straight-quote pair → strip, remember both runs.
        assert_eq!(
            strip_matched_quotes("'waht'"),
            ("waht".into(), "'".into(), "'".into())
        );
        // One-sided → kept verbatim (possessive, leading contraction).
        assert_eq!(
            strip_matched_quotes("dogs'"),
            ("dogs'".into(), String::new(), String::new())
        );
        assert_eq!(
            strip_matched_quotes("'em"),
            ("'em".into(), String::new(), String::new())
        );
        assert_eq!(
            strip_matched_quotes("parents'"),
            ("parents'".into(), String::new(), String::new())
        );
        // Internal apostrophe → untouched (the core QA-17 promise).
        for w in ["don't", "it's", "user's", "well-known"] {
            assert_eq!(
                strip_matched_quotes(w),
                (w.into(), String::new(), String::new()),
                "internal apostrophe/hyphen must be preserved: {w}"
            );
        }
        // Accepted tradeoff: a fully-quoted contraction loses its outer quotes.
        assert_eq!(
            strip_matched_quotes("'twas'"),
            ("twas".into(), "'".into(), "'".into())
        );
        // Degenerate all-quote token → never strip to empty.
        assert_eq!(
            strip_matched_quotes("''"),
            ("''".into(), String::new(), String::new())
        );
    }

    /// Apply an injection (delete N from the caret, then type `replacement`) to
    /// the text-before-caret — the field model the accept/revert operate on.
    fn apply_inject(before_caret: &str, delete_count: u32, replacement: &str) -> String {
        let mut chars: Vec<char> = before_caret.chars().collect();
        for _ in 0..delete_count {
            chars.pop();
        }
        chars.extend(replacement.chars());
        chars.into_iter().collect()
    }

    /// Build the pending exactly as the fire path does (strip + re-attach), so a
    /// geometry test exercises the same assembly the loop uses.
    fn pending_for(core: &str, outer_trail: &str, target: &str, boundary: char) -> PendingSuggestion {
        let (inner, q_lead, q_trail) = strip_matched_quotes(core);
        PendingSuggestion {
            typed: inner,
            target: target.into(),
            boundary,
            word_len: core.chars().count(),
            word_end: core.chars().count(),
            lead: q_lead,
            trail: format!("{q_trail}{outer_trail}"),
            outer_trail_len: outer_trail.chars().count(),
            armed_at_ms: 0,
        }
    }

    fn last_correction_for(ps: &PendingSuggestion) -> LastCorrection {
        LastCorrection {
            typed: ps.typed.clone(),
            target: ps.target.clone(),
            boundary: ps.boundary,
            lead: ps.lead.clone(),
            trail: ps.trail.clone(),
            immediate: true,
            after_len: ps.outer_trail_len + 1,
            fired_at_ms: 0,
        }
    }

    /// Accept then revert restores the original — for every QA-17 shape. `field`
    /// is the text before the caret at accept time (word + closing punct +
    /// boundary; any LEADING punct sits further back and is never touched).
    #[track_caller]
    fn assert_accept_revert(core: &str, outer_trail: &str, target: &str, field: &str, fixed: &str) {
        let ps = pending_for(core, outer_trail, target, ' ');
        let (dc, repl) = immediate_accept_injection(&ps);
        let after = apply_inject(field, dc, &repl);
        assert_eq!(after, fixed, "accept: {core:?}+{outer_trail:?} → {target:?}");
        let lc = last_correction_for(&ps);
        let (dc2, repl2) = immediate_revert_injection(&lc);
        let restored = apply_inject(&after, dc2, &repl2);
        assert_eq!(restored, field, "revert must restore the original: {core:?}");
    }

    #[test]
    fn accept_revert_plain_word_unchanged_geometry() {
        // No surrounding punctuation → old word+boundary / target+boundary.
        assert_accept_revert("teh", "", "the", "teh ", "the ");
    }

    #[test]
    fn accept_revert_single_quoted_typo_rewraps() {
        // 5a: straight quotes glued into the core → deleted with it, re-typed
        // around the target. (Without the fix this never fired at all.)
        assert_accept_revert("'waht'", "", "what", "'waht' ", "'what' ");
    }

    #[test]
    fn accept_revert_double_quoted_typo_keeps_trailing_quote() {
        // 5b: the trailing `"` sits beyond word_end (in tok.trailing). The
        // leading `"` is never in the delete window, so it survives untouched.
        assert_accept_revert("beleive", "\"", "believe", "\"beleive\" ", "\"believe\" ");
    }

    #[test]
    fn pending_survives_the_seal_of_a_trailing_quoted_word() {
        // QA-17 5b regression: the end-of-arm retention gate must keep a bubble
        // whose word carries a trailing `"` (caret rests 2 past word_end), not
        // just a bare word (rests 1 past). Anchored OFF for all of these.
        let anchored = false;
        // Plain word: caret one past the word → survives.
        assert!(pending_survives(true, 11, 10, 0, anchored));
        // Trailing double-quote: caret two past (word + `"` + boundary) → survives
        // (this is the exact case that regressed to "no bubble at all").
        assert!(pending_survives(true, 12, 10, 1, anchored));
        // Caret typed genuinely PAST the rest position → dropped (a plain word,
        // caret 2 past with no trailing punct, is a real move-on).
        assert!(!pending_survives(true, 12, 10, 0, anchored));
        // Not a clean forward keystroke (nav/edit) → dropped regardless.
        assert!(!pending_survives(false, 11, 10, 0, anchored));
        // Caret at or before the word → dropped.
        assert!(!pending_survives(true, 10, 10, 0, anchored));
        // Anchored ON keeps it up to the full cap, well past the rest position.
        assert!(pending_survives(true, 15, 10, 0, true));
    }

    #[test]
    fn quoted_word_rest_position_is_immediate() {
        // Regression for the real 5b bug: a trailing quote makes the caret rest
        // at outer_trail_len + 1 (= 2), which MUST read as immediate — not as an
        // "anchored" accept that the disabled path silently declines.
        let ps = pending_for("beleive", "\"", "believe", ' ');
        let rest_after_len = ps.outer_trail_len + 1;
        assert_eq!(rest_after_len, 2, "word + trailing quote rests two past word_end");
        // Plain word still rests at 1.
        let plain = pending_for("teh", "", "the", ' ');
        assert_eq!(plain.outer_trail_len + 1, 1);
    }

    // ---- QA-15 space-drop detector (Step 1, observe-only) ----------------

    #[test]
    fn space_reconciler_clean_stream_never_flags() {
        // Every observed space keyDown is followed by its accepted Key — no drop.
        let mut f = Funnel::new(0);
        for n in 1..=10u64 {
            assert_eq!(f.note_space_observed(n), None, "clean space {n} must not flag");
            f.note_space_accepted();
        }
        assert_eq!(f.space_drops_suspected, 0);
    }

    #[test]
    fn space_reconciler_flags_a_dropped_space_one_press_late() {
        // keyDowns 1..4 observed; the text key for space 3 is LOST (no accept).
        // The drop is provable only at the next keyDown, when the in-flight
        // tolerance no longer hides it.
        let mut f = Funnel::new(0);
        assert_eq!(f.note_space_observed(1), None);
        f.note_space_accepted();
        assert_eq!(f.note_space_observed(2), None);
        f.note_space_accepted();
        assert_eq!(f.note_space_observed(3), None); // space 3 keyDown; its char is dropped
        assert_eq!(f.note_space_observed(4), Some(1)); // now space 3's loss is evident
        f.note_space_accepted(); // space 4's char arrives
        assert_eq!(f.space_drops_suspected, 1);
    }

    #[test]
    fn space_reconciler_counts_two_separate_drops_and_not_in_flight() {
        // A single in-flight space (keyDown seen, keyUp pending) must never flag;
        // two genuinely dropped spaces must count as two.
        let mut f = Funnel::new(0);
        f.note_space_observed(1);
        f.note_space_accepted();
        f.note_space_observed(2);
        f.note_space_accepted();
        f.note_space_observed(3); // dropped (no accept)
        assert_eq!(f.note_space_observed(4), Some(1)); // drop #1 surfaces
        f.note_space_accepted();
        f.note_space_observed(5); // dropped (no accept)
        assert_eq!(f.note_space_observed(6), Some(2)); // drop #2 surfaces
        f.note_space_accepted();
        assert_eq!(f.space_drops_suspected, 2);
        // A trailing in-flight space (observed but not yet accepted) does not flag.
        assert_eq!(f.note_space_observed(7), None);
    }

    #[test]
    fn space_reconciler_tolerates_a_lost_observed_marker() {
        // If a SpaceObserved marker itself is lost, an accept can momentarily lead
        // observed; saturating arithmetic must not panic or under/over-flag, and
        // the next (cumulative) total re-syncs.
        let mut f = Funnel::new(0);
        f.note_space_accepted(); // accepted before any observed (marker lost)
        assert_eq!(f.note_space_observed(1), None); // re-sync, no false flag
        assert_eq!(f.space_drops_suspected, 0);
    }

    // ---- Watch Dog: competitor-sense state machine -----------------------

    #[test]
    fn competitor_sense_defers_after_threshold_and_rearms() {
        let mut s = CompetitorSense::default();
        // Armed (firing) until the threshold is cleared.
        assert!(!s.is_deferring(1_000));
        s.note_footprint(1_000);
        assert!(!s.is_deferring(1_000), "one footprint is below N=2");
        s.note_footprint(2_000);
        assert!(s.is_deferring(2_000), "two footprints -> defer common lane");

        // Stays deferring while footprints are recent.
        assert!(s.is_deferring(2_000 + COMPETITOR_REARM_IDLE_MS - 1));
        // The instant the idle window passes, deferral lapses (fire again).
        assert!(!s.is_deferring(2_000 + COMPETITOR_REARM_IDLE_MS));

        // Watchdog decay zeroes stale evidence, so re-arm needs N fresh
        // footprints again (not just one riding on a stale count).
        s.decay(2_000 + COMPETITOR_REARM_IDLE_MS);
        s.note_footprint(100_000);
        assert!(
            !s.is_deferring(100_000),
            "single fresh footprint must not re-defer"
        );
        s.note_footprint(101_000);
        assert!(s.is_deferring(101_000));

        // Focus/app change hard-resets immediately.
        s.reset();
        assert!(!s.is_deferring(101_000));
    }

    // ---- M3 bubble: casing preservation + contraction filter -------------

    #[test]
    fn match_source_case_preserves_user_casing() {
        assert_eq!(match_source_case("teh", "the"), "the");
        assert_eq!(match_source_case("Teh", "the"), "The");
        assert_eq!(match_source_case("WAHT", "what"), "WHAT");
        // Single leading capital, not all-caps.
        assert_eq!(match_source_case("Recieve", "receive"), "Receive");
        // A one-letter capital isn't "all caps" — leading-cap branch.
        assert_eq!(match_source_case("I", "i"), "I");
    }

    #[test]
    fn apostrophe_filter_suppresses_contractions_not_motor_garbles() {
        // Pure apostrophe insertion (Auto-Correct's job) — suppressed.
        assert!(is_apostrophe_fix("todays", "today's"));
        assert!(is_apostrophe_fix("dont", "don't"));
        // Possessive completion after a typed apostrophe — suppressed.
        assert!(is_apostrophe_fix("key'", "key's"));
        assert!(is_apostrophe_fix("One'", "one's"));
        // Genuine MOTOR garble in an apostrophe word — NOT suppressed.
        assert!(!is_apostrophe_fix("doens't", "doesn't"));
        // No apostrophe in the target — never the filter's business.
        assert!(!is_apostrophe_fix("teh", "the"));
        assert!(!is_apostrophe_fix("haev", "have"));
    }

    // ---- Component 5c snapshot date helpers ------------------------------

    const DAY_MS: u64 = 86_400_000;

    #[test]
    fn ymd_from_epoch_known_dates() {
        assert_eq!(ymd_from_epoch_ms(0), (1970, 1, 1));
        // 2026-05-30 (today, per the build context) — midnight UTC.
        let ms = epoch_ms_from_ymd(2026, 5, 30);
        assert_eq!(ymd_from_epoch_ms(ms), (2026, 5, 30));
        // A leap day round-trips.
        assert_eq!(
            ymd_from_epoch_ms(epoch_ms_from_ymd(2024, 2, 29)),
            (2024, 2, 29)
        );
    }

    #[test]
    fn snapshot_date_round_trips_through_filename() {
        for &(y, m, d) in &[(1970, 1, 1), (2026, 5, 30), (2024, 2, 29), (1999, 12, 31)] {
            let ms = epoch_ms_from_ymd(y, m, d);
            let name = format!("{}.json", snapshot_date(ms));
            assert_eq!(parse_snapshot_date(&name), Some((y, m, d)));
        }
    }

    #[test]
    fn ymd_ignores_intraday_time() {
        // Any time within a day maps to that day's date.
        let base = epoch_ms_from_ymd(2026, 5, 30);
        assert_eq!(ymd_from_epoch_ms(base + DAY_MS - 1), (2026, 5, 30));
        assert_eq!(ymd_from_epoch_ms(base + DAY_MS), (2026, 5, 31));
    }

    #[test]
    fn parse_snapshot_date_rejects_non_snapshots() {
        assert_eq!(parse_snapshot_date("motor_map.json"), None);
        assert_eq!(parse_snapshot_date("2026-13-01.json"), None); // bad month
        assert_eq!(parse_snapshot_date("2026-05-30.txt"), None); // wrong ext
        assert_eq!(parse_snapshot_date("2026-05-30-extra.json"), None); // extra part
        assert_eq!(parse_snapshot_date("not-a-date.json"), None);
    }

    // ---- Each of the four nav keys ---------------------------------------

    #[test]
    fn left_moves_caret_back_one() {
        // Both forms macOS may emit (C0 cursor control and AppKit PU).
        assert_eq!(nav_action(KEY_C0_LEFT, 5, 10, false), Some(4));
        assert_eq!(nav_action(KEY_NS_LEFT, 5, 10, false), Some(4));
    }

    #[test]
    fn left_at_start_saturates_at_zero() {
        assert_eq!(nav_action(KEY_C0_LEFT, 0, 10, false), Some(0));
    }

    #[test]
    fn right_moves_caret_forward_one() {
        assert_eq!(nav_action(KEY_C0_RIGHT, 5, 10, false), Some(6));
        assert_eq!(nav_action(KEY_NS_RIGHT, 5, 10, false), Some(6));
    }

    #[test]
    fn right_at_end_clamps_at_line_len() {
        assert_eq!(nav_action(KEY_C0_RIGHT, 10, 10, false), Some(10));
    }

    #[test]
    fn home_moves_caret_to_zero() {
        // External-keyboard Home key (NSHomeFunctionKey).
        assert_eq!(nav_action(KEY_NS_HOME, 7, 10, false), Some(0));
        assert_eq!(nav_action(KEY_NS_HOME, 0, 10, false), Some(0));
    }

    #[test]
    fn end_moves_caret_to_line_len() {
        // External-keyboard End key (NSEndFunctionKey).
        assert_eq!(nav_action(KEY_NS_END, 3, 10, false), Some(10));
        assert_eq!(nav_action(KEY_NS_END, 10, 10, false), Some(10));
        assert_eq!(nav_action(KEY_NS_END, 0, 0, false), Some(0));
    }

    #[test]
    fn cmd_left_is_home_on_macos() {
        // The actual MacBook gesture: Cmd+Left = Home. Diagnosed from
        // the live "key in:" log — every Home press arrives as the Left
        // arrow codepoint with the Command modifier set.
        assert_eq!(nav_action(KEY_C0_LEFT, 7, 10, true), Some(0));
        assert_eq!(nav_action(KEY_NS_LEFT, 7, 10, true), Some(0));
    }

    #[test]
    fn cmd_right_is_end_on_macos() {
        assert_eq!(nav_action(KEY_C0_RIGHT, 3, 10, true), Some(10));
        assert_eq!(nav_action(KEY_NS_RIGHT, 3, 10, true), Some(10));
    }

    // ---- Cross-key regression guards --------------------------------------

    #[test]
    fn plain_arrows_never_jump_to_zero_unless_already_there() {
        // Regression: previously the Fn-modifier fallback routed Left to
        // Home. Plain Left from any non-zero caret must decrement by
        // exactly 1, regardless of any other state.
        for caret in 1..20 {
            assert_eq!(nav_action(KEY_C0_LEFT, caret, 30, false), Some(caret - 1));
            assert_eq!(nav_action(KEY_NS_LEFT, caret, 30, false), Some(caret - 1));
        }
    }

    #[test]
    fn plain_arrows_and_home_end_are_distinct_outcomes() {
        // Plain Left vs Cmd+Left at the same caret give different results
        // (except at caret=0). Same for Right vs Cmd+Right at line_len.
        assert_eq!(nav_action(KEY_C0_LEFT, 5, 10, false), Some(4));
        assert_eq!(nav_action(KEY_C0_LEFT, 5, 10, true), Some(0));
        assert_ne!(
            nav_action(KEY_C0_LEFT, 5, 10, false),
            nav_action(KEY_C0_LEFT, 5, 10, true)
        );
    }

    #[test]
    fn unmapped_function_key_returns_none() {
        // Up/Down arrows are PU codepoints we filter but don't map — they
        // must return None so the caller no-ops, NOT default to 0 or len.
        assert_eq!(nav_action(KEY_C0_UP, 5, 10, false), None);
        assert_eq!(nav_action(KEY_C0_DOWN, 5, 10, false), None);
        assert_eq!(nav_action(KEY_NS_UP, 5, 10, false), None);
        assert_eq!(nav_action(KEY_NS_DOWN, 5, 10, false), None);
        // Random unmapped PU codepoint (e.g. some F-key).
        assert_eq!(nav_action('\u{F710}', 5, 10, false), None);
        // Cmd modifier doesn't rescue an unmapped codepoint.
        assert_eq!(nav_action('\u{F710}', 5, 10, true), None);
    }

    // ---- Filter --------------------------------------------------------

    #[test]
    fn is_non_text_filters_the_full_pu_function_range() {
        // Every codepoint in U+F700..=U+F8FF must be non-text.
        assert!(is_non_text_key('\u{F700}'));
        assert!(is_non_text_key('\u{F702}'));
        assert!(is_non_text_key('\u{F729}'));
        assert!(is_non_text_key('\u{F72B}'));
        assert!(is_non_text_key('\u{F8FF}'));
    }

    #[test]
    fn is_non_text_filters_c0_cursor_codes() {
        // The actual codepoints macOS emits for arrows on US English.
        assert!(is_non_text_key('\u{001C}'));
        assert!(is_non_text_key('\u{001D}'));
        assert!(is_non_text_key('\u{001E}'));
        assert!(is_non_text_key('\u{001F}'));
    }

    #[test]
    fn is_non_text_allows_text_chars() {
        assert!(!is_non_text_key('a'));
        assert!(!is_non_text_key('Z'));
        assert!(!is_non_text_key('!'));
        assert!(!is_non_text_key(' '));
        assert!(!is_non_text_key('3'));
    }

    #[test]
    fn is_non_text_keeps_newline_tab_cr_as_text() {
        // These are control codepoints but the engine handles them upstream
        // — newline triggers a line reset, tab is a tokenizer whitespace
        // boundary. They must NOT be filtered out.
        assert!(!is_non_text_key('\n'));
        assert!(!is_non_text_key('\r'));
        assert!(!is_non_text_key('\t'));
    }

    #[test]
    fn insert_text_char_rejects_pu_and_c0() {
        // Defensive guard at the single line_buf entry point: any non-text
        // codepoint that somehow reaches here is rejected and the buffer
        // is unchanged.
        let mut line: Vec<char> = vec!['a', 'b', 'c'];
        assert_eq!(insert_text_char(&mut line, 1, '\u{F702}'), None);
        assert_eq!(line, vec!['a', 'b', 'c']);
        assert_eq!(insert_text_char(&mut line, 1, '\u{001C}'), None);
        assert_eq!(line, vec!['a', 'b', 'c']);
        // Plain text still works.
        assert_eq!(insert_text_char(&mut line, 1, 'X'), Some(2));
        assert_eq!(line, vec!['a', 'X', 'b', 'c']);
    }

    // ---- Lexicon row payload — v2 membership/frequency split -----------

    #[test]
    fn lexicon_row_known_comes_from_is_known_not_frequency() {
        // Regression: emit_sealed_token once computed `known = frequency
        // > 0`, which under lexicon v2 admits Norvig web typos. The
        // following must hold:
        //   - `teh`: in Norvig (count > 0) but NOT in SCOWL → known=false
        //   - `recieve`: same shape — Norvig has it, SCOWL doesn't
        //   - the freq field still carries the honest count (the engine
        //     reports the data; the panel suppresses display)
        let lex = correction_engine::Lexicon::shared();

        let row = lexicon_row_for("teh", lex);
        assert!(
            !row.known,
            "teh must be known=false even though Norvig has a count"
        );
        assert!(
            row.frequency > 0,
            "test premise: teh has a Norvig count — pin so this regression test isn't toothless"
        );

        let row = lexicon_row_for("recieve", lex);
        assert!(!row.known, "recieve must be known=false");
    }

    #[test]
    fn lexicon_row_known_words_keep_their_freq() {
        let lex = correction_engine::Lexicon::shared();
        for w in &["the", "because", "it", "should"] {
            let row = lexicon_row_for(w, lex);
            assert!(row.known, "{w} should be known");
            assert!(row.frequency > 0, "{w} should have a Norvig frequency");
        }
    }

    #[test]
    fn lexicon_row_contractions_are_known() {
        // The other v2 fix — SCOWL contractions list includes these, so
        // the engine never flags them as needing correction.
        let lex = correction_engine::Lexicon::shared();
        for w in &["didn't", "can't", "should've", "they're"] {
            let row = lexicon_row_for(w, lex);
            assert!(row.known, "contraction {w:?} should be known");
        }
    }

    #[test]
    fn lexicon_row_seed_proper_nouns_are_known() {
        let lex = correction_engine::Lexicon::shared();
        for w in &["Krutrim", "ZAMS", "ONDC"] {
            let row = lexicon_row_for(w, lex);
            assert!(row.known, "seed {w} should be known (case-insensitive)");
        }
    }

    #[test]
    fn lexicon_row_nonsense_is_unknown_with_zero_freq() {
        let lex = correction_engine::Lexicon::shared();
        for w in &["asdfqwerty", "qzxjvk"] {
            let row = lexicon_row_for(w, lex);
            assert!(!row.known);
            assert_eq!(row.frequency, 0);
        }
    }

    #[test]
    fn lexicon_row_carries_current_version() {
        let lex = correction_engine::Lexicon::shared();
        let row = lexicon_row_for("the", lex);
        assert_eq!(row.lexicon_version, correction_engine::LEXICON_VERSION);
    }

    // ---- Progress daily stats (Statistics tab) ---------------------------

    /// A throwaway temp dir for one test, removed on drop. Unique per call so
    /// parallel tests don't collide (no `tempfile` dev-dep needed).
    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "ta-progress-{}-{}-{}",
                tag,
                std::process::id(),
                n
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }
        fn file(&self) -> PathBuf {
            self.0.join("progress_snapshots.json")
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn daily_tally_slip_split_sums_to_slips() {
        let mut t = DailyTally::new((2026, 6, 3));
        t.add_word();
        t.add_word();
        t.add_word();
        t.add_slip(SlipClass::Coordination);
        t.add_slip(SlipClass::Precision);
        assert_eq!(t.words, 3);
        assert_eq!(t.slips, 2);
        // The invariant the Statistics tab relies on: coord + precis == slips
        // (so coord% + precis% == slip rate).
        assert_eq!(t.coord + t.precis, t.slips);
        assert!(t.dirty);
    }

    #[test]
    fn write_progress_upserts_today_and_preserves_past() {
        let scratch = ScratchDir::new("upsert");
        let path = scratch.file();

        // Day 1 lands.
        let mut t = DailyTally::new((2026, 6, 1));
        t.add_word();
        write_progress(&path, &t).unwrap();

        // Day 2 starts; updating it must not touch day 1.
        let mut t2 = DailyTally::new((2026, 6, 2));
        t2.add_word();
        write_progress(&path, &t2).unwrap();
        // ... and updating day 2 again REPLACES its row (not append).
        t2.add_word();
        write_progress(&path, &t2).unwrap();

        let days = read_progress_days(&path);
        assert_eq!(days.len(), 2, "two distinct days, no duplicate rows");
        assert_eq!(days[0].date, "2026-06-01");
        assert_eq!(days[0].words, 1, "past day untouched");
        assert_eq!(days[1].date, "2026-06-02");
        assert_eq!(days[1].words, 2, "today's row replaced, not appended");
    }

    #[test]
    fn load_daily_tally_resumes_today() {
        let scratch = ScratchDir::new("resume");
        let path = scratch.file();
        let now = epoch_ms_from_ymd(2026, 6, 3) + 5_000;

        let mut t = DailyTally::new(ymd_from_epoch_ms(now));
        t.add_word();
        t.add_word();
        t.add_slip(SlipClass::Precision);
        write_progress(&path, &t).unwrap();

        // A "restart" mid-day must resume today's counts, not zero them.
        let resumed = load_daily_tally(Some(&path), now);
        assert_eq!(resumed.date, (2026, 6, 3));
        assert_eq!(resumed.words, 2);
        assert_eq!(resumed.slips, 1);
        assert_eq!(resumed.precis, 1);
        assert!(!resumed.dirty, "a freshly loaded tally is clean");
    }

    #[test]
    fn load_daily_tally_ignores_a_different_day() {
        let scratch = ScratchDir::new("otherday");
        let path = scratch.file();

        let mut yesterday = DailyTally::new((2026, 6, 2));
        yesterday.add_word();
        write_progress(&path, &yesterday).unwrap();

        // Loading on the 3rd starts today fresh (yesterday's row stays on disk).
        let now = epoch_ms_from_ymd(2026, 6, 3) + 1_000;
        let today = load_daily_tally(Some(&path), now);
        assert_eq!(today.date, (2026, 6, 3));
        assert_eq!(today.words, 0);
    }

    #[test]
    fn tick_progress_rolls_the_day() {
        let scratch = ScratchDir::new("roll");
        let path = scratch.file();
        let mut last_save = 0u64;

        // A tally still on the 2nd, with data, ticked with a "now" on the 3rd:
        // the old day must be flushed and the tally reset to the new day.
        let mut t = DailyTally::new((2026, 6, 2));
        t.add_word();
        t.add_slip(SlipClass::Coordination);
        let now = epoch_ms_from_ymd(2026, 6, 3) + 1_000;
        let wrote = tick_progress(&mut t, Some(&path), now, &mut last_save);
        assert!(wrote);
        assert_eq!(t.date, (2026, 6, 3), "tally rolled to the new day");
        assert_eq!(t.words, 0, "new day starts clean");

        let days = read_progress_days(&path);
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].date, "2026-06-02");
        assert_eq!(days[0].words, 1, "the completed day was persisted on roll");
        assert_eq!(days[0].coord, 1);
    }

    #[test]
    fn tick_progress_skips_empty_day_rows() {
        let scratch = ScratchDir::new("empty");
        let path = scratch.file();
        let mut last_save = 0u64;

        // An idle day (no words) rolling over must NOT write a zero row.
        let mut t = DailyTally::new((2026, 6, 2));
        let now = epoch_ms_from_ymd(2026, 6, 3) + 1_000;
        tick_progress(&mut t, Some(&path), now, &mut last_save);
        assert_eq!(t.date, (2026, 6, 3));
        assert!(
            read_progress_days(&path).is_empty(),
            "empty days never clutter the history"
        );
    }
}
