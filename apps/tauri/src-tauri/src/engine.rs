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

use std::time::Instant;

use std::time::{SystemTime, UNIX_EPOCH};

use behavioural_model::{BehaviouralModel, InputEvent};
use correction_engine::{
    decide, has_motor_evidence, ranked_known_candidates, score_candidates, should_log,
    AnchorTracker, Confidence, ConfidenceTier, DecisionLedger, DecisionOutcome, Lexicon,
    OutcomeResolver, ScoredCandidate, Token, TokenKind, Tokenizer, ACTIVE_TIER,
    CANDIDATES_VERSION, DECISION_VERSION, LEXICON_VERSION, SCORE_VERSION,
};
use serde::Serialize;
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
// Up/Down kept here for documentation and tests — they're filtered out
// (via `is_non_text_key`) but deliberately not mapped to caret moves in
// the single-line model. Marked dead_code so the binary build doesn't
// warn; the test module references them.
#[allow(dead_code)]
const KEY_C0_UP: char = '\u{001E}';
#[allow(dead_code)]
const KEY_C0_DOWN: char = '\u{001F}';
const KEY_NS_LEFT: char = '\u{F702}';
const KEY_NS_RIGHT: char = '\u{F703}';
#[allow(dead_code)]
const KEY_NS_UP: char = '\u{F700}';
#[allow(dead_code)]
const KEY_NS_DOWN: char = '\u{F701}';
const KEY_NS_HOME: char = '\u{F729}';
const KEY_NS_END: char = '\u{F72B}';

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

/// Run one [`OutcomeResolver`] pass and surface every transition. The
/// resolver itself is a pure observer — this helper applies the
/// transitions to the ledger and broadcasts the updated record on
/// [`EVT_LOG_RECORD_UPDATED`]. Called from every anchor-affecting site:
/// once a fresh anchor lands (so the debounce timer starts ticking),
/// and once after each edit that might move a record's outcome.
fn tick_resolver<R: Runtime>(
    app: &AppHandle<R>,
    resolver: &mut OutcomeResolver,
    anchors: &AnchorTracker,
    line_buf: &[char],
    ledger: &mut DecisionLedger,
) {
    let changes = resolver.tick(now_ms(), anchors.anchors(), line_buf, ledger);
    for (record_id, outcome) in changes {
        if ledger.resolve_outcome(record_id, outcome) {
            if let Some(rec) = ledger.get(record_id).cloned() {
                let _ = app.emit(EVT_LOG_RECORD_UPDATED, rec);
            }
        }
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
    line_dwells: &[u32],
) {
    if matches!(tok.kind, TokenKind::Word) {
        // Resolve the anchor id BEFORE running the pipeline so a missing
        // id (a real bug, not a normal outcome) shows up next to the
        // decision in the log. Fresh registrations return Some(id);
        // replays of the same span/core return None from try_register
        // and we follow up with find_tracking_id.
        let fresh_id = anchors.try_register(tok.start, tok.end, &tok.core);
        let anchor_id = fresh_id
            .or_else(|| anchors.find_tracking_id(tok.start, tok.end, &tok.core));

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

        // Snapshot what the C5a resolver will need from the score report
        // BEFORE we move `report.scored` into the panel emission. Sourced
        // from the report (not the decision arm) so a
        // `LeaveAlone(BelowActiveTier)` record still carries the
        // candidate — that's the load-bearing fix for the
        // "bullon → bullion under Cautious" misclassification.
        let top_candidate_word = report.scored.first().map(|s| s.word.clone());
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
        let _ = app.emit(
            EVT_DECISION,
            DecisionPayload {
                outcome: outcome.clone(),
                active_tier: ACTIVE_TIER,
                decide_time_ms,
                decision_version: DECISION_VERSION,
            },
        );

        // Component 4 — append to the decision ledger if the gates pass.
        // Two gates: (a) UNKNOWN-word filter via `should_log`; (b) motor
        // evidence via per-char dwells in this token's span. The anchor
        // id is required — if we couldn't resolve it (shouldn't happen
        // for Word tokens that just registered), we skip the append
        // rather than fabricate a link. The privacy guarantee is
        // structural: no motor evidence → no ledger entry.
        let has_motor = has_motor_evidence(line_dwells, tok.start, tok.end);
        if should_log(&outcome, has_motor) {
            if let Some(anchor_id) = anchor_id {
                let ts = now_ms();
                let new_id = ledger.append(
                    ts,
                    outcome,
                    anchor_id,
                    ACTIVE_TIER,
                    top_candidate_word,
                    top_score_for_log,
                    top_confidence_for_log,
                );
                // Emit the just-appended record. The ledger owns it and
                // may evict later, but the panel keeps its own copy in
                // its own bounded list.
                if let Some(rec) = ledger.get(new_id).cloned() {
                    let _ = app.emit(EVT_LOG_RECORD, rec);
                }
            } else {
                // Decision passed the gates but the anchor id wasn't
                // resolvable. Surface the bug rather than silently
                // dropping the record; C5 needs the anchor link to
                // attribute outcomes at all.
                tracing::warn!(
                    "C4 ledger skipped a loggable decision: anchor id unresolved \
                     for word {:?} at [{}, {})",
                    tok.core,
                    tok.start,
                    tok.end
                );
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

pub fn spawn<R: Runtime>(app: &AppHandle<R>) -> Result<(), Box<dyn std::error::Error>> {
    // TYPEASSIST_AX_PROMPT=1 asks the sidecar to pop the macOS Accessibility
    // dialog if the permission is missing — appropriate now that the Tauri app
    // is the engine's host (CLAUDE.md: "leaves the prompt to L5").
    let sidecar = app
        .shell()
        .sidecar("typeassist-input-macos")?
        .env("TYPEASSIST_AX_PROMPT", "1");

    // `sidecar_child` owns the parent-side write-end of the sidecar's
    // stdin pipe. It is **moved into the async task below** and dropped
    // when that task ends — see the load-bearing comment at the bottom
    // of the closure for the lifetime contract.
    let (mut rx, sidecar_child) = sidecar.spawn()?;
    let app_handle = app.clone();

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
        // L4 lexicon (Component 3a). Process-wide singleton — first touch
        // parses the ~50k-entry bundled list; subsequent reads are HashMap
        // lookups. Read-only this slice: scoring/correction come later.
        let lexicon: &'static Lexicon = Lexicon::shared();
        // Caret position in `line_buf` (char index). Anchor edit deltas
        // (p, d, i) are computed from this. Updated on inserts, backspaces,
        // and Left / Right / Home / End nav keys; mouse-click moves and
        // paste are intentionally out of scope (Component 5 AX backstop).
        let mut caret: usize = 0;

        while let Some(event) = rx.recv().await {
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
                                "sidecar reports Accessibility permission missing — \
                                 grant in System Settings › Privacy & Security › Accessibility"
                            );
                        }
                        InputEvent::Shutdown => break,
                        InputEvent::Backspace { .. } => {
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
                            for c in replay {
                                if let Some(tok) = tokenizer.observe_char(c) {
                                    emit_sealed_token(
                                        &app_handle,
                                        tok,
                                        &mut anchors,
                                        lexicon,
                                        model.slip_detector.map(),
                                        &mut ledger,
                                        &line_dwells,
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
                                &anchors,
                                &line_buf,
                                &mut ledger,
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
                            // sidecar is actually emitting. INFO level on
                            // purpose — load-bearing while the nav-key story
                            // settles; demote once stable.
                            tracing::info!(
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

                            if is_non_text {
                                // Caret-only handling. Left/Right/Home/End
                                // map to caret moves; Up/Down and every
                                // other non-text codepoint are intentional
                                // no-ops for the single-line model. No
                                // edit, no token feed — and the buffer is
                                // never written.
                                let c = single_char.unwrap();
                                tracing::info!(
                                    "non-text key U+{:04X} — caret-only handling",
                                    c as u32
                                );
                                // ONE coherent handler for all four nav
                                // keys. Anything not mapped (Up/Down,
                                // F1–F12, other PU codepoints) returns
                                // `None` and we no-op. The Command flag
                                // distinguishes Cmd+Left/Right (= Home/End
                                // on macOS) from plain arrow navigation.
                                if let Some(new_caret) =
                                    nav_action(c, caret, line_buf.len(), modifiers.command)
                                {
                                    caret = new_caret;
                                }
                                // Anchor positions don't move on pure
                                // navigation, so no snapshot emit needed.
                            } else {
                                // Feed the tokenizer + drive the anchor tracker.
                                // Component 1 (token) and Component 2 (anchor)
                                // both live downstream of this block.
                                let mut ch_iter = key.chars();
                                match (ch_iter.next(), ch_iter.next()) {
                                    (Some(c), None) => {
                                        if c == '\n' || c == '\r' {
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
                                                    &line_dwells,
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
                                            // C4 motor-evidence proxy: store this
                                            // char's dwell at the same index. A real
                                            // keystroke carries a non-zero dwell;
                                            // pasted / synthetic chars come through
                                            // with dwell == 0 and fail the gate.
                                            line_dwells.insert(insert_pos, dwell_ms);
                                            anchors.apply_insert(caret, c);
                                            caret = new_caret;

                                            if was_end_of_line {
                                                if let Some(tok) = tokenizer.observe_char(c) {
                                                    emit_sealed_token(
                                                        &app_handle,
                                                        tok,
                                                        &mut anchors,
                                                        lexicon,
                                                        model.slip_detector.map(),
                                                        &mut ledger,
                                                        &line_dwells,
                                                    );
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
                                                for c in replay {
                                                    if let Some(tok) = tokenizer.observe_char(c) {
                                                        emit_sealed_token(
                                                            &app_handle,
                                                            tok,
                                                            &mut anchors,
                                                            lexicon,
                                                            model.slip_detector.map(),
                                                            &mut ledger,
                                                            &line_dwells,
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
                                            // record).
                                            tick_resolver(
                                                &app_handle,
                                                &mut resolver,
                                                &anchors,
                                                &line_buf,
                                                &mut ledger,
                                            );
                                            let snap = anchors.snapshot();
                                            let _ = app_handle.emit(
                                                EVT_ANCHOR_SNAPSHOT,
                                                anchor_emit_payload(&snap, &line_buf),
                                            );
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
                        "sidecar terminated (code={:?}, signal={:?})",
                        payload.code,
                        payload.signal
                    );
                    break;
                }
                _ => {}
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
        // quitting, `CommandEvent::Terminated`, panic unwind) means the
        // sidecar dies with us instead of leaking.
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
    });

    Ok(())
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
