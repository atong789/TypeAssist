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
    PatternReadiness, ScoredCandidate, SlipClass, StabilityReport, Token, TokenKind, Tokenizer,
    WordFreq, WordPatternStore, ACTIVE_TIER, CANDIDATES_VERSION, DECISION_VERSION, LEXICON_VERSION,
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
/// **M3 correction Step 1.** A live correction was just injected — fired once
/// per applied fix so the HUD cue can show `typed → target` briefly. Payload is
/// [`CorrectionAppliedEvent`]. (Principle #7: every correction is observable.)
pub const EVT_CORRECTION_APPLIED: &str = "corrections://applied";

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

/// Payload for [`EVT_CAPTURE_UI`] — the debounced, menu-bar-facing view of
/// capture. `active` is the settled state (see the event's doc); when it's
/// false, `permission_revoked` chooses the recovery action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct CaptureUiEvent {
    active: bool,
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
    /// **M3 correction Step 1 — per-pattern toggle.** Enable (add) or disable
    /// (remove) one `typed → target` pattern in the allow-list, from the panel.
    /// Persists + echoes [`EVT_CORRECTION_STATE`].
    SetPatternEnabled {
        typed: String,
        target: String,
        enabled: bool,
    },
    /// **M3 correction Step 1.** Ask the engine to emit the current allow-list
    /// on [`EVT_CORRECTION_STATE`] — the panel/tray pull fresh state on open.
    RequestAllowList,
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
/// now-stale caret. `trigger` is a log tag only (`mouse` / `focus` / `updown`).
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

/// Payload for [`EVT_CORRECTION_APPLIED`] — what the HUD cue shows. Carries the
/// before/after words and whether this event is a fix or its undo, so one cue
/// component renders both (`teh → the` on apply, `the → teh` reverting on undo).
#[derive(Serialize, Clone)]
struct CorrectionAppliedEvent {
    typed: String,
    target: String,
    /// `false` for an injected fix, `true` for an Escape teach-stop revert.
    undo: bool,
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
    fired_at_ms: u64,
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

    /// Emit the structured funnel line to the log. Same output for both
    /// callers (the Cmd+Shift+F chord and the 60s auto-dump).
    fn dump(&self) {
        tracing::info!(
            "FUNNEL_DUMP {{ c_keystrokes_received: {}, c_keystrokes_accepted: {}, \
             c_tokens_sealed: {}, c_records_admitted: {}, c_verdicts_resolved: {{kept: {}, \
             corr_sug: {}, corr_oth: {}, abandoned: {}}}, c_motor_observations: {{kept: {}, \
             slip: {}}}, c_motor_saves: {}, c_word_patterns: {{observed: {}, skipped: {}}}, \
             c_word_pattern_saves: {}, c_corrections: {{applied: {}, undone: {}}}, \
             session_started_at: {} }}",
            self.keystrokes_received,
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

/// One calendar day's typing rollup, as persisted. `date` is `YYYY-MM-DD` (UTC
/// civil date, matching the dated motor snapshots). `coord + precis == slips`
/// always (every counted slip classifies as exactly one).
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
    let mut tally = DailyTally::new(ymd_from_epoch_ms(now));
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
    let today = ymd_from_epoch_ms(now);

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

/// Minimum slip rate for a key to count as "worth practicing" and light the
/// tray dot. The dot reflects MOTOR-MAP STATE (is there a genuinely weak key?),
/// not "did the user practice today" — so it keys off the worst slip rate, not
/// merely whether any key has enough samples. **Tunable.** Starting at 5%: a
/// clean typist (e.g. all keys <1% slip) shows no dot, which is honest; a
/// recovering hand with a 20–40% slip on a slow finger lights it clearly.
const PRACTICE_DOT_SLIP_THRESHOLD: f32 = 0.05;

/// Whether there's a weak key "worth practicing" — drives the menu-bar badge
/// dot. `weakest` is sorted worst-first, so the head is the highest slip rate;
/// light the dot only when that clears the bar.
fn wants_practice_dot(report: &StabilityReport) -> bool {
    report
        .weakest
        .first()
        .is_some_and(|(_, slip_rate)| *slip_rate >= PRACTICE_DOT_SLIP_THRESHOLD)
}

/// The menu-bar "capture stopped" icon: a hollow version of the keyboard glyph
/// (rounded-square outline, no filled centre) with a diagonal slash through it —
/// the universal "off" look (like wifi-off). Built in memory as a **template**
/// (alpha-only; macOS recolours it for the light/dark bar), so the alarm reads
/// by SHAPE, never colour — NEVER red (a11y + the no-deficit-framing rule).
/// 44×44 to match `tray-icon.png`, so swapping it in doesn't resize the icon.
fn capture_off_icon() -> tauri::image::Image<'static> {
    const N: i32 = 44;
    let n = N as f32;
    // Rounded-square outline, matching the base glyph's bounding box + radius.
    let margin = 8.0;
    let half = (n - 2.0 * margin) / 2.0; // half side of the square
    let cx = n / 2.0;
    let cy = n / 2.0;
    let corner = 8.0;
    let ring = 4.0; // outline stroke width
                    // Diagonal slash, top-right → bottom-left (the "no/off" diagonal).
    let inset = margin - 1.0;
    let (ax, ay) = (n - inset, inset); // top-right
    let (bx, by) = (inset, n - inset); // bottom-left
    let slash = 4.5; // slash stroke width

    // Signed distance to a rounded rectangle centred at (cx,cy).
    let rrect_sdf = |px: f32, py: f32| -> f32 {
        let qx = (px - cx).abs() - (half - corner);
        let qy = (py - cy).abs() - (half - corner);
        let ax = qx.max(0.0);
        let ay = qy.max(0.0);
        (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - corner
    };
    // Distance from a point to the slash segment A→B.
    let seg_dist = |px: f32, py: f32| -> f32 {
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 {
            (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (qx, qy) = (ax + t * dx, ay + t * dy);
        ((px - qx).powi(2) + (py - qy).powi(2)).sqrt()
    };

    let mut rgba = vec![0u8; (N * N * 4) as usize];
    for y in 0..N {
        for x in 0..N {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            // Outline coverage: within half a stroke of the rounded-rect edge.
            let ring_cov = (1.0 - (rrect_sdf(px, py).abs() - ring / 2.0)).clamp(0.0, 1.0);
            // Slash coverage: within half a stroke of the segment.
            let slash_cov = (1.0 - (seg_dist(px, py) - slash / 2.0)).clamp(0.0, 1.0);
            let cov = ring_cov.max(slash_cov);
            let i = ((y * N + x) * 4) as usize;
            // Template: opaque black, alpha carries the shape.
            rgba[i + 3] = (cov * 255.0) as u8;
        }
    }
    tauri::image::Image::new_owned(rgba, N as u32, N as u32)
}

/// Set the menu-bar icon from the two orthogonal signals, only touching the OS
/// when the rendered state changes (tracked via `last`). The capture-stopped
/// alarm WINS over the practice badge — a dead capture is the more urgent thing
/// to show, and stacking both would muddy the glyph. All three are **template**
/// images (monochrome, OS-recoloured) so the signal is shape, never colour.
fn apply_tray_icon<R: Runtime>(
    app: &AppHandle<R>,
    not_active: bool,
    practice_dot: bool,
    last: &mut Option<(bool, bool)>,
) {
    let key = (not_active, practice_dot);
    if *last == Some(key) {
        return;
    }
    if let Some(tray) = app.tray_by_id("main-tray") {
        let icon = if not_active {
            capture_off_icon()
        } else if practice_dot {
            tauri::include_image!("icons/tray-icon-dot.png")
        } else {
            tauri::include_image!("icons/tray-icon.png")
        };
        let _ = tray.set_icon(Some(icon));
        let _ = tray.set_icon_as_template(true);
        *last = Some(key);
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
                                    // accrues (e.g. the flip to Tier1Ready when
                                    // weight crosses the threshold). No injection.
                                    let readiness = correction_engine::classify(
                                        &rec.original_text,
                                        c,
                                        word_patterns,
                                        Lexicon::shared(),
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

/// Read-only (kill-switch OFF): classify every learned pattern and log a
/// summary, so the M3 decision is observable against real accumulating data
/// BEFORE anything is ever injected. Counts by readiness and lists the
/// patterns that WOULD act (Tier-1) or suggest (Tier-2), with their reason and
/// decayed weight. Emitted alongside the 60s funnel dump. No injection — this
/// only surfaces what the classifier *would* decide. No-op on an empty store.
fn dump_classifications(store: &WordPatternStore) {
    if store.is_empty() {
        return;
    }
    let lexicon = Lexicon::shared();
    let (mut tier1, mut tier2, mut silent) = (0u32, 0u32, 0u32);
    let mut actionable: Vec<String> = Vec::new();
    for snap in store.snapshots() {
        match correction_engine::classify(&snap.typed, &snap.target, store, lexicon) {
            PatternReadiness::Tier1Ready => {
                tier1 += 1;
                actionable.push(format!(
                    "{}→{} TIER1 w={:.1}",
                    snap.typed, snap.target, snap.weight
                ));
            }
            PatternReadiness::Tier2Only { reason } => {
                tier2 += 1;
                actionable.push(format!(
                    "{}→{} TIER2({:?}) w={:.1}",
                    snap.typed, snap.target, reason, snap.weight
                ));
            }
            PatternReadiness::Silent { .. } => silent += 1,
        }
    }
    // debug!, not info!: `actionable` lists raw typed→target word pairs (privacy).
    // Counts live in FUNNEL_DUMP (text-free), which stays at info.
    tracing::debug!(
        "KILL_SWITCH_DUMP patterns={} tier1={} tier2={} silent={} actionable={:?}",
        store.len(),
        tier1,
        tier2,
        silent,
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
/// Heartbeat ≥ this many ms old → Stopped + auto-respawn. 15s = 7
/// missed heartbeats; if we haven't heard from the sidecar in that
/// long it's not coming back on its own.
const HEARTBEAT_STOPPED_MS: u128 = 15_000;
/// How often the watchdog re-emits the current health state even
/// when nothing has changed — so a panel that just mounted converges
/// to truth without waiting for a transition.
const HEALTH_REPEAT_TICKS: u64 = 5;
/// How long capture must stay continuously non-`Live` before the menu-bar UI
/// declares it not-active (drives [`EVT_CAPTURE_UI`]). Measured from the moment
/// health *left* `Live`. Sized to clear the whole self-heal window: a disabled
/// tap re-arms in the sidecar within ~2s, and a dead sidecar is auto-respawned
/// at [`HEARTBEAT_STOPPED_MS`] (15s) with its first fresh heartbeat ~2s later —
/// so 16s gives that one automatic respawn time to land before we alarm. A
/// transient blip recovers to `Live` (clearing the timer) long before this, so
/// the icon never strobes; only a genuine, persistent stop trips it.
const NOT_ACTIVE_DEBOUNCE_MS: u128 = 16_000;

/// Spawn (or respawn) the Swift sidecar — `app.shell().sidecar()` plus
/// the `TYPEASSIST_AX_PROMPT=1` env that opts into the macOS
/// Accessibility dialog when the permission is missing. Factored out
/// so commit O's hard-restart path uses the SAME spawn shape as the
/// initial boot — divergence here would be a fertile source of "works
/// the first time, then dies on restart" bugs.
fn spawn_sidecar<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<
    (
        tokio::sync::mpsc::Receiver<CommandEvent>,
        tauri_plugin_shell::process::CommandChild,
    ),
    Box<dyn std::error::Error>,
> {
    let mut cmd = app
        .shell()
        .sidecar("typeassist-input-macos")?
        .env("TYPEASSIST_AX_PROMPT", "1");
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
        // M3 correction Step 1: the last fix, retained so a single Escape
        // within `UNDO_WINDOW_MS` reverts it (and teach-stops the pattern).
        // Disarmed on revert, on any other keystroke (implicit accept), and on
        // window timeout (watchdog).
        let mut last_correction: Option<LastCorrection> = None;
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

        // Menu-bar icon state, reconciled from two signals (capture-stopped +
        // practice-dot). `last_tray_icon` is the last `(not_active, practice_dot)`
        // actually pushed to the OS (`None` until the first set). `current_practice_dot`
        // persists the latest badge decision between stability emits so a
        // health-driven icon refresh keeps the badge correct.
        let mut last_tray_icon: Option<(bool, bool)> = None;
        let mut current_practice_dot = false;

        // Debounced menu-bar "capture active?" state (drives EVT_CAPTURE_UI).
        // `non_live_since` = when health last LEFT Live (None while Live), so the
        // debounce is measured from the start of the outage; `ui_not_active` is
        // the settled flag the UI renders. `permission_ok` tracks the sidecar's
        // Accessibility grant — flipped false on a `PermissionRequired`, true on
        // any heartbeat (a heartbeat means the sidecar built a tap → grant in
        // effect) — and chooses the recovery action. `last_ui_emit` dedupes the
        // EVT_CAPTURE_UI emit.
        let mut non_live_since: Option<Instant> = None;
        let mut ui_not_active = false;
        let mut permission_ok = true;
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
                        }
                        InputEvent::Backspace { .. } => {
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
                            );
                            let snap = anchors.snapshot();
                            let _ = app_handle.emit(
                                EVT_ANCHOR_SNAPSHOT,
                                anchor_emit_payload(&snap, &line_buf),
                            );
                        }
                        InputEvent::Key { key, dwell_ms, modifiers, .. } => {
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
                                    dump_classifications(&word_patterns);
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
                                    // Revert: delete the target + boundary we
                                    // injected, retype the original word +
                                    // boundary.
                                    let delete_count = (lc.target.chars().count() + 1) as u32;
                                    let replacement = format!("{}{}", lc.typed, lc.boundary);
                                    let echo_len =
                                        delete_count + replacement.chars().count() as u32;
                                    if send_inject_correction(
                                        &mut sidecar_child,
                                        delete_count,
                                        replacement,
                                    ) {
                                        // Skip the revert's own echo too.
                                        pending_echo += echo_len;
                                        funnel.corrections_undone += 1;
                                        tracing::info!(
                                            "CORRECTION_UNDONE typed={:?} target={:?}",
                                            lc.typed,
                                            lc.target
                                        );
                                        // Cue shows the revert direction.
                                        let _ = app_handle.emit(
                                            EVT_CORRECTION_APPLIED,
                                            CorrectionAppliedEvent {
                                                typed: lc.target.clone(),
                                                target: lc.typed.clone(),
                                                undo: true,
                                            },
                                        );
                                    }
                                    // Teach-stop: a wrong fix is a one-key fix
                                    // that won't recur. Remove + persist + echo
                                    // so the panel/tray reflect the removal.
                                    if allow_list.disable(&lc.typed) {
                                        tracing::info!(
                                            "ALLOW_LIST_TEACH_STOP removed typed={:?}",
                                            lc.typed
                                        );
                                        persist_and_emit_allow_list(
                                            &app_handle,
                                            &allow_list,
                                            allow_list_path.as_deref(),
                                        );
                                    }
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

                                            // M3 Step 1: set when a correction
                                            // fired and reset the line, so the
                                            // trailing resolver tick is skipped
                                            // (mirrors the newline-reset path,
                                            // which also doesn't tick after).
                                            let mut corrected = false;
                                            if was_end_of_line {
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
                                                    let fire = if matches!(tok.kind, TokenKind::Word) {
                                                        allow_list.target_for(&tok.core).map(|t| {
                                                            (tok.core.clone(), t.to_string(), tok.end - tok.start)
                                                        })
                                                    } else {
                                                        None
                                                    };
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
                                                    if let Some((typed, target, word_len)) = fire {
                                                        // Delete the word + the
                                                        // boundary char we just
                                                        // typed, then retype the
                                                        // target + the same
                                                        // boundary. Caret is at the
                                                        // end of the line here
                                                        // (was_end_of_line), so a
                                                        // plain backspace-count
                                                        // delete is correct.
                                                        let delete_count = (word_len + 1) as u32;
                                                        let replacement = format!("{target}{c}");
                                                        let echo_len = delete_count
                                                            + replacement.chars().count() as u32;
                                                        if send_inject_correction(
                                                            &mut sidecar_child,
                                                            delete_count,
                                                            replacement,
                                                        ) {
                                                            // Skip this fix's echo
                                                            // (backspaces + retyped
                                                            // chars) when it streams
                                                            // back through the tap.
                                                            pending_echo += echo_len;
                                                            funnel.corrections_applied += 1;
                                                            tracing::info!(
                                                                "CORRECTION_APPLIED typed={:?} target={:?} delete={}",
                                                                typed,
                                                                target,
                                                                delete_count
                                                            );
                                                            let _ = app_handle.emit(
                                                                EVT_CORRECTION_APPLIED,
                                                                CorrectionAppliedEvent {
                                                                    typed: typed.clone(),
                                                                    target: target.clone(),
                                                                    undo: false,
                                                                },
                                                            );
                                                            last_correction = Some(LastCorrection {
                                                                typed,
                                                                target,
                                                                boundary: c,
                                                                fired_at_ms: now_ms(),
                                                            });
                                                            // The tap suppresses the
                                                            // injection's echo, so
                                                            // the engine won't see
                                                            // the text change — reset
                                                            // the line to mirror the
                                                            // corrected state (same
                                                            // precedent as the
                                                            // newline reset). This
                                                            // also drops the
                                                            // just-sealed typed word's
                                                            // Pending record (it
                                                            // idle-abandons), so the
                                                            // engine never learns from
                                                            // its own fix.
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
                                                            corrected = true;
                                                        }
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
                            current_practice_dot = wants_practice_dot(&report);
                            apply_tray_icon(
                                &app_handle,
                                ui_not_active,
                                current_practice_dot,
                                &mut last_tray_icon,
                            );
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
                        EngineControl::SetCorrectionEnabled(enabled) => {
                            // M3 Step 1 master gate — the instant global on/off.
                            // Persist + echo so the tray check and panel switch
                            // converge on the engine's authoritative state. Turn
                            // OFF disarms any pending undo (no fix to revert).
                            if allow_list.set_enabled(enabled) {
                                if !enabled {
                                    last_correction = None;
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
                        EngineControl::SetPatternEnabled { typed, target, enabled } => {
                            // M3 Step 1 — the panel's per-pattern toggle. Enable
                            // adds (idempotent), disable removes; either way
                            // persist + echo so the panel reflects the truth.
                            let changed = if enabled {
                                allow_list.enable(&typed, &target)
                            } else {
                                allow_list.disable(&typed)
                            };
                            if changed {
                                tracing::info!(
                                    "ALLOW_LIST_TOGGLE typed={typed:?} target={target:?} enabled={enabled}"
                                );
                            }
                            persist_and_emit_allow_list(
                                &app_handle,
                                &allow_list,
                                allow_list_path.as_deref(),
                            );
                        }
                        EngineControl::RequestAllowList => {
                            // Panel/tray pull fresh state on open. Read-only.
                            let _ = app_handle.emit(EVT_CORRECTION_STATE, allow_list.clone());
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
                    );

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
                        dump_classifications(&word_patterns);
                        // Observe-only guesser accuracy readout (no injection).
                        dump_guess_accuracy(&guess_ledger);
                    }

                    // C5c motor stability: periodic read-model emit so a
                    // passive consumer (debug panel / future Practice) stays
                    // current. Skip while the map is empty (nothing to say).
                    if watchdog_ticks % MOTOR_STABILITY_EMIT_TICKS == 0 && !motor_map.is_empty() {
                        let report: StabilityReport =
                            motor_map.stability_report(WEAKEST_PREVIEW_N);
                        // Tray "dot" reflects whether there are weak keys worth
                        // practicing — shape, not colour (it's a template icon).
                        // The actual icon push happens below (after the health
                        // debounce), so capture-stopped can override the badge.
                        current_practice_dot = wants_practice_dot(&report);
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
                    if matches!(current_capture_health, CaptureHealth::Live) {
                        non_live_since = None;
                    } else if non_live_since.is_none() {
                        non_live_since = Some(Instant::now());
                    }
                    ui_not_active = non_live_since.is_some_and(|since| {
                        Instant::now().duration_since(since).as_millis() >= NOT_ACTIVE_DEBOUNCE_MS
                    });

                    // Push the menu-bar icon (capture-stopped overrides the
                    // practice badge); no-op when unchanged.
                    apply_tray_icon(
                        &app_handle,
                        ui_not_active,
                        current_practice_dot,
                        &mut last_tray_icon,
                    );

                    // Emit the settled UI state on change, and periodically so a
                    // freshly-registered listener (the tray) converges. The
                    // recovery action only matters while not active, so pin
                    // permission_revoked to false when active (avoids a spurious
                    // change emit from a stale flag).
                    let ui = CaptureUiEvent {
                        active: !ui_not_active,
                        permission_revoked: ui_not_active && !permission_ok,
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
