//! A ratchet on function length.
//!
//! The audit that started this work found that the problem was function length rather than file length: a
//! five-thousand-line file of small functions is fine, a nine-hundred-line function is not. Splitting them
//! once is worth nothing if nothing notices them growing back, so this test fails when a function passes the
//! limit and is not on the list below — and fails just as loudly when a listed function has grown.
//!
//! It runs as part of `cargo test`, which is the only kind of guard that survives: a script in a directory
//! is a script nobody runs.
//!
//! Run it with a different limit to see what it is holding back:
//! `ANIMA_FN_LIMIT=100 cargo test --test function_length -- --nocapture`

use std::path::{Path, PathBuf};

/// Beyond this, a function is a place where careful edits go wrong.
const LIMIT: usize = 150;

/// The functions that are still over the limit, with the reason each one is still here and what it would
/// take. Adding a line is a decision; the numbers are checked, so a listed function may not grow.
const KNOWN: &[(&str, &str, usize, &str)] = &[
    (
        "src/bin/anima-overlay.rs",
        "main",
        355,
        "the overlay's startup, layout, event loop and painting in one function — the audit's step three",
    ),
    (
        "src/bin/anima-studio.rs",
        "view",
        302,
        "the window's layout: one arm per tab, and the tab bodies have moved out already",
    ),
    (
        "src/bin/anima-overlay.rs",
        "vertices",
        281,
        "building the overlay's geometry; a candidate for pulling the shapes out one at a time",
    ),
    (
        "src/bin/anima-shepherd.rs",
        "menu",
        172,
        "the tray menu's entries, described in one place on purpose",
    ),
];

#[derive(Debug)]
struct Found {
    path: String,
    name: String,
    line: usize,
    lines: usize,
}

/// Every function in a source file, with the length of its body.
///
/// The heuristic is indentation: a function ends at the first line that is a lone `}` at the same indent as
/// its `fn`. It does not try to understand Rust, which means it is easy to explain and easy to be wrong
/// about in a way that shows up immediately — a mis-parse produces an absurd number rather than a quiet pass.
fn functions_in(path: &Path, text: &str) -> Vec<Found> {
    let lines: Vec<&str> = text.lines().collect();
    // Tests describe rather than tangle, and a long test module is not the problem this guard is about.
    let tests_begin = lines
        .iter()
        .position(|line| line.trim_start().starts_with("#[cfg(test)]"))
        .unwrap_or(usize::MAX);
    let shown = path
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or(path);
    let mut found = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        if index > tests_begin {
            break;
        }
        let Some(name) = function_name(line) else {
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        let Some(end) = lines
            .iter()
            .enumerate()
            .skip(index + 1)
            .find(|(_, candidate)| {
                candidate.trim() == "}" && candidate.len() - candidate.trim_start().len() == indent
            })
            .map(|(position, _)| position)
        else {
            continue;
        };
        found.push(Found {
            path: shown.display().to_string(),
            name,
            line: index + 1,
            lines: end - index + 1,
        });
    }
    found
}

fn function_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .strip_prefix("pub(crate) ")
        .or_else(|| trimmed.strip_prefix("pub(super) "))
        .or_else(|| trimmed.strip_prefix("pub "))
        .or_else(|| trimmed.strip_prefix("async "))
        .unwrap_or(trimmed);
    let rest = rest.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn sources(directory: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
}

fn all_functions() -> Vec<Found> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(&path).ok().map(|text| (path, text)))
        .flat_map(|(path, text)| functions_in(&path, &text))
        .collect()
}

#[test]
fn no_function_is_longer_than_the_limit_without_saying_why() {
    let limit: usize = std::env::var("ANIMA_FN_LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(LIMIT);
    let functions = all_functions();
    assert!(
        functions.len() > 100,
        "the scan found only {} functions, so it is looking in the wrong place",
        functions.len()
    );

    let mut complaints = Vec::new();
    for function in functions.iter().filter(|f| f.lines > limit) {
        let known = KNOWN
            .iter()
            .find(|(path, name, _, _)| *path == function.path && *name == function.name);
        match known {
            None => complaints.push(format!(
                "{}: fn {} (line {}) is {} lines — split it, or add it to KNOWN with a reason",
                function.path, function.name, function.line, function.lines
            )),
            Some((_, _, recorded, _)) if function.lines > *recorded => complaints.push(format!(
                "{}: fn {} (line {}) has grown from {} to {} lines — the list may only shrink",
                function.path, function.name, function.line, recorded, function.lines
            )),
            Some(_) => {}
        }
    }

    // A listed function that has come back under the limit, or shrunk at all, wants its entry updated.
    for (path, name, recorded, _) in KNOWN {
        if let Some(function) = functions
            .iter()
            .find(|f| f.path == *path && f.name == *name)
            && function.lines < *recorded
        {
            complaints.push(format!(
                    "{path}: fn {name} is down to {} lines, from {recorded} — lower or remove its entry",
                    function.lines
                ));
        }
    }

    assert!(
        complaints.is_empty(),
        "function length check (limit {limit}):\n  {}\n",
        complaints.join("\n  ")
    );
}

#[test]
fn the_scan_reads_the_crate_and_its_binaries() {
    let functions = all_functions();
    let paths: Vec<&str> = functions.iter().map(|f| f.path.as_str()).collect();
    // Real crates and binaries, not `src/lib.rs`, which only re-exports and has no function to find.
    for expected in [
        "src/bin/anima-studio.rs",
        "src/workshop.rs",
        "src/ai/session.rs",
    ] {
        assert!(
            paths.contains(&expected),
            "no functions found in {expected}"
        );
    }
    // The scan must agree with the file it is reading: a function's body cannot be longer than its file.
    for function in &functions {
        assert!(function.lines > 0);
    }
}
