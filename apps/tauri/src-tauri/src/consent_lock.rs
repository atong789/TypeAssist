//! Break-it lock for the resolution-diary consent wiring (frontend side).
//!
//! There is no JS test runner in this repo, so these are source-level assertions over
//! the Svelte files. They exist to make the consent contract hard to break by accident:
//!
//! * consent is recorded in exactly ONE place — `completeOnboarding()` in `App.svelte`
//!   (every way out of onboarding lands there);
//! * nothing else auto-enables the diary: the only other caller of
//!   `set_resolution_diary` is the user's own Settings switch;
//! * the approved consent + privacy wording is present verbatim, and the old
//!   "keep no log of what you write" claim (now untrue) is gone.
//!
//! The behavioural half (fresh install = off; consent = on; explicit opt-out wins) is
//! pinned in `resolution_diary::tests`.

#![cfg(test)]

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../src")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(src_dir().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Collapse all whitespace runs so hard-wrapped Svelte copy compares as prose.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn frontend_files() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("svelte" | "ts")
            ) {
                out.push(p);
            }
        }
    }
    let mut paths = Vec::new();
    walk(&src_dir(), &mut paths);
    paths
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(src_dir())
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (rel, std::fs::read_to_string(&p).unwrap())
        })
        .collect()
}

/// The text of `async function completeOnboarding() { … }` (brace-balanced).
fn complete_onboarding_body(app: &str) -> String {
    let start = app
        .find("function completeOnboarding")
        .expect("completeOnboarding exists");
    let open = start + app[start..].find('{').expect("body");
    let mut depth = 0usize;
    for (i, c) in app[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return app[open..open + i + 1].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced completeOnboarding");
}

#[test]
fn consent_is_recorded_in_completeonboarding_and_nowhere_else() {
    let app = read("App.svelte");
    assert!(
        complete_onboarding_body(&app).contains("record_diary_consent"),
        "completeOnboarding() must record consent — otherwise a completed onboarding never starts the diary"
    );
    let callers: Vec<String> = frontend_files()
        .into_iter()
        .filter(|(_, src)| src.contains("record_diary_consent"))
        .map(|(rel, _)| rel)
        .collect();
    assert_eq!(
        callers,
        vec!["App.svelte".to_string()],
        "consent must have a single call site"
    );
}

#[test]
fn only_the_settings_switch_can_turn_the_diary_on_or_off() {
    let callers: Vec<String> = frontend_files()
        .into_iter()
        .filter(|(_, src)| src.contains("set_resolution_diary"))
        .map(|(rel, _)| rel)
        .collect();
    assert_eq!(
        callers,
        vec!["routes/Settings.svelte".to_string()],
        "nothing but the user's own Settings switch may call set_resolution_diary"
    );
    assert!(
        read("routes/Settings.svelte").contains("aria-label=\"14-day progress note\""),
        "off-switch must exist"
    );
}

#[test]
fn approved_consent_line_is_on_the_last_onboarding_step_between_the_two_sentences() {
    let ob = flat(&read("routes/Onboarding.svelte"));
    let want = "I’ll start learning straight away. To show you your progress, I keep a quiet 14-day note of the words you slip on and whether you took the word I offered — never your passwords, never the sentences around them, and never off this Mac. Whenever you want a warm-up";
    assert!(ob.contains(want), "approved consent line missing or moved");
}

#[test]
fn privacy_page_no_longer_claims_no_log_of_what_you_write() {
    let app = flat(&read("App.svelte"));
    assert!(
        !app.contains("keep no log of what you write"),
        "stale privacy claim still present"
    );
    assert!(
        app.contains(
            "keep no log of your sentences — only a quiet 14-day note of the words you slip on and what you did, so I can show your progress."
        ),
        "approved privacy wording missing"
    );
}

#[test]
fn consent_command_is_registered() {
    let lib = flat(
        &std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")).unwrap(),
    );
    assert!(lib.contains("fn record_diary_consent"));
    assert!(
        lib.contains("record_diary_consent, import_resolution_diary")
            || lib.contains("record_diary_consent,")
    );
}
