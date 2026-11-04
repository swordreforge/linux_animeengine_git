//! Inspect, verify and install the AnimaEngine translation catalog.
//!
//! ```text
//! anima-i18n candidates                      # search order for anima_translations.json
//! anima-i18n status                          # active language and catalog
//! anima-i18n verify [file] [--all] [--missing]
//! anima-i18n install <file> [--primary|--languages=a,b]
//! anima-i18n merge <fragment.json> [catalog]   # apply translation additions
//! ```
//!
//! `verify` fails while a **primary** language (en, zh-CN, zh-TW, ja) is
//! incomplete; `--all` makes every language count instead. `--missing` prints the
//! source strings a primary language does not translate, which is the list a
//! translator needs. `install --primary` (or `--languages=…`) trims the catalog on
//! the way in, so a release can ship just the languages it supports.

use std::path::PathBuf;
use std::process::ExitCode;

use anima_engine_rs::i18n;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("candidates") => {
            for path in i18n::catalog_candidates() {
                println!("{}", path.display());
            }
            ExitCode::SUCCESS
        }
        Some("status") => {
            let status = i18n::I18n::discover().status();
            println!("language: {}", status.language);
            println!(
                "catalog:  {} (v{})",
                status.catalog_path.as_deref().unwrap_or("<not found>"),
                status.catalog_version
            );
            println!("entries:  {}", status.entries);
            ExitCode::SUCCESS
        }
        Some("verify") => verify(&arguments[1..]),
        Some("install") => install(&arguments[1..]),
        Some("merge") => merge(&arguments[1..]),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: anima-i18n <candidates|status|verify [file] [--all] [--missing]|\
         install <file> [--primary|--languages=a,b]|merge <fragment.json> [catalog]>"
    );
    ExitCode::FAILURE
}

fn verify(arguments: &[String]) -> ExitCode {
    let mut path = None;
    let mut every_language = false;
    let mut show_missing = false;
    for argument in arguments {
        match argument.as_str() {
            "--all" => every_language = true,
            "--missing" => show_missing = true,
            other if !other.starts_with('-') => path = Some(PathBuf::from(other)),
            _ => return usage(),
        }
    }

    let path = path.or_else(|| {
        i18n::catalog_candidates()
            .into_iter()
            .find(|candidate| candidate.is_file())
    });
    let Some(path) = path else {
        eprintln!("no catalog found; pass a path to verify");
        return ExitCode::FAILURE;
    };

    let catalog = i18n::load_catalog(&path);
    let Some(report) = i18n::verify_catalog(&catalog) else {
        eprintln!("{} is empty or unreadable", path.display());
        return ExitCode::FAILURE;
    };

    println!(
        "{}: {} source string(s), {} language(s)",
        path.display(),
        report.source_entries,
        report.languages.len()
    );
    println!(
        "{:<8} {:>8} {:>8} {:>6} {:>9}",
        "language", "entries", "missing", "empty", "untranslated"
    );
    for language in &report.languages {
        println!(
            "{:<8} {:>8} {:>8} {:>6} {:>9} {}",
            language.language,
            language.entries,
            language.missing,
            language.empty,
            language.untranslated,
            if language.primary { "*" } else { "" }
        );
    }
    println!("(* primary: {})", i18n::PRIMARY_LANGUAGES.join(", "));

    if show_missing {
        for language in &report.languages {
            if !language.primary || language.missing == 0 {
                continue;
            }
            let missing = i18n::missing_sources(&catalog, &language.language);
            println!(
                "\n{} does not translate {}:",
                language.language,
                missing.len()
            );
            for source in missing.iter().take(25) {
                println!("  {source}");
            }
            if missing.len() > 25 {
                println!("  ... ({} more)", missing.len() - 25);
            }
        }
    }

    let incomplete: Vec<&str> = if every_language {
        report
            .incomplete()
            .map(|language| language.language.as_str())
            .collect()
    } else {
        report
            .primary_incomplete()
            .map(|language| language.language.as_str())
            .collect()
    };
    if incomplete.is_empty() {
        println!("all primary languages complete");
        ExitCode::SUCCESS
    } else {
        eprintln!("incomplete: {}", incomplete.join(", "));
        ExitCode::FAILURE
    }
}

/// Merge a fragment (such as `packaging/i18n-additions.json`) into a catalog and
/// write it back. Language-neutral entries are not touched; additions win.
fn merge(arguments: &[String]) -> ExitCode {
    let mut paths = arguments
        .iter()
        .filter(|argument| !argument.starts_with('-'));
    let Some(fragment) = paths.next().map(PathBuf::from) else {
        return usage();
    };
    let base_path = paths.next().map(PathBuf::from).or_else(|| {
        i18n::catalog_candidates()
            .into_iter()
            .find(|candidate| candidate.is_file())
    });
    let Some(base_path) = base_path else {
        eprintln!("no catalog to merge into");
        return ExitCode::FAILURE;
    };

    let additions = i18n::load_catalog(&fragment);
    if additions.is_empty() {
        eprintln!("{} is not a usable fragment", fragment.display());
        return ExitCode::FAILURE;
    }
    let mut base = i18n::load_catalog(&base_path);
    if base.is_empty() {
        eprintln!("{} is empty or unreadable", base_path.display());
        return ExitCode::FAILURE;
    }

    let changed = i18n::merge_catalog(&mut base, &additions);
    match i18n::write_catalog(&base_path, &base) {
        Ok(()) => {
            println!(
                "{}: {changed} entr{} merged",
                base_path.display(),
                if changed == 1 { "y" } else { "ies" }
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("merge failed: {err}");
            ExitCode::FAILURE
        }
    }
}

fn install(arguments: &[String]) -> ExitCode {
    let mut path = None;
    let mut languages: Vec<String> = Vec::new();
    for argument in arguments {
        if let Some(list) = argument.strip_prefix("--languages=") {
            languages = list
                .split(',')
                .map(|code| code.trim().to_string())
                .filter(|code| !code.is_empty())
                .collect();
            continue;
        }
        match argument.as_str() {
            "--primary" => {
                languages = i18n::PRIMARY_LANGUAGES
                    .iter()
                    .map(|code| (*code).to_string())
                    .collect();
            }
            other if !other.starts_with('-') => path = Some(PathBuf::from(other)),
            _ => return usage(),
        }
    }
    let Some(path) = path else {
        return usage();
    };

    let catalog = i18n::load_catalog(&path);
    if catalog.is_empty() {
        eprintln!("{} is empty or unreadable", path.display());
        return ExitCode::FAILURE;
    }

    let (catalog, note) = if languages.is_empty() {
        (catalog, String::new())
    } else {
        let wanted: Vec<&str> = languages.iter().map(String::as_str).collect();
        let trimmed = i18n::filter_catalog(&catalog, &wanted);
        let note = format!(
            " ({} of {} languages: {})",
            trimmed.len(),
            catalog.len(),
            trimmed.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        (trimmed, note)
    };

    match i18n::install_catalog_data(&catalog) {
        Ok(destination) => {
            println!("{}{note}", destination.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("install failed: {err}");
            ExitCode::FAILURE
        }
    }
}
