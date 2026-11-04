//! Inspect, verify and install the AnimaEngine translation catalog.
//!
//! ```text
//! anima-i18n candidates        # search order for anima_translations.json
//! anima-i18n status            # active language and catalog
//! anima-i18n verify [file]     # per-language health (exit 1 when incomplete)
//! anima-i18n install <file>    # validate and install it as the catalog
//! ```

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
        Some("verify") => verify(arguments.get(1).map(PathBuf::from)),
        Some("install") => install(arguments.get(1).map(PathBuf::from)),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: anima-i18n <candidates|status|verify [file]|install <file>>");
    ExitCode::FAILURE
}

fn verify(path: Option<PathBuf>) -> ExitCode {
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
            "{:<8} {:>8} {:>8} {:>6} {:>9}",
            language.language,
            language.entries,
            language.missing,
            language.empty,
            language.untranslated
        );
    }

    let incomplete: Vec<&str> = report
        .incomplete()
        .map(|language| language.language.as_str())
        .collect();
    if incomplete.is_empty() {
        println!("all languages complete");
        ExitCode::SUCCESS
    } else {
        eprintln!("incomplete: {}", incomplete.join(", "));
        ExitCode::FAILURE
    }
}

fn install(path: Option<PathBuf>) -> ExitCode {
    let Some(path) = path else {
        return usage();
    };
    match i18n::install_catalog(&path) {
        Ok(destination) => {
            println!("{}", destination.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("install failed: {err}");
            ExitCode::FAILURE
        }
    }
}
