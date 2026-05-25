//! Streaming tokenizer — Component 1 of the L4 Observing brief.
//!
//! **Single source of word-boundary truth.** Both the future word-completion
//! trigger and the span anchor will go through this module to decide
//! "where does the current token end?". By construction they cannot disagree:
//! anything they ask of the keystroke stream is answered here, once.
//!
//! ## Contract (from the brief)
//!
//! Each emitted [`Token`] carries:
//! * `core` — the semantic content, case preserved.
//! * `start`, `end` — half-open char-index span of *core* within the current
//!   line. The span anchor will later track `[start, end)`.
//! * `leading`, `trailing` — punctuation stripped from before / after the core.
//! * `terminator` — the boundary char that sealed the token, or `None` when
//!   sealed by a forced finalize (end-of-input).
//! * `kind` — `Word | Number | Url | Email | Code | Acronym`.
//! * `correctable` — `true` for `Word` kind only. Everything else is observed
//!   but not corrected. Url/Email are reserved in the enum but not produced
//!   by this slice (they require boundary-rule overrides for `:` `/` `@`).
//! * `tokenizer_version` — bump [`TOKENIZER_VERSION`] whenever the
//!   classification or boundary rules change.
//!
//! ## Boundaries
//!
//! Whitespace (space/tab/newline/carriage return) + terminal punctuation
//! (`.` `,` `;` `:` `!` `?`) + closing brackets/quotes seal a token.
//! Apostrophe and hyphen are word characters and never split (`don't`,
//! `user's`, `well-known`).
//!
//! ## Period
//!
//! Periods are deferred one character so we can classify them in context:
//! * digits on both sides → internal (`3.14` → one `Number`).
//! * letters on at least one side → internal but the resulting kind is
//!   non-`Word` (`U.S.A`, `e.g` → `Acronym`, `correctable=false`).
//! * anything else after the period → period was terminal (`hello. ` →
//!   `Word "hello"` with `trailing="."`).
//!
//! ## Backspace
//!
//! The tokenizer is forward-only. Callers that need backspace support
//! maintain their own line buffer and rebuild the tokenizer from the new
//! buffer — cheap because lines are bounded. (See `apps/tauri/src-tauri/
//! src/engine.rs`.)

use serde::{Deserialize, Serialize};

/// Version of the tokenizer's classification + boundary rules.
/// Bump on every observable behaviour change; consumers can refuse stale
/// tokens by comparing this against `Token::tokenizer_version`.
///
/// v2 — kind classifier rewritten in specific→general order
/// (Number → Email → Url → Code → Acronym → Word). Acronym now keys off
/// all-caps alphabetic content (`ZAMS`, `NASA`), not off internal dots.
/// Url now catches bare domains (`books.soumyosinha.org`) in addition to
/// scheme and `www.` prefixes. Boundary logic / spans / Token shape
/// unchanged from v1.
pub const TOKENIZER_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TokenKind {
    /// Letters (with internal apostrophe/hyphen). The only correctable kind.
    Word,
    /// All-digit core, optionally with a digit-flanked period or a leading
    /// hyphen ("3.14", "-5").
    Number,
    /// Reserved — not yet detected by this slice.
    Url,
    /// Reserved — not yet detected by this slice.
    Email,
    /// Catch-all: mixed letters + digits, or anything we don't recognize as
    /// a Word/Number/Acronym.
    Code,
    /// Internal period(s) flanked by letters (`U.S.A`, `e.g`).
    Acronym,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Token {
    pub core: String,
    pub start: usize,
    pub end: usize,
    pub leading: String,
    pub trailing: String,
    pub terminator: Option<char>,
    pub kind: TokenKind,
    pub correctable: bool,
    pub tokenizer_version: u32,
}

#[derive(Debug, Default)]
pub struct Tokenizer {
    /// Char index of the NEXT character to be observed in the current line.
    pos: usize,
    /// Tokens sealed on the current line so far, in order.
    tokens: Vec<Token>,
    /// State of the in-progress (unsealed) token, if any.
    phase: Phase,
    leading: String,
    core: String,
    /// Position of the first char of `core` (valid in InCore/PendingDot/Trailing).
    start: usize,
    has_internal_period: bool,
    trailing: String,
    /// First boundary char that sealed the core. Sticks through Trailing so
    /// the emitted token reports what *closed* it, not the last char absorbed.
    terminator: Option<char>,
}

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
enum Phase {
    /// Between tokens. No in-progress content.
    #[default]
    Idle,
    /// Opening punctuation collected; no core characters yet.
    Leading,
    /// Building the core run of word characters.
    InCore,
    /// Just absorbed a `.` at the right edge of core; deciding internal vs
    /// terminal based on the next character.
    PendingDot,
    /// Core has been sealed by terminal/closing punctuation; absorbing any
    /// further closing/terminal chars into `trailing` until whitespace or a
    /// fresh token starts.
    Trailing,
}

impl Tokenizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// All sealed tokens on the current line, in order.
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Reset state to a fresh line. Called when the keystroke stream sees a
    /// newline, when the focused field changes, or when the caller needs to
    /// rebuild after a backspace.
    pub fn reset_line(&mut self) {
        *self = Self::default();
    }

    /// Feed one character of input. Returns `Some(Token)` iff this character
    /// sealed a token (whitespace, terminal/closing punctuation, or — when
    /// processed via [`Self::seal`] — end of input). Otherwise `None`.
    pub fn observe_char(&mut self, c: char) -> Option<Token> {
        // Newlines are hard line boundaries: seal any pending content,
        // reset state, return whatever was emitted (if anything). The caller
        // is responsible for noticing the line reset and resetting any UI.
        if c == '\n' || c == '\r' {
            let emitted = self.finalize(Some(c));
            self.reset_line();
            return emitted;
        }

        let result = match self.phase {
            Phase::Idle => self.from_idle(c),
            Phase::Leading => self.from_leading(c),
            Phase::InCore => self.from_in_core(c),
            Phase::PendingDot => self.from_pending_dot(c),
            Phase::Trailing => self.from_trailing(c),
        };
        self.pos += 1;
        result
    }

    /// Force-seal any pending content. Use at end of input (focus loss, file
    /// close, dev-time inspection) when there's no boundary char coming.
    pub fn seal(&mut self) -> Option<Token> {
        self.finalize(None)
    }

    // ---- State handlers --------------------------------------------------

    fn from_idle(&mut self, c: char) -> Option<Token> {
        if is_whitespace(c) {
            None
        } else if is_word_char(c) {
            self.start_core(c);
            None
        } else if is_opening_punct(c) {
            self.leading.push(c);
            self.phase = Phase::Leading;
            None
        } else {
            // Stray terminal/closing punctuation with no token to attach to —
            // discard silently. Keeps the stream robust against editor noise.
            None
        }
    }

    fn from_leading(&mut self, c: char) -> Option<Token> {
        if is_whitespace(c) {
            // Leading without core never had a token — discard.
            self.leading.clear();
            self.phase = Phase::Idle;
            None
        } else if is_word_char(c) {
            self.start_core(c);
            None
        } else if is_opening_punct(c) {
            self.leading.push(c);
            None
        } else {
            // Terminal/closing punctuation with no core: discard everything,
            // we never had a real token.
            self.leading.clear();
            self.phase = Phase::Idle;
            None
        }
    }

    fn from_in_core(&mut self, c: char) -> Option<Token> {
        if c == '.' {
            // Defer the decision — internal (3.14, U.S.A) or terminal (hello.).
            self.phase = Phase::PendingDot;
            None
        } else if is_word_char(c) {
            self.core.push(c);
            None
        } else if is_whitespace(c) {
            self.terminator = Some(c);
            self.emit_token()
        } else if is_terminal_punct(c) || is_closing_punct(c) {
            self.trailing.push(c);
            if self.terminator.is_none() {
                self.terminator = Some(c);
            }
            self.phase = Phase::Trailing;
            None
        } else if is_opening_punct(c) {
            // A new token's opening punctuation right against the previous
            // word — seal current and start fresh leading.
            let emitted = self.emit_token();
            self.leading.push(c);
            self.phase = Phase::Leading;
            emitted
        } else {
            // Unknown character class — treat conservatively as a word char.
            self.core.push(c);
            None
        }
    }

    fn from_pending_dot(&mut self, c: char) -> Option<Token> {
        if is_word_char(c) {
            // Period was internal — commit it and the new char to core.
            self.core.push('.');
            self.core.push(c);
            self.has_internal_period = true;
            self.phase = Phase::InCore;
            None
        } else {
            // Period was terminal. Materialize that fact, then re-process
            // `c` under the post-core Trailing rules.
            self.trailing.push('.');
            if self.terminator.is_none() {
                self.terminator = Some('.');
            }
            self.phase = Phase::Trailing;
            self.handle_in_trailing(c)
        }
    }

    fn from_trailing(&mut self, c: char) -> Option<Token> {
        self.handle_in_trailing(c)
    }

    fn handle_in_trailing(&mut self, c: char) -> Option<Token> {
        if is_whitespace(c) {
            self.emit_token()
        } else if is_terminal_punct(c) || is_closing_punct(c) {
            // Stack additional closing/terminal punctuation into trailing,
            // e.g. `hello.)` → trailing = ".)".
            self.trailing.push(c);
            None
        } else if is_word_char(c) {
            // No whitespace between trailing and a new word — seal and
            // start a fresh token at the current position.
            let emitted = self.emit_token();
            self.start_core(c);
            emitted
        } else if is_opening_punct(c) {
            let emitted = self.emit_token();
            self.leading.push(c);
            self.phase = Phase::Leading;
            emitted
        } else {
            None
        }
    }

    // ---- Helpers ---------------------------------------------------------

    fn start_core(&mut self, c: char) {
        self.core.push(c);
        self.start = self.pos;
        self.phase = Phase::InCore;
    }

    fn emit_token(&mut self) -> Option<Token> {
        if self.core.is_empty() {
            self.clear_pending();
            return None;
        }
        let token = self.build_token();
        self.tokens.push(token.clone());
        self.clear_pending();
        Some(token)
    }

    fn finalize(&mut self, term: Option<char>) -> Option<Token> {
        // Resolve a pending period as terminal at end-of-input.
        if self.phase == Phase::PendingDot {
            self.trailing.push('.');
            if self.terminator.is_none() {
                self.terminator = Some('.');
            }
            self.phase = Phase::Trailing;
        }
        // If we're sealing with an explicit boundary char and we haven't
        // already recorded one, use it. (Mostly relevant for newline finalize.)
        if self.terminator.is_none() {
            self.terminator = term;
        }
        let emitted = self.emit_token();
        self.phase = Phase::Idle;
        emitted
    }

    fn clear_pending(&mut self) {
        self.leading.clear();
        self.core.clear();
        self.start = 0;
        self.has_internal_period = false;
        self.trailing.clear();
        self.terminator = None;
        self.phase = Phase::Idle;
    }

    fn build_token(&self) -> Token {
        let end = self.start + self.core.chars().count();
        let kind = classify(&self.core, self.has_internal_period);
        let correctable = matches!(kind, TokenKind::Word);
        Token {
            core: self.core.clone(),
            start: self.start,
            end,
            leading: self.leading.clone(),
            trailing: self.trailing.clone(),
            terminator: self.terminator,
            kind,
            correctable,
            tokenizer_version: TOKENIZER_VERSION,
        }
    }
}

// ---- Classification --------------------------------------------------------

/// Classify a sealed core into a `TokenKind`. Pure function of the core text
/// and whether we observed an internal period during streaming.
///
/// **Order: specific → general.** Number → Email → Url → Code → Acronym →
/// Word. Acronym keys off **all-caps alphabetic** content (`ZAMS`,
/// `NASA`), never off dots. Url includes bare domains
/// (`books.soumyosinha.org`) — a dot-separated token whose final segment is
/// an alpha TLD-like run of length 2–6.
fn classify(core: &str, has_internal_period: bool) -> TokenKind {
    let chars: Vec<char> = core.chars().collect();
    if chars.is_empty() {
        return TokenKind::Code;
    }
    let has_letter = chars.iter().any(|c| c.is_ascii_alphabetic());
    let has_digit = chars.iter().any(|c| c.is_ascii_digit());
    let all_numeric_like = chars
        .iter()
        .all(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == ',');

    // 1. Number — pure digits with optional separators, no letters.
    if all_numeric_like && has_digit && !has_letter {
        return TokenKind::Number;
    }

    // 2. Email — contains '@' with surrounding text. Not produced by the
    //    current streamer (no '@' in `is_word_char`) but the classifier
    //    handles it for when that changes / for test inputs constructed
    //    directly.
    if has_letter && core.contains('@') {
        return TokenKind::Email;
    }

    // 3. Url — http(s):// or www. prefix, OR bare domain (dotted, last
    //    segment alpha length 2–6).
    if is_url_like(core) {
        return TokenKind::Url;
    }

    // 4. Code — mixed letters and digits, or an internal-period dotted run
    //    that isn't a URL (`U.S.A`, `e.g`, `foo.bar` w/ short final). Code
    //    is the "structured non-word" bucket; never correctable.
    if has_letter && has_digit {
        return TokenKind::Code;
    }
    if has_internal_period {
        return TokenKind::Code;
    }

    // 5. Acronym — all-caps alphabetic, length ≥ 2. Pure-letter content
    //    only — does NOT key off dots.
    if chars.len() >= 2
        && chars
            .iter()
            .all(|c| c.is_ascii_alphabetic() && c.is_ascii_uppercase())
    {
        return TokenKind::Acronym;
    }

    // 6. Word — pure letters (with optional internal apostrophe / hyphen)
    //    that didn't trip Acronym. The only correctable kind.
    if has_letter && !has_digit {
        return TokenKind::Word;
    }

    // Fallback: e.g. a lone apostrophe or hyphen that somehow reached here.
    TokenKind::Code
}

/// Bare-domain detection for the Url path.
///
/// True for: an `http://` / `https://` scheme, a `www.` prefix, OR a
/// dot-separated token whose **final** segment is an alphabetic TLD-like
/// run of length 2–6 (`books.soumyosinha.org`, `example.com`).
///
/// False for `U.S.A` / `e.g` (final segment too short) — those fall through
/// to `Code` via the internal-period branch.
fn is_url_like(core: &str) -> bool {
    let lower = core.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") {
        return true;
    }
    let segments: Vec<&str> = core.split('.').collect();
    if segments.len() < 2 {
        return false;
    }
    let Some(last) = segments.last() else {
        return false;
    };
    let len = last.chars().count();
    (2..=6).contains(&len) && last.chars().all(|c| c.is_ascii_alphabetic())
}

// ---- Character predicates --------------------------------------------------
// Tiny helpers, deliberately not exposed: the tokenizer is the only thing
// that needs to ask these questions, and any consumer pulling them out would
// risk drifting from the boundary definitions above.

fn is_whitespace(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\n' || c == '\r'
}

/// Word character: ASCII alphanumeric plus apostrophe and hyphen. Apostrophe
/// and hyphen never split a word ("don't", "user's", "well-known").
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '\'' || c == '-'
}

fn is_opening_punct(c: char) -> bool {
    matches!(c, '(' | '[' | '{' | '"' | '<' | '`' | '«' | '“' | '‘')
}

fn is_closing_punct(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '"' | '>' | '`' | '»' | '”' | '’')
}

fn is_terminal_punct(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '!' | '?')
}

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed every char in `s`, return the tokens emitted (in order).
    fn tokenize(s: &str) -> Vec<Token> {
        let mut t = Tokenizer::new();
        let mut out = Vec::new();
        for c in s.chars() {
            if let Some(tok) = t.observe_char(c) {
                out.push(tok);
            }
        }
        if let Some(tok) = t.seal() {
            out.push(tok);
        }
        out
    }

    fn one(s: &str) -> Token {
        let toks = tokenize(s);
        assert_eq!(toks.len(), 1, "expected exactly one token from {s:?}, got {toks:?}");
        toks.into_iter().next().unwrap()
    }

    #[test]
    fn plain_word_sealed_by_space() {
        let t = one("hello ");
        assert_eq!(t.core, "hello");
        assert_eq!(t.start, 0);
        assert_eq!(t.end, 5);
        assert_eq!(t.leading, "");
        assert_eq!(t.trailing, "");
        assert_eq!(t.terminator, Some(' '));
        assert_eq!(t.kind, TokenKind::Word);
        assert!(t.correctable);
        assert_eq!(t.tokenizer_version, TOKENIZER_VERSION);
    }

    #[test]
    fn apostrophe_stays_inside_core() {
        let t = one("don't ");
        assert_eq!(t.core, "don't");
        assert_eq!(t.kind, TokenKind::Word);
        assert!(t.correctable);

        let t = one("user's ");
        assert_eq!(t.core, "user's");
        assert_eq!(t.kind, TokenKind::Word);
    }

    #[test]
    fn hyphen_stays_inside_core() {
        let t = one("well-known ");
        assert_eq!(t.core, "well-known");
        assert_eq!(t.kind, TokenKind::Word);
        assert!(t.correctable);
    }

    #[test]
    fn digit_flanked_period_is_internal_number() {
        let t = one("3.14 ");
        assert_eq!(t.core, "3.14");
        assert_eq!(t.kind, TokenKind::Number);
        // Per the brief: only Word is correctable.
        assert!(!t.correctable);
    }

    #[test]
    fn period_at_end_of_word_is_terminal_trailing() {
        let t = one("hello. ");
        assert_eq!(t.core, "hello");
        assert_eq!(t.trailing, ".");
        assert_eq!(t.terminator, Some('.'));
        assert_eq!(t.kind, TokenKind::Word);
        assert!(t.correctable);
        assert_eq!(t.start, 0);
        assert_eq!(t.end, 5);
    }

    #[test]
    fn dotted_letter_run_with_short_final_is_code() {
        // U.S.A: internal period letter-run; final segment "A" is too short
        // to be a TLD (rule: alpha length 2–6). Falls through to Code via
        // the internal-period branch — NOT Acronym (Acronym now keys off
        // all-caps *alphabetic* content, not dots). Either way, not
        // correctable, which is the strict-filter behaviour we need.
        let t = one("U.S.A ");
        assert_eq!(t.core, "U.S.A");
        assert_eq!(t.kind, TokenKind::Code);
        assert!(!t.correctable);
    }

    #[test]
    fn case_is_preserved_in_core() {
        let t = one("HeLlO ");
        assert_eq!(t.core, "HeLlO");
    }

    #[test]
    fn comma_seals_with_terminator_set() {
        let toks = tokenize("hello, world ");
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].core, "hello");
        assert_eq!(toks[0].trailing, ",");
        assert_eq!(toks[0].terminator, Some(','));
        assert_eq!(toks[1].core, "world");
        assert_eq!(toks[1].start, 7); // after "hello, "
        assert_eq!(toks[1].end, 12);
    }

    #[test]
    fn closing_bracket_seals() {
        let t = one("hello) ");
        assert_eq!(t.core, "hello");
        assert_eq!(t.trailing, ")");
        assert_eq!(t.terminator, Some(')'));
    }

    #[test]
    fn opening_bracket_is_leading() {
        let t = one("(hello) ");
        assert_eq!(t.leading, "(");
        assert_eq!(t.core, "hello");
        assert_eq!(t.trailing, ")");
        // Start is the index of the first core char — *after* the leading paren.
        assert_eq!(t.start, 1);
        assert_eq!(t.end, 6);
    }

    #[test]
    fn quoted_word() {
        let t = one(r#""hello" "#);
        assert_eq!(t.leading, "\"");
        assert_eq!(t.core, "hello");
        assert_eq!(t.trailing, "\"");
    }

    #[test]
    fn trailing_absorbs_multiple_punctuation() {
        let t = one("hello.) ");
        assert_eq!(t.core, "hello");
        assert_eq!(t.trailing, ".)");
        // First terminator wins — the period sealed; the paren stacked on.
        assert_eq!(t.terminator, Some('.'));
    }

    #[test]
    fn end_of_input_seals_without_explicit_boundary() {
        let t = one("hello");
        assert_eq!(t.core, "hello");
        assert_eq!(t.terminator, None);
        assert_eq!(t.kind, TokenKind::Word);
    }

    #[test]
    fn newline_seals_and_resets_line() {
        let mut tok = Tokenizer::new();
        let mut emitted = Vec::new();
        for c in "hello\nworld ".chars() {
            if let Some(t) = tok.observe_char(c) {
                emitted.push(t);
            }
        }
        assert_eq!(emitted.len(), 2);
        assert_eq!(emitted[0].core, "hello");
        assert_eq!(emitted[1].core, "world");
        // Newline reset the line — second token starts at 0 again, not 6.
        assert_eq!(emitted[1].start, 0);
    }

    #[test]
    fn empty_input_yields_no_tokens() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
    }

    #[test]
    fn stray_punctuation_alone_yields_no_token() {
        assert!(tokenize("...").is_empty());
        // Leading punctuation followed by whitespace also discards.
        assert!(tokenize("( ").is_empty());
    }

    #[test]
    fn numeric_with_leading_minus_is_number() {
        let t = one("-5 ");
        assert_eq!(t.core, "-5");
        assert_eq!(t.kind, TokenKind::Number);
    }

    #[test]
    fn mixed_letters_and_digits_is_code() {
        let t = one("abc123 ");
        assert_eq!(t.core, "abc123");
        assert_eq!(t.kind, TokenKind::Code);
        assert!(!t.correctable);
    }

    #[test]
    fn dotted_short_final_eg_is_code() {
        // "e.g": final segment "g" is alpha but length 1 → not a TLD →
        // falls through to Code via the internal-period branch. Not Acronym
        // (lowercase). Strict-filter outcome (not correctable) unchanged.
        let t = one("e.g ");
        assert_eq!(t.core, "e.g");
        assert_eq!(t.kind, TokenKind::Code);
        assert!(!t.correctable);
    }

    #[test]
    fn bare_domain_two_segments_is_url() {
        // hello.world: final segment "world" is alpha length 5 (TLD-like).
        // Bare-domain rule fires → Url. Some false positives expected on
        // sentence-like inputs; intentional per the brief's bare-domain rule.
        let t = one("hello.world ");
        assert_eq!(t.core, "hello.world");
        assert_eq!(t.kind, TokenKind::Url);
        assert!(!t.correctable);
    }

    #[test]
    fn deterministic_across_runs() {
        // Same input twice → byte-identical token streams. Guards against
        // accidental HashMap ordering or RNG creeping in.
        let a = tokenize("Hello, well-known world. 3.14 U.S.A");
        let b = tokenize("Hello, well-known world. 3.14 U.S.A");
        assert_eq!(a, b);
    }

    #[test]
    fn tokenizer_version_is_attached_to_every_token() {
        for t in tokenize("hello world! 3.14") {
            assert_eq!(t.tokenizer_version, TOKENIZER_VERSION);
        }
    }

    #[test]
    fn correctable_is_word_only() {
        for t in tokenize("hello 3.14 U.S.A abc123 ") {
            let expected = matches!(t.kind, TokenKind::Word);
            assert_eq!(t.correctable, expected, "{:?}", t);
        }
    }

    #[test]
    fn span_covers_core_only_not_leading_or_trailing() {
        let t = one("(hello). ");
        // leading "(", core "hello", trailing ").":
        assert_eq!(t.leading, "(");
        assert_eq!(t.core, "hello");
        assert_eq!(t.trailing, ").");
        // Span is exactly [start, end) of CORE.
        assert_eq!(t.start, 1);
        assert_eq!(t.end, 6);
        // Terminator is the FIRST boundary that sealed — ')' here.
        assert_eq!(t.terminator, Some(')'));
    }

    // ---- Regression tests for the v2 classifier ------------------------

    #[test]
    fn all_caps_alphabetic_word_is_acronym() {
        // ZAMS: all-caps, alphabetic, length 4 — matches the new Acronym
        // rule. Must NOT be Word/correctable.
        let t = one("ZAMS ");
        assert_eq!(t.core, "ZAMS");
        assert_eq!(t.kind, TokenKind::Acronym);
        assert!(!t.correctable);
    }

    #[test]
    fn nasa_two_letter_caps_is_acronym() {
        let t = one("NASA ");
        assert_eq!(t.kind, TokenKind::Acronym);
        assert!(!t.correctable);
    }

    #[test]
    fn single_capital_letter_is_word_not_acronym() {
        // "I" — length 1, fails the ≥ 2 rule, stays Word/correctable.
        let t = one("I ");
        assert_eq!(t.kind, TokenKind::Word);
        assert!(t.correctable);
    }

    #[test]
    fn proper_noun_with_one_capital_is_word() {
        // "Hello" — mixed case, not all-caps → stays Word/correctable.
        let t = one("Hello ");
        assert_eq!(t.kind, TokenKind::Word);
        assert!(t.correctable);
    }

    #[test]
    fn bare_domain_three_segments_is_url() {
        // books.soumyosinha.org: final segment "org" is alpha length 3 →
        // bare-domain rule fires → Url. Must NOT be Acronym (the v1 bug).
        let t = one("books.soumyosinha.org ");
        assert_eq!(t.core, "books.soumyosinha.org");
        assert_eq!(t.kind, TokenKind::Url);
        assert!(!t.correctable);
    }

    #[test]
    fn two_segment_dotcom_is_url() {
        let t = one("example.com ");
        assert_eq!(t.kind, TokenKind::Url);
    }

    #[test]
    fn www_prefix_is_url() {
        // www.example.com — even without the dot-segment heuristic, the
        // www. prefix on its own qualifies.
        let t = one("www.example.com ");
        assert_eq!(t.kind, TokenKind::Url);
    }

    #[test]
    fn mid_line_insert_rebuild_produces_correct_token() {
        // Mirrors the engine's behaviour after the user types "color",
        // Left, "u", space. The engine rebuilds the tokenizer from the
        // line buffer after the mid-line 'u' insert, so the sealed token
        // is "colour" — not the forward-only "coloru" that streaming
        // alone would produce. We test the rebuild outcome directly.
        let toks = tokenize("colour ");
        assert_eq!(toks.len(), 1);
        assert_eq!(toks[0].core, "colour");
        assert_eq!((toks[0].start, toks[0].end), (0, 6));
        assert_eq!(toks[0].kind, TokenKind::Word);
        assert!(toks[0].correctable);
    }

    #[test]
    fn classifier_does_not_break_pre_existing_kinds() {
        // 3.14 stays Number, don't / well-known stay Word — guard the
        // happy-path cases the regression brief calls out.
        assert_eq!(one("3.14 ").kind, TokenKind::Number);
        assert_eq!(one("don't ").kind, TokenKind::Word);
        assert_eq!(one("well-known ").kind, TokenKind::Word);
    }

    #[test]
    fn multiple_tokens_in_one_burst() {
        let toks = tokenize("the quick brown fox ");
        assert_eq!(toks.len(), 4);
        let cores: Vec<&str> = toks.iter().map(|t| t.core.as_str()).collect();
        assert_eq!(cores, vec!["the", "quick", "brown", "fox"]);
        // Positions chain correctly across whitespace.
        assert_eq!((toks[0].start, toks[0].end), (0, 3));
        assert_eq!((toks[1].start, toks[1].end), (4, 9));
        assert_eq!((toks[2].start, toks[2].end), (10, 15));
        assert_eq!((toks[3].start, toks[3].end), (16, 19));
    }
}
