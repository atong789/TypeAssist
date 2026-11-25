//! Resolution diary — an append-only, LOCAL, per-event log of how each correction
//! bubble was resolved: **taken** (Shift-accepted), **retyped** (ignored, then the
//! user typed the suggested word themselves) or **ignored**.
//!
//! This is a per-event, timestamped layer *beside* the all-time decayed stores
//! (`word_patterns.json` etc.), which are unchanged. It exists so Progress can show
//! the actual words for a recent window / week-over-week, and so two Macs can
//! **merge** their event logs without double-counting.
//!
//! **Format** — `resolution_diary.jsonl`, one JSON object per line, plain and
//! OS-neutral (epoch-ms timestamps, no paths, no keycodes):
//! `{"v":1,"id":…,"ts":…,"dev":…,"typed":…,"target":…,"outcome":"taken|retyped|ignored","supersedes":…}`
//!
//! * `id` is unique per event (`<device8>-<ts hex>-<seq hex>`); merge is a **union by
//!   id**, so importing the same file twice changes nothing.
//! * `dev` is a random per-install id (`device_id` file) — never hardware-derived.
//! * A `retyped` row carries `supersedes: <id of the ignored row>`. Ignored is logged
//!   when the bubble ends; retyped is only known later (≤30 s). The reader folds the
//!   pair so one bubble counts **once** (as retyped), including after a merge.
//!
//! **Privacy (Principles #8 / content-blind).** Words are stored, so collection is
//! **consent-gated: no recorded choice = off.** It starts only when onboarding
//! **completes** (the user has just read the consent line) — [`record_consent`] writes
//! the `resolution_diary.on` flag — or when the user turns it on in Settings; the
//! Settings switch can stop it anytime (an explicit `resolution_diary.off` marker
//! then blocks re-consent). Strictly on-device, with **hard retention**
//! ([`RETENTION_MS`], 14 days) and a **byte cap** ([`MAX_BYTES`]). No app id, no
//! context, no position: exactly the four content fields above. Turning it off
//! deletes the log. Merging is a user-initiated file import — never network.
//!
//! **Principle #6 exception, stated plainly:** this file is deliberately pruned. It
//! is short-lived *view* data, not recovery history (the dated snapshots remain the
//! durable record).
//!
//! **Principle #7.** `decided` (a resolution the engine made while the diary was on)
//! and `appended` (a line that actually landed) are independent counters, reconciled
//! at the funnel-dump cadence ([`ResolutionDiary::dump`]); a gap is logged loudly.
//!
//! The engine task is the **sole writer**; the UI reads via [`read_summary`].

use correction_engine::persist::durable_write;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DIARY_FILE: &str = "resolution_diary.jsonl";
pub const DIARY_FLAG: &str = "resolution_diary.on";
/// Explicit opt-out marker. Distinguishes "turned it off" from "never chose": consent
/// (onboarding completion) never re-enables over it.
pub const DIARY_OFF_FLAG: &str = "resolution_diary.off";
pub const DEVICE_ID_FILE: &str = "device_id";

/// Events older than this are pruned (14 days: two full weeks for week-over-week).
pub const RETENTION_MS: u64 = 14 * 24 * 60 * 60 * 1000;
/// A week, for the this-week / prior-week split.
pub const WEEK_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Rows dated further ahead than this (clock skew / a bad import) are dropped.
const FUTURE_TOLERANCE_MS: u64 = 24 * 60 * 60 * 1000;
/// Hard size cap. Past it the oldest rows are trimmed (logging continues).
pub const MAX_BYTES: u64 = 2_000_000;
/// After a size trim, shrink to this fraction of the cap so we don't trim per line.
const TRIM_TARGET_BYTES: u64 = MAX_BYTES * 9 / 10;
/// Re-run the age prune at most this often while the engine stays up.
const PRUNE_INTERVAL_MS: u64 = 6 * 60 * 60 * 1000;
/// Max distinct word rows returned per outcome per week.
const MAX_WORDS_PER_LIST: usize = 50;
const SCHEMA_V: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiaryOutcome {
    Taken,
    Retyped,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiaryEvent {
    #[serde(default = "schema_v")]
    pub v: u32,
    pub id: String,
    pub ts: u64,
    pub dev: String,
    pub typed: String,
    pub target: String,
    pub outcome: DiaryOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

fn schema_v() -> u32 {
    SCHEMA_V
}

fn norm(w: &str) -> String {
    w.trim().to_lowercase()
}

/// A row is well-formed enough to keep: non-empty id/words, typed ≠ target.
fn well_formed(e: &DiaryEvent) -> bool {
    !e.id.is_empty()
        && !e.dev.is_empty()
        && !e.typed.is_empty()
        && !e.target.is_empty()
        && e.typed != e.target
}

/// Parse a jsonl body. Unparseable / malformed lines are skipped and counted —
/// one bad line never sinks the file.
pub fn parse_lines(text: &str) -> (Vec<DiaryEvent>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<DiaryEvent>(line) {
            Ok(e) if well_formed(&e) => out.push(e),
            _ => skipped += 1,
        }
    }
    (out, skipped)
}

fn serialize_lines(events: &[DiaryEvent]) -> String {
    let mut s = String::new();
    for e in events {
        if let Ok(l) = serde_json::to_string(e) {
            s.push_str(&l);
            s.push('\n');
        }
    }
    s
}

/// Keep rows inside the retention window (and not absurdly in the future).
pub fn retained(events: Vec<DiaryEvent>, now: u64) -> Vec<DiaryEvent> {
    let floor = now.saturating_sub(RETENTION_MS);
    let ceil = now.saturating_add(FUTURE_TOLERANCE_MS);
    events
        .into_iter()
        .filter(|e| e.ts >= floor && e.ts <= ceil)
        .collect()
}

/// Result of a union-by-id merge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MergeReport {
    /// Rows from the incoming side that were new.
    pub added: usize,
    /// Rows whose id was already present (dedupe).
    pub duplicates: usize,
    /// Unparseable / malformed incoming lines.
    pub skipped: usize,
    /// Incoming rows outside the retention window.
    pub expired: usize,
    /// Rows in the merged log.
    pub total: usize,
}

/// Union of two event sets by `id` (first occurrence wins), sorted by `(ts, id)`.
/// Pure and idempotent: `merge(a, a) == a`, and `merge(merge(a,b), b) == merge(a,b)`.
pub fn merge_events(a: Vec<DiaryEvent>, b: Vec<DiaryEvent>) -> (Vec<DiaryEvent>, usize) {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(a.len() + b.len());
    let mut dups = 0;
    for e in a.into_iter().chain(b) {
        if seen.insert(e.id.clone()) {
            out.push(e);
        } else {
            dups += 1;
        }
    }
    out.sort_by(|x, y| (x.ts, &x.id).cmp(&(y.ts, &y.id)));
    (out, dups)
}

/// Fold ignored→retyped pairs: an `ignored` row superseded by a `retyped` row is
/// dropped so the bubble counts once (as retyped). A `retyped` whose ignored row is
/// absent (pruned / on another Mac's log) simply counts as retyped.
pub fn fold(events: &[DiaryEvent]) -> Vec<&DiaryEvent> {
    let superseded: HashSet<&str> = events
        .iter()
        .filter(|e| e.outcome == DiaryOutcome::Retyped)
        .filter_map(|e| e.supersedes.as_deref())
        .collect();
    events
        .iter()
        .filter(|e| !(e.outcome == DiaryOutcome::Ignored && superseded.contains(e.id.as_str())))
        .collect()
}

/// One distinct `typed → target` with how many times it resolved this way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WordCount {
    pub typed: String,
    pub target: String,
    pub count: u32,
}

/// Counts + the actual words, for one window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WindowSummary {
    pub taken: u32,
    pub retyped: u32,
    pub ignored: u32,
    pub taken_words: Vec<WordCount>,
    pub retyped_words: Vec<WordCount>,
    pub ignored_words: Vec<WordCount>,
}

/// What the Progress UI reads. `this_week` = last 7 days, `prior_week` = the 7 before.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DiarySummary {
    /// Whether collection is switched on right now.
    pub enabled: bool,
    /// Earliest / latest retained event (None = nothing recorded yet).
    pub oldest_ts: Option<u64>,
    pub newest_ts: Option<u64>,
    pub total_events: usize,
    /// Lines in the file that could not be read (shown honestly, never hidden).
    pub skipped_lines: usize,
    pub this_week: WindowSummary,
    pub prior_week: WindowSummary,
}

fn window(events: &[&DiaryEvent], from: u64, to: u64) -> WindowSummary {
    let mut w = WindowSummary::default();
    let mut maps: [HashMap<(String, String), u32>; 3] = Default::default();
    for e in events.iter().filter(|e| e.ts >= from && e.ts < to) {
        let i = match e.outcome {
            DiaryOutcome::Taken => {
                w.taken += 1;
                0
            }
            DiaryOutcome::Retyped => {
                w.retyped += 1;
                1
            }
            DiaryOutcome::Ignored => {
                w.ignored += 1;
                2
            }
        };
        *maps[i]
            .entry((e.typed.clone(), e.target.clone()))
            .or_insert(0) += 1;
    }
    let list = |m: HashMap<(String, String), u32>| {
        let mut v: Vec<WordCount> = m
            .into_iter()
            .map(|((typed, target), count)| WordCount {
                typed,
                target,
                count,
            })
            .collect();
        v.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.typed.cmp(&b.typed))
                .then_with(|| a.target.cmp(&b.target))
        });
        v.truncate(MAX_WORDS_PER_LIST);
        v
    };
    let [t, r, i] = maps;
    w.taken_words = list(t);
    w.retyped_words = list(r);
    w.ignored_words = list(i);
    w
}

/// Pure summary of an event set at `now`.
pub fn summarize(
    events: Vec<DiaryEvent>,
    now: u64,
    enabled: bool,
    skipped_lines: usize,
) -> DiarySummary {
    let events = retained(events, now);
    let folded = fold(&events);
    let week_start = now.saturating_sub(WEEK_MS);
    let prior_start = now.saturating_sub(2 * WEEK_MS);
    let end = now.saturating_add(FUTURE_TOLERANCE_MS);
    DiarySummary {
        enabled,
        oldest_ts: events.iter().map(|e| e.ts).min(),
        newest_ts: events.iter().map(|e| e.ts).max(),
        total_events: events.len(),
        skipped_lines,
        this_week: window(&folded, week_start, end),
        prior_week: window(&folded, prior_start, week_start),
    }
}

// ---- file-level helpers (a data dir in, no engine state) ------------------

/// Is collection switched on for this data dir? Only the `on` flag counts — written
/// solely by [`record_consent`] (onboarding completed) or the Settings switch. There
/// is deliberately **no** env-var or other back door around consent.
pub fn is_enabled(dir: &Path) -> bool {
    dir.join(DIARY_FLAG).exists()
}

fn read_events(dir: &Path) -> (Vec<DiaryEvent>, usize) {
    match std::fs::read_to_string(dir.join(DIARY_FILE)) {
        Ok(t) => parse_lines(&t),
        Err(_) => (Vec::new(), 0),
    }
}

/// Read-only summary for the Progress UI. Never writes.
pub fn read_summary(dir: &Path, now: u64) -> DiarySummary {
    let (events, skipped) = read_events(dir);
    summarize(events, now, is_enabled(dir), skipped)
}

/// Export the retained events to `dest` (read-only on the data dir; the user picked
/// `dest`). Returns the number of rows written.
pub fn export_to(dir: &Path, dest: &Path, now: u64) -> Result<usize, String> {
    let (events, _) = read_events(dir);
    let events = retained(events, now);
    durable_write(dest, serialize_lines(&events).as_bytes())
        .map_err(|e| format!("write export: {e}"))?;
    Ok(events.len())
}

/// Merge an incoming jsonl file into the diary (union by id, retention applied,
/// atomic rewrite). **Engine-only** (sole writer). Idempotent.
pub fn import_file(dir: &Path, incoming: &Path, now: u64) -> Result<MergeReport, String> {
    let text =
        std::fs::read_to_string(incoming).map_err(|e| format!("could not read file: {e}"))?;
    let (inc, skipped) = parse_lines(&text);
    if inc.is_empty() && skipped > 0 {
        return Err("not a resolution diary file".to_string());
    }
    let inc_total = inc.len();
    let inc = retained(inc, now);
    let expired = inc_total - inc.len();
    let inc_kept = inc.len();
    let (own, _) = read_events(dir);
    let (merged, dups) = merge_events(own, inc);
    let merged = trim_to_cap(retained(merged, now));
    std::fs::create_dir_all(dir).map_err(|e| format!("create data dir: {e}"))?;
    durable_write(&dir.join(DIARY_FILE), serialize_lines(&merged).as_bytes())
        .map_err(|e| format!("write diary: {e}"))?;
    let total = merged.len();
    Ok(MergeReport {
        added: inc_kept.saturating_sub(dups),
        duplicates: dups,
        skipped,
        expired,
        total,
    })
}

/// Drop the oldest rows until the serialized log fits under the trim target.
fn trim_to_cap(mut events: Vec<DiaryEvent>) -> Vec<DiaryEvent> {
    let mut size: u64 = events
        .iter()
        .map(|e| {
            serde_json::to_string(e)
                .map(|s| s.len() as u64 + 1)
                .unwrap_or(0)
        })
        .sum();
    if size <= MAX_BYTES {
        return events;
    }
    let mut cut = 0;
    while size > TRIM_TARGET_BYTES && cut < events.len() {
        size -= serde_json::to_string(&events[cut])
            .map(|s| s.len() as u64 + 1)
            .unwrap_or(0);
        cut += 1;
    }
    events.drain(..cut);
    events
}

fn random_u64(salt: u64) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(salt);
    h.write_u32(std::process::id());
    h.finish()
}

fn load_or_create_device_id(dir: &Path, now: u64) -> Option<String> {
    let path = dir.join(DEVICE_ID_FILE);
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim();
        if s.len() >= 8 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(s.to_string());
        }
    }
    let id = format!(
        "{:016x}{:016x}",
        random_u64(now),
        random_u64(now ^ 0x9E37_79B9_7F4A_7C15)
    );
    std::fs::create_dir_all(dir).ok()?;
    durable_write(&path, id.as_bytes()).ok()?;
    Some(id)
}

/// Record the user's consent — called when onboarding **completes** (the user has
/// just read the consent line on its last step). Creates the `on` flag unless the
/// user has explicitly turned the diary off (`off` flag present): an explicit
/// opt-out always wins, so a replayed onboarding can't re-enable it. Returns
/// whether collection is now on. **No recorded choice = off** (nothing else ever
/// writes the `on` flag except this and the Settings switch).
pub fn record_consent(dir: &Path) -> Result<bool, String> {
    if dir.join(DIARY_OFF_FLAG).exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("create data dir: {e}"))?;
    std::fs::write(dir.join(DIARY_FLAG), b"").map_err(|e| format!("record consent: {e}"))?;
    Ok(true)
}

/// Engine-side writer. Default = disabled (no dir), so tests/other callers that use
/// `BubbleLedger::default()` never touch disk.
#[derive(Debug, Default)]
pub struct ResolutionDiary {
    dir: Option<PathBuf>,
    device: Option<String>,
    seq: u64,
    last_prune_ms: u64,
    /// Resolutions decided while the diary was on (independent side of the reconcile).
    decided: u64,
    /// Lines that actually landed on disk (the other side).
    appended: u64,
}

impl ResolutionDiary {
    /// Open against a data dir. Prunes on startup if the diary is on.
    pub fn open(dir: Option<PathBuf>, now: u64) -> Self {
        let mut d = Self {
            dir,
            ..Self::default()
        };
        if d.enabled() {
            d.prune(now);
        }
        d
    }

    /// Checked on every call (one cheap `stat`) so flipping the flag takes effect
    /// without a restart.
    pub fn enabled(&self) -> bool {
        self.dir.as_deref().is_some_and(is_enabled)
    }

    /// Record one resolution. Returns the new row's id (so a later `retyped` can
    /// reference it), or `None` if disabled / not recordable / the write failed.
    /// Best-effort: never disturbs the pipeline.
    pub fn record(
        &mut self,
        outcome: DiaryOutcome,
        typed: &str,
        target: &str,
        supersedes: Option<String>,
        now: u64,
    ) -> Option<String> {
        let dir = self.dir.clone()?;
        if !is_enabled(&dir) {
            return None;
        }
        let (typed, target) = (norm(typed), norm(target));
        // A legitimately-skipped pair is not a "decided" resolution (same guard the
        // pattern store uses), so it can't make the reconcile cry wolf.
        if typed.is_empty() || target.is_empty() || typed == target {
            return None;
        }
        self.decided += 1;
        if now.saturating_sub(self.last_prune_ms) >= PRUNE_INTERVAL_MS {
            self.prune(now);
        }
        if self.device.is_none() {
            self.device = load_or_create_device_id(&dir, now);
        }
        let dev = self.device.clone()?;
        self.seq += 1;
        let id = format!("{}-{:x}-{:x}", &dev[..8.min(dev.len())], now, self.seq);
        let ev = DiaryEvent {
            v: SCHEMA_V,
            id: id.clone(),
            ts: now,
            dev,
            typed,
            target,
            outcome,
            supersedes,
        };
        let line = serde_json::to_string(&ev).ok()?;
        let path = dir.join(DIARY_FILE);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() >= MAX_BYTES) {
            self.prune(now); // size trim (also drops anything expired)
        }
        let _ = std::fs::create_dir_all(&dir);
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok()?;
        f.write_all(format!("{line}\n").as_bytes()).ok()?;
        self.appended += 1;
        Some(id)
    }

    /// Age prune + size trim (atomic rewrite). Skips the write if nothing changed.
    pub fn prune(&mut self, now: u64) {
        self.last_prune_ms = now;
        let Some(dir) = self.dir.clone() else { return };
        let path = dir.join(DIARY_FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let (events, skipped) = parse_lines(&text);
        let before = events.len();
        let kept = trim_to_cap(retained(events, now));
        if kept.len() != before || skipped > 0 {
            let _ = durable_write(&path, serialize_lines(&kept).as_bytes());
        }
    }

    /// Turn collection on/off. Off **deletes the log** (opt-out erases).
    pub fn set_enabled(&mut self, on: bool) -> Result<(), String> {
        let Some(dir) = self.dir.clone() else {
            return Err("no data directory".into());
        };
        let flag = dir.join(DIARY_FLAG);
        let off_flag = dir.join(DIARY_OFF_FLAG);
        if on {
            std::fs::create_dir_all(&dir).map_err(|e| format!("create data dir: {e}"))?;
            match std::fs::remove_file(&off_flag) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("enable: {e}")),
            }
            std::fs::write(&flag, b"").map_err(|e| format!("enable: {e}"))?;
        } else {
            // Record the explicit opt-out FIRST so a failure below can't leave the
            // diary on with the user's "off" half-applied.
            std::fs::create_dir_all(&dir).map_err(|e| format!("create data dir: {e}"))?;
            std::fs::write(&off_flag, b"").map_err(|e| format!("disable: {e}"))?;
            for p in [flag, dir.join(DIARY_FILE)] {
                match std::fs::remove_file(&p) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("disable: {e}")),
                }
            }
            self.decided = 0;
            self.appended = 0;
        }
        Ok(())
    }

    /// Onboarding completed ⇒ consent recorded ⇒ collection starts (unless the user
    /// explicitly opted out earlier — see [`record_consent`]).
    pub fn consent(&mut self) -> Result<bool, String> {
        let dir = self.dir.clone().ok_or("no data directory")?;
        record_consent(&dir)
    }

    /// Merge an incoming diary file (engine = sole writer).
    pub fn import(&mut self, path: &Path, now: u64) -> Result<MergeReport, String> {
        let dir = self.dir.clone().ok_or("no data directory")?;
        import_file(&dir, path, now)
    }

    /// `Some(description)` when decided ≠ appended — a write stopped landing.
    pub fn mismatch(&self) -> Option<String> {
        (self.decided != self.appended)
            .then(|| format!("decided={} appended={}", self.decided, self.appended))
    }

    /// Emit the reconcile line at the funnel-dump cadence (Principle #7).
    pub fn dump(&self) {
        if !self.enabled() && self.decided == 0 {
            return;
        }
        tracing::info!(
            "DIARY_DUMP {{ c_decided: {}, c_appended: {} }}",
            self.decided,
            self.appended
        );
        if let Some(d) = self.mismatch() {
            tracing::warn!("DIARY_INTEGRITY_BROKEN {d} — a diary write stopped landing");
        }
    }

    #[cfg(test)]
    pub fn counts(&self) -> (u64, u64) {
        (self.decided, self.appended)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60 * 1000;
    const NOW: u64 = 1_800_000_000_000;

    fn tmp(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("tcd-diary-{tag}-{:?}", std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn ev(
        id: &str,
        ts: u64,
        typed: &str,
        target: &str,
        o: DiaryOutcome,
        sup: Option<&str>,
    ) -> DiaryEvent {
        DiaryEvent {
            v: 1,
            id: id.into(),
            ts,
            dev: "devdevdev".into(),
            typed: typed.into(),
            target: target.into(),
            outcome: o,
            supersedes: sup.map(String::from),
        }
    }

    fn on(dir: &Path) {
        std::fs::write(dir.join(DIARY_FLAG), b"").unwrap();
    }

    // ---- writer ----

    #[test]
    fn disabled_by_default_writes_nothing() {
        let dir = tmp("off");
        let mut d = ResolutionDiary::open(Some(dir.clone()), NOW);
        assert!(d
            .record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .is_none());
        assert!(!dir.join(DIARY_FILE).exists(), "no file when off");
        assert!(!dir.join(DEVICE_ID_FILE).exists(), "no device id when off");
        assert_eq!(d.counts(), (0, 0), "off is not 'decided'");
    }

    #[test]
    fn default_diary_never_touches_disk() {
        let mut d = ResolutionDiary::default();
        assert!(d
            .record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .is_none());
        assert_eq!(d.counts(), (0, 0));
    }

    #[test]
    fn records_one_row_with_exactly_the_four_content_fields() {
        let dir = tmp("row");
        on(&dir);
        let mut d = ResolutionDiary::open(Some(dir.clone()), NOW);
        let id = d
            .record(DiaryOutcome::Taken, " Teh ", "THE", None, NOW)
            .expect("id");
        let text = std::fs::read_to_string(dir.join(DIARY_FILE)).unwrap();
        let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        keys.sort();
        // privacy guard: no app id / context / position — only these fields.
        assert_eq!(
            keys,
            vec!["dev", "id", "outcome", "target", "ts", "typed", "v"]
        );
        assert_eq!(v["typed"], "teh");
        assert_eq!(v["target"], "the");
        assert_eq!(v["outcome"], "taken");
        assert_eq!(v["id"], id.as_str());
        assert_eq!(d.counts(), (1, 1));
    }

    #[test]
    fn unrecordable_pairs_are_skipped_not_counted() {
        let dir = tmp("skip");
        on(&dir);
        let mut d = ResolutionDiary::open(Some(dir), NOW);
        assert!(d
            .record(DiaryOutcome::Ignored, "the", "The", None, NOW)
            .is_none());
        assert!(d
            .record(DiaryOutcome::Ignored, "", "the", None, NOW)
            .is_none());
        assert_eq!(d.counts(), (0, 0));
        assert!(d.mismatch().is_none());
    }

    #[test]
    fn ids_are_unique_and_device_id_is_stable() {
        let dir = tmp("ids");
        on(&dir);
        let mut d = ResolutionDiary::open(Some(dir.clone()), NOW);
        let a = d
            .record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .unwrap();
        let b = d
            .record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .unwrap();
        assert_ne!(a, b, "same ms, same pair → still distinct ids");
        let dev1 = std::fs::read_to_string(dir.join(DEVICE_ID_FILE)).unwrap();
        let mut d2 = ResolutionDiary::open(Some(dir.clone()), NOW + 1);
        d2.record(DiaryOutcome::Taken, "teh", "the", None, NOW + 1)
            .unwrap();
        assert_eq!(
            dev1,
            std::fs::read_to_string(dir.join(DEVICE_ID_FILE)).unwrap(),
            "device id persists"
        );
        assert_eq!(dev1.len(), 32);
    }

    #[test]
    fn dropped_write_makes_the_reconcile_mismatch() {
        let dir = tmp("drop");
        on(&dir);
        let mut d = ResolutionDiary::open(Some(dir.clone()), NOW);
        d.record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .unwrap();
        assert!(d.mismatch().is_none());
        // Sabotage: make the diary path a directory so the append fails.
        std::fs::remove_file(dir.join(DIARY_FILE)).unwrap();
        std::fs::create_dir(dir.join(DIARY_FILE)).unwrap();
        assert!(d
            .record(DiaryOutcome::Taken, "wrod", "word", None, NOW)
            .is_none());
        assert_eq!(d.counts(), (2, 1));
        assert!(
            d.mismatch().is_some(),
            "a write that stops landing must be visible"
        );
    }

    #[test]
    fn opt_out_deletes_the_log_and_the_flag() {
        let dir = tmp("optout");
        let mut d = ResolutionDiary::open(Some(dir.clone()), NOW);
        d.set_enabled(true).unwrap();
        d.record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .unwrap();
        assert!(dir.join(DIARY_FILE).exists());
        d.set_enabled(false).unwrap();
        assert!(!dir.join(DIARY_FILE).exists(), "opt-out erases the log");
        assert!(!d.enabled());
        assert!(d
            .record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .is_none());
    }

    // ---- consent gate (break-it lock) ----

    fn fresh_diary(tag: &str) -> (PathBuf, ResolutionDiary) {
        let dir = tmp(tag);
        let d = ResolutionDiary::open(Some(dir.clone()), NOW);
        (dir, d)
    }

    #[test]
    fn fresh_install_without_onboarding_collects_nothing() {
        // Break-it (negative): no recorded choice = off, however many bubbles resolve.
        let (dir, mut d) = fresh_diary("consent-none");
        for i in 0..5 {
            assert!(d
                .record(DiaryOutcome::Taken, "teh", "the", None, NOW + i)
                .is_none());
            assert!(d
                .record(DiaryOutcome::Ignored, "wrod", "word", None, NOW + i)
                .is_none());
        }
        assert!(!d.enabled());
        assert!(!dir.join(DIARY_FILE).exists(), "no log without consent");
        assert!(
            !dir.join(DEVICE_ID_FILE).exists(),
            "no device id without consent"
        );
        assert!(!dir.join(DIARY_FLAG).exists());
        assert_eq!(d.counts(), (0, 0));
    }

    #[test]
    fn completing_onboarding_starts_collection() {
        // Break-it (positive): consent recorded → the very next resolution is logged.
        let (dir, mut d) = fresh_diary("consent-yes");
        assert!(
            d.record(DiaryOutcome::Taken, "teh", "the", None, NOW)
                .is_none(),
            "off before consent"
        );
        assert_eq!(d.consent(), Ok(true));
        assert!(d.enabled());
        let id = d
            .record(DiaryOutcome::Taken, "teh", "the", None, NOW + 1)
            .expect("collects after consent");
        let (rows, _) = parse_lines(&std::fs::read_to_string(dir.join(DIARY_FILE)).unwrap());
        assert_eq!(
            rows.len(),
            1,
            "only the post-consent resolution is in the log"
        );
        assert_eq!(rows[0].id, id);
    }

    #[test]
    fn consent_survives_a_restart() {
        let (dir, mut d) = fresh_diary("consent-restart");
        d.consent().unwrap();
        let mut d2 = ResolutionDiary::open(Some(dir), NOW + 10);
        assert!(d2
            .record(DiaryOutcome::Ignored, "wrod", "word", None, NOW + 11)
            .is_some());
    }

    #[test]
    fn env_var_cannot_bypass_consent() {
        // The old dev opt-in env var must not be a back door around consent.
        let (dir, mut d) = fresh_diary("consent-env");
        std::env::set_var("TYPEASSIST_RESOLUTION_DIARY", "1");
        let got = d.record(DiaryOutcome::Taken, "teh", "the", None, NOW);
        std::env::remove_var("TYPEASSIST_RESOLUTION_DIARY");
        assert!(got.is_none());
        assert!(!dir.join(DIARY_FILE).exists());
    }

    #[test]
    fn settings_off_stops_collection_deletes_the_log_and_blocks_reconsent() {
        let (dir, mut d) = fresh_diary("consent-off");
        d.consent().unwrap();
        d.record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .unwrap();
        d.set_enabled(false).unwrap();
        assert!(!d.enabled());
        assert!(
            !dir.join(DIARY_FILE).exists(),
            "turning it off erases the note"
        );
        assert!(d
            .record(DiaryOutcome::Taken, "teh", "the", None, NOW + 1)
            .is_none());
        // A replayed/repeated onboarding must NOT override an explicit opt-out.
        assert_eq!(d.consent(), Ok(false));
        assert!(!d.enabled());
        assert!(d
            .record(DiaryOutcome::Taken, "teh", "the", None, NOW + 2)
            .is_none());
    }

    #[test]
    fn settings_on_after_off_resumes_collection() {
        let (_dir, mut d) = fresh_diary("consent-reon");
        d.consent().unwrap();
        d.set_enabled(false).unwrap();
        d.set_enabled(true).unwrap();
        assert!(d.enabled());
        assert!(d
            .record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .is_some());
    }

    // ---- retention + cap ----

    #[test]
    fn prune_drops_rows_older_than_14_days_and_keeps_the_rest() {
        let dir = tmp("prune");
        on(&dir);
        let rows = vec![
            ev("a", NOW - 15 * DAY, "x1", "y1", DiaryOutcome::Taken, None),
            ev("b", NOW - 13 * DAY, "x2", "y2", DiaryOutcome::Taken, None),
            ev("c", NOW - DAY, "x3", "y3", DiaryOutcome::Ignored, None),
        ];
        std::fs::write(dir.join(DIARY_FILE), serialize_lines(&rows)).unwrap();
        let _ = ResolutionDiary::open(Some(dir.clone()), NOW); // startup prune
        let (left, _) = parse_lines(&std::fs::read_to_string(dir.join(DIARY_FILE)).unwrap());
        let ids: Vec<&str> = left.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c"]);
    }

    #[test]
    fn size_cap_trims_oldest_first_and_logging_continues() {
        let mut rows = Vec::new();
        for i in 0..40_000u64 {
            rows.push(ev(
                &format!("id{i:06}"),
                NOW - 10 * DAY + i,
                "abcdefgh",
                "abcdefgi",
                DiaryOutcome::Ignored,
                None,
            ));
        }
        let kept = trim_to_cap(rows);
        let bytes: u64 = serialize_lines(&kept).len() as u64;
        assert!(bytes <= MAX_BYTES, "must fit the cap, got {bytes}");
        assert!(!kept.is_empty());
        assert_eq!(kept.last().unwrap().id, "id039999", "newest survives");
        assert_ne!(kept.first().unwrap().id, "id000000", "oldest dropped first");
    }

    #[test]
    fn corrupt_lines_are_skipped_not_fatal() {
        let good = serialize_lines(&[ev("a", NOW, "teh", "the", DiaryOutcome::Taken, None)]);
        let text = format!("{good}not json\n{{\"id\":\"\"}}\n");
        let (rows, skipped) = parse_lines(&text);
        assert_eq!(rows.len(), 1);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn future_dated_rows_are_ignored() {
        let rows = vec![
            ev("ok", NOW, "teh", "the", DiaryOutcome::Taken, None),
            ev(
                "future",
                NOW + 30 * DAY,
                "wrod",
                "word",
                DiaryOutcome::Taken,
                None,
            ),
        ];
        let kept = retained(rows, NOW);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "ok");
    }

    // ---- fold + summary ----

    #[test]
    fn retyped_supersedes_its_ignored_row_so_a_bubble_counts_once() {
        let rows = vec![
            ev("i1", NOW - 1000, "tp", "to", DiaryOutcome::Ignored, None),
            ev(
                "r1",
                NOW - 500,
                "tp",
                "to",
                DiaryOutcome::Retyped,
                Some("i1"),
            ),
            ev("i2", NOW - 400, "ahve", "have", DiaryOutcome::Ignored, None),
        ];
        let s = summarize(rows, NOW, true, 0);
        assert_eq!(
            (s.this_week.taken, s.this_week.retyped, s.this_week.ignored),
            (0, 1, 1)
        );
    }

    #[test]
    fn retyped_with_a_missing_ignored_row_still_counts_as_retyped() {
        let s = summarize(
            vec![ev(
                "r1",
                NOW - 5,
                "tp",
                "to",
                DiaryOutcome::Retyped,
                Some("gone"),
            )],
            NOW,
            true,
            0,
        );
        assert_eq!(s.this_week.retyped, 1);
        assert_eq!(s.this_week.ignored, 0);
    }

    #[test]
    fn week_over_week_split_and_real_words() {
        let rows = vec![
            ev("a", NOW - DAY, "teh", "the", DiaryOutcome::Taken, None),
            ev("b", NOW - 2 * DAY, "teh", "the", DiaryOutcome::Taken, None),
            ev(
                "c",
                NOW - 3 * DAY,
                "ahve",
                "have",
                DiaryOutcome::Ignored,
                None,
            ),
            ev(
                "d",
                NOW - 9 * DAY,
                "wrod",
                "word",
                DiaryOutcome::Taken,
                None,
            ),
            ev(
                "e",
                NOW - 20 * DAY,
                "old",
                "olde",
                DiaryOutcome::Taken,
                None,
            ),
        ];
        let s = summarize(rows, NOW, true, 0);
        assert_eq!((s.this_week.taken, s.this_week.ignored), (2, 1));
        assert_eq!(s.prior_week.taken, 1);
        assert_eq!(s.total_events, 4, "the 20-day-old row is outside retention");
        assert_eq!(
            s.this_week.taken_words,
            vec![WordCount {
                typed: "teh".into(),
                target: "the".into(),
                count: 2
            }]
        );
        assert_eq!(s.this_week.ignored_words[0].typed, "ahve");
        assert_eq!(s.prior_week.taken_words[0].typed, "wrod");
    }

    #[test]
    fn empty_log_reads_as_nothing_recorded_not_zeros_with_a_date() {
        let s = summarize(vec![], NOW, true, 0);
        assert_eq!((s.oldest_ts, s.newest_ts, s.total_events), (None, None, 0));
    }

    // ---- merge ----

    #[test]
    fn merge_is_a_union_by_id_and_idempotent() {
        let a = vec![
            ev("1", NOW - 3, "teh", "the", DiaryOutcome::Taken, None),
            ev("2", NOW - 2, "wrod", "word", DiaryOutcome::Ignored, None),
        ];
        let b = vec![
            ev("2", NOW - 2, "wrod", "word", DiaryOutcome::Ignored, None),
            ev("3", NOW - 1, "ahve", "have", DiaryOutcome::Taken, None),
        ];
        let (m, dups) = merge_events(a.clone(), b.clone());
        assert_eq!(dups, 1);
        assert_eq!(
            m.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["1", "2", "3"]
        );
        let (again, dups2) = merge_events(m.clone(), b);
        assert_eq!(again, m, "re-importing changes nothing");
        assert_eq!(dups2, 2);
        let (aa, _) = merge_events(a.clone(), a.clone());
        assert_eq!(aa, a);
    }

    #[test]
    fn merge_keeps_a_cross_mac_ignored_retyped_pair_counted_once() {
        // Mac A logged the ignored row; Mac B (same log, later) has the retyped row.
        let a = vec![ev("i", NOW - 10, "tp", "to", DiaryOutcome::Ignored, None)];
        let b = vec![ev(
            "r",
            NOW - 5,
            "tp",
            "to",
            DiaryOutcome::Retyped,
            Some("i"),
        )];
        let (m, _) = merge_events(a, b);
        let s = summarize(m, NOW, true, 0);
        assert_eq!((s.this_week.retyped, s.this_week.ignored), (1, 0));
    }

    #[test]
    fn import_file_merges_dedupes_and_is_idempotent_end_to_end() {
        let dir = tmp("imp-a");
        on(&dir);
        let own = vec![ev("1", NOW - 3, "teh", "the", DiaryOutcome::Taken, None)];
        std::fs::write(dir.join(DIARY_FILE), serialize_lines(&own)).unwrap();
        let other = tmp("imp-b");
        let inc = vec![
            ev("1", NOW - 3, "teh", "the", DiaryOutcome::Taken, None),
            ev("2", NOW - 2, "ahve", "have", DiaryOutcome::Ignored, None),
            ev("old", NOW - 30 * DAY, "x", "y", DiaryOutcome::Taken, None),
        ];
        let f = other.join("incoming.jsonl");
        std::fs::write(&f, format!("{}garbage\n", serialize_lines(&inc))).unwrap();
        let r = import_file(&dir, &f, NOW).unwrap();
        assert_eq!(
            (r.added, r.duplicates, r.skipped, r.expired, r.total),
            (1, 1, 1, 1, 2)
        );
        let r2 = import_file(&dir, &f, NOW).unwrap();
        assert_eq!((r2.added, r2.total), (0, 2), "second import adds nothing");
    }

    #[test]
    fn import_rejects_a_file_that_is_not_a_diary() {
        let dir = tmp("imp-bad");
        let f = dir.join("junk.txt");
        std::fs::write(&f, "hello\nworld\n").unwrap();
        assert!(import_file(&dir, &f, NOW).is_err());
        assert!(
            !dir.join(DIARY_FILE).exists(),
            "a rejected import writes nothing"
        );
    }

    #[test]
    fn export_round_trips_through_import() {
        let a = tmp("exp-a");
        on(&a);
        let mut d = ResolutionDiary::open(Some(a.clone()), NOW);
        d.record(DiaryOutcome::Taken, "teh", "the", None, NOW)
            .unwrap();
        d.record(DiaryOutcome::Ignored, "ahve", "have", None, NOW + 1)
            .unwrap();
        let out = a.join("export.jsonl");
        assert_eq!(export_to(&a, &out, NOW + 2).unwrap(), 2);
        let b = tmp("exp-b");
        let r = import_file(&b, &out, NOW + 2).unwrap();
        assert_eq!((r.added, r.total), (2, 2));
        let s = read_summary(&b, NOW + 2);
        assert_eq!((s.this_week.taken, s.this_week.ignored), (1, 1));
    }
}
