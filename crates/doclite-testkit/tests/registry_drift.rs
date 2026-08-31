//! The `conformance!` list is every rule, and every rule is on it.
//!
//! The list is hand-written inside a macro, so a rule added to `cases.rs` and
//! not to the list compiles, is public, never runs, and warns about nothing.
//! Nothing but this says otherwise.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn every_rule_is_registered_and_every_registration_is_a_rule() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let written: BTreeSet<String> = ["cases.rs", "programs.rs", "properties.rs"]
        .iter()
        .flat_map(|file| declared(&source.join(file)))
        .collect();
    let listed = registered(&source.join("lib.rs"));

    assert!(!written.is_empty(), "no rules were found at all");

    let unlisted: Vec<&String> = written.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "rules nothing runs:\n{}",
        joined(&unlisted)
    );

    let unwritten: Vec<&String> = listed.difference(&written).collect();
    assert!(
        unwritten.is_empty(),
        "names on the list that are not rules:\n{}",
        joined(&unwritten)
    );
}

fn joined(names: &[&String]) -> String {
    names
        .iter()
        .map(|name| name.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `pub async fn` a rule module declares.
fn declared(file: &PathBuf) -> Vec<String> {
    let text = fs::read_to_string(file).expect("a rule module is readable");
    text.lines()
        .filter_map(|line| line.strip_prefix("pub async fn "))
        .filter_map(|rest| rest.split(['<', '(']).next())
        .map(str::to_owned)
        .collect()
}

/// Every name inside the `rules: [ .. ]` block of `conformance!`.
fn registered(file: &PathBuf) -> BTreeSet<String> {
    let text = fs::read_to_string(file).expect("lib.rs is readable");
    let opened = text.find("rules: [").expect("conformance! lists its rules");
    let body = &text[opened..];
    let closed = body.find(']').expect("the list is closed");

    body[..closed]
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_suffix(','))
        .filter(|name| name.chars().all(|c| c.is_ascii_lowercase() || c == '_'))
        .map(str::to_owned)
        .collect()
}
