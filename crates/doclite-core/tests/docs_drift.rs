//! Every identifier the prose names in backticks still exists in the source,
//! and every link it makes resolves.
//!
//! The ADRs and the blog are both living documents, so both go stale the same
//! way and are held to the same check. The blog is the reason the second half
//! exists: its posts are numbered and cross-linked, and renaming one is exactly
//! the change that breaks a link with nothing to notice.
//!
//! Fenced code blocks are stripped before backticks are paired. A fence is
//! itself made of backticks, so pairing first silently swallows roughly one
//! identifier in eight.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Words that read as identifiers but are prose here, in three groups.
///
/// Names belonging to somebody else: a system call, another crate's method, a
/// SQL keyword, a pragma. The prose has to be able to say what it is comparing
/// this design against.
const BORROWED: &[&str] = &[
    "memcmp",
    "rustc",
    "fsync",
    "compile_fail",
    "spawn_blocking",
    "sqlite3",
    "bigint",
    "as_blob",
    "DISTINCT",
    "COMMIT",
    "REAL",
    "UNIQUE",
    "UPDATE",
    "SQLITE_BUSY",
    "foreign_keys",
];

/// What the design deliberately lacks: "there is no `begin`", "no `by_id` on a
/// query builder", "explicit `ABORT` is not missing either". A name that is
/// absent on purpose cannot resolve, and saying so is the writing rather than
/// drift.
const ABSENT: &[&str] = &["begin", "by_id", "ABORT", "BEGIN", "RETURN"];

/// Cargo's own vocabulary, which `docs/releasing.md` is entirely about.
const CARGO: &[&str] = &["cargo", "package", "categories", "LICENSE"];

/// Where prose lives, relative to the workspace root.
///
/// `docs` itself rather than only its subdirectories: the spec, the engine
/// design and the pushdown note go stale exactly the way an ADR does, and a
/// renamed function in one of them is the drift nothing else would catch.
const DOCS: &[&str] = &["docs", "docs/adr", "docs/blog"];

/// The index pages, which describe a directory rather than the code and so are
/// link-checked and not identifier-checked.
///
/// Listed rather than derived from [`DOCS`], so a renamed one fails here
/// instead of quietly dropping out of the walk.
const INDEXES: &[&str] = &["docs/adr/README.md", "docs/blog/README.md"];

#[test]
fn every_identifier_the_prose_names_still_exists() {
    let root = repo_root();
    let source = read_all_source(&root.join("crates"));
    let mut missing = Vec::new();

    for document in prose(&root) {
        let text = fs::read_to_string(&document).expect("a document is readable");
        let name = shown(&root, &document);

        for identifier in identifiers(&strip_fences(&text)) {
            if !mentions(&source, &identifier) {
                missing.push(format!("{name}: `{identifier}`"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "identifiers no longer in the source:\n{}",
        missing.join("\n")
    );
}

/// Every item a code sample imports, and every attribute it writes, still
/// exists.
///
/// The samples are fragments and cannot be doctests: they name types the
/// surrounding prose never defines, and making them compile would turn the
/// README into a dozen self-contained programs rather than a document. This is
/// the part of them that can be checked, and it is the part that rots: a
/// renamed export or a renamed attribute.
///
/// It does not check that a signature still matches, only that the name is
/// still there. A sample calling a method that has grown an argument still
/// passes here.
#[test]
fn every_item_a_sample_imports_still_exists() {
    let root = repo_root();
    let source = read_all_source(&root.join("crates"));
    let mut missing = Vec::new();

    let mut documents = prose(&root);
    documents.push(root.join("README.md"));
    documents.push(root.join("CONTEXT.md"));

    for document in documents {
        let text = fs::read_to_string(&document).expect("a document is readable");
        let name = shown(&root, &document);

        for identifier in shown_in_code(&text) {
            if !mentions(&source, &identifier) {
                missing.push(format!("{name}: `{identifier}`"));
            }
        }
    }

    assert!(
        missing.is_empty(),
        "code samples naming what is no longer there:\n{}",
        missing.join("\n")
    );
}

/// Every attribute the derive accepts is written down somewhere a reader looks.
///
/// The derive's `attributes(..)` list is the whole set a caller may write, and
/// adding one is exactly the change that leaves the prose enumerating the old
/// set. The identifier walk cannot catch that: an attribute nobody documents is
/// absent from the docs rather than wrong in them, and absence is what a walk
/// over the docs has nothing to trip on.
///
/// The README is the reference and the tour is where a reader meets a feature,
/// so an attribute has to reach both.
#[test]
fn every_attribute_the_derive_accepts_is_documented() {
    let root = repo_root();
    let readme = fs::read_to_string(root.join("README.md")).expect("the readme is readable");
    let tour: String = markdown(&root.join("docs/blog"))
        .iter()
        .map(|post| fs::read_to_string(post).expect("a post is readable"))
        .collect();

    let mut undocumented = Vec::new();
    for attribute in accepted(&root) {
        let written = format!("#[{attribute}");
        if !readme.contains(&written) {
            undocumented.push(format!("README.md: `{written}`"));
        }
        if !tour.contains(&written) {
            undocumented.push(format!("docs/blog: `{written}`"));
        }
    }

    assert!(
        undocumented.is_empty(),
        "attributes the derive accepts and nothing describes:\n{}",
        undocumented.join("\n")
    );
}

/// The names inside `#[proc_macro_derive(Document, attributes(..))]`.
///
/// Read out of the source rather than listed here, so this test cannot be the
/// thing that goes stale.
fn accepted(root: &Path) -> Vec<String> {
    let source = fs::read_to_string(root.join("crates/doclite-derive/src/lib.rs"))
        .expect("the derive crate has a lib.rs");
    let at = source
        .find("attributes(")
        .expect("the Document derive accepts attributes");
    let rest = &source[at + "attributes(".len()..];
    let inside = &rest[..rest.find(')').expect("the attribute list closes")];

    let names: Vec<String> = inside
        .split(',')
        .map(|name| name.trim().to_owned())
        .filter(|name| is_identifier(name))
        .collect();
    assert!(!names.is_empty(), "no attributes were found at all");
    names
}

/// Every relative link in the prose and the README points at a file.
///
/// Only relative ones: an `http` link is somebody else's to keep alive, and a
/// fragment names a heading rather than a file.
#[test]
fn every_link_resolves() {
    let root = repo_root();
    let mut documents = prose(&root);
    documents.push(root.join("README.md"));
    documents.push(root.join("CONTEXT.md"));
    for index in INDEXES {
        documents.push(root.join(index));
    }

    let mut broken = Vec::new();
    for document in documents {
        let text = match fs::read_to_string(&document) {
            Ok(text) => text,
            Err(error) => panic!("cannot read {}: {error}", document.display()),
        };
        let beside = document.parent().unwrap_or(&root).to_path_buf();

        for target in links(&text) {
            if beside.join(&target).exists() {
                continue;
            }
            broken.push(format!("{}: {target}", shown(&root, &document)));
        }
    }

    assert!(
        broken.is_empty(),
        "links pointing at nothing:\n{}",
        broken.join("\n")
    );
}

/// Every markdown document under [`DOCS`], README indexes excluded.
///
/// An index is skipped for the identifier walk the same way it always was: it
/// describes the directory rather than the code.
fn prose(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = DOCS.iter().flat_map(|d| markdown(&root.join(d))).collect();
    found.sort();
    found
}

/// A path as the failure message should print it: relative to the root.
fn shown(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// The target of every inline markdown link, minus anchors and absolute URLs.
fn links(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("](") {
        rest = &rest[open + 2..];
        let Some(close) = rest.find(')') else { break };
        let target = &rest[..close];
        rest = &rest[close + 1..];

        let target = target.split('#').next().unwrap_or(target);
        if target.is_empty() || target.contains("://") || target.starts_with('<') {
            continue;
        }
        found.push(target.to_owned());
    }
    found
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels under the workspace root")
        .to_path_buf()
}

fn markdown(directory: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|kind| kind == "md"))
        .filter(|path| path.file_name().is_some_and(|name| name != "README.md"))
        .collect();
    found.sort();
    assert!(
        !found.is_empty(),
        "{} holds no documents",
        directory.display()
    );
    found
}

/// Every name a Rust code block imports or writes as an attribute.
///
/// Only those two positions. A local binding is named by whoever wrote the
/// sample and means nothing to the source, so widening this to every token
/// would be a list of `conn` and `user` to keep excusing.
fn shown_in_code(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for line in fenced_rust(text).lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("use ") {
            found.extend(imported(rest));
        }
        if let Some(rest) = line.strip_prefix("#[") {
            let name = rest.split(['(', ']']).next().unwrap_or_default();
            if is_identifier(name) {
                found.insert(name.to_owned());
            }
        }
    }
    found
}

/// The items a `use` line brings in, by their last segment.
fn imported(rest: &str) -> Vec<String> {
    let rest = rest.trim_end_matches(';');
    let inside = match (rest.find('{'), rest.rfind('}')) {
        (Some(open), Some(close)) if open < close => &rest[open + 1..close],
        _ => rest,
    };
    inside
        .split(',')
        .filter_map(|item| path_tail(item.trim()))
        .collect()
}

/// Everything inside a ```rust fence, and nothing outside one.
///
/// The opposite of [`strip_fences`], which the prose walk uses: a sample is
/// exactly what that one throws away.
fn fenced_rust(text: &str) -> String {
    let mut kept = String::new();
    let mut inside = false;
    for line in text.lines() {
        let fence = line.trim_start();
        if fence.starts_with("```") {
            inside = !inside && matches!(fence.trim_end(), "```rust" | "```rust,no_run");
            continue;
        }
        if inside {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

/// Drops everything between a pair of fences, and the fences themselves.
fn strip_fences(text: &str) -> String {
    let mut kept = String::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            inside = !inside;
            continue;
        }
        if !inside {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

/// The last segment of every backticked span that looks like a Rust path.
///
/// Splitting on a backtick makes every odd piece the inside of a pair, which is
/// only true once the fences are gone.
fn identifiers(text: &str) -> BTreeSet<String> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter_map(path_tail)
        .collect()
}

fn path_tail(span: &str) -> Option<String> {
    let bare = span.split('(').next().unwrap_or(span);
    if bare.is_empty() {
        return None;
    }
    let segments: Vec<&str> = bare.split("::").collect();
    if !segments.iter().all(|segment| is_identifier(segment)) {
        return None;
    }
    let last = segments.last().copied().unwrap_or(bare);
    if [BORROWED, ABSENT, CARGO]
        .iter()
        .any(|group| group.contains(&last))
    {
        return None;
    }
    Some(last.to_owned())
}

fn is_identifier(text: &str) -> bool {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn read_all_source(directory: &Path) -> String {
    let mut text = String::new();
    collect(directory, &mut text);
    text
}

fn collect(directory: &Path, into: &mut String) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => panic!("cannot read {}: {error}", directory.display()),
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, into);
        } else if path.extension().is_some_and(|kind| kind == "rs") {
            let text = fs::read_to_string(&path).expect("a source file is readable");
            into.push_str(&strip_comments(&text));
        }
    }
}

/// Drops every comment line, so an identifier has to resolve against code.
///
/// Without this a bare lowercase name resolves against ordinary English in a
/// doc comment: `keep` once matched the word "keep" in a sentence and the ADR
/// naming it went stale unnoticed.
fn strip_comments(text: &str) -> String {
    let mut kept = String::new();
    for line in text.lines() {
        if !line.trim_start().starts_with("//") {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    kept
}

/// Whole-word match, so `Row` does not resolve just because `RowKey` exists.
fn mentions(source: &str, identifier: &str) -> bool {
    source.match_indices(identifier).any(|(at, _)| {
        let before = source[..at].chars().next_back();
        let after = source[at + identifier.len()..].chars().next();
        free(before) && free(after)
    })
}

fn free(neighbour: Option<char>) -> bool {
    !matches!(neighbour, Some(character) if character.is_ascii_alphanumeric() || character == '_')
}
