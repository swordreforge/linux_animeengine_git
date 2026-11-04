//! `anima-workshop` — inspect the local Steam Workshop content for AnimaEngine.
//!
//! ```text
//! anima-workshop roots              # discovered Steam roots
//! anima-workshop libraries          # all Steam libraries
//! anima-workshop list               # installed items + asset counts
//! anima-workshop show <id>          # one item's directory and assets
//! anima-workshop download <id...>   # via SteamCMD (STEAMCMD, STEAMCMD_DIR)
//! anima-workshop sync               # subscribed items, downloading missing ones
//! anima-workshop web <id...>        # metadata (needs --features workshop-web)
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use anima_engine_rs::workshop::{self, LocalWorkshopSource};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("roots") => {
            for root in workshop::steam_roots() {
                println!("{}", root.display());
            }
            ExitCode::SUCCESS
        }
        Some("libraries") => {
            for library in workshop::discover_libraries() {
                println!("{}", library.display());
            }
            ExitCode::SUCCESS
        }
        Some("list") => list(),
        Some("show") => match args.get(1).and_then(|id| id.parse::<u64>().ok()) {
            Some(id) => show(id),
            None => usage(),
        },
        Some("download") => download(&args[1..]),
        Some("sync") => sync(),
        Some("web") => web(&args[1..]),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: anima-workshop <roots|libraries|list|show <id>|download <id...>|sync|web <id...>>"
    );
    ExitCode::FAILURE
}

/// Content roots for every discovered library that actually has workshop data.
fn content_roots() -> Vec<PathBuf> {
    workshop::discover_libraries()
        .iter()
        .map(|library| workshop::content_root(library))
        .filter(|root| root.is_dir())
        .collect()
}

fn list() -> ExitCode {
    let roots = content_roots();
    if roots.is_empty() {
        eprintln!("no workshop content found (see `anima-workshop roots`)");
        return ExitCode::FAILURE;
    }
    for root in roots {
        println!("# {}", root.display());
        let source = LocalWorkshopSource::new(root.clone());
        match workshop::sync(&source) {
            Ok(snapshot) => {
                for id in snapshot.ids() {
                    if let Some(item) = snapshot.get(id) {
                        println!(
                            "{}  {} asset(s)  {}",
                            item.id,
                            item.assets.len(),
                            item.path.display()
                        );
                    }
                }
            }
            Err(err) => eprintln!("scan failed: {err}"),
        }
    }
    ExitCode::SUCCESS
}

fn show(id: u64) -> ExitCode {
    for root in content_roots() {
        let path = root.join(id.to_string());
        if path.is_dir() {
            println!("{}", path.display());
            for asset in workshop::scan_item(&path) {
                println!("  {}", asset.display());
            }
            return ExitCode::SUCCESS;
        }
    }
    eprintln!("item {id} is not installed in any discovered library");
    ExitCode::FAILURE
}

fn download(ids: &[String]) -> ExitCode {
    let ids: Vec<u64> = ids.iter().filter_map(|id| id.parse().ok()).collect();
    if ids.is_empty() {
        return usage();
    }
    let Some(steamcmd) = anima_engine_rs::steamcmd::SteamCmd::detect() else {
        eprintln!(
            "steamcmd not found: install it or set STEAMCMD=/path/to/steamcmd \
             (STEAMCMD_DIR selects the download root)"
        );
        return ExitCode::FAILURE;
    };
    let mut ok = true;
    for id in ids {
        match steamcmd.download(id) {
            Ok(path) => println!("{}  {}", id, path.display()),
            Err(err) => {
                eprintln!("{id}: {err}");
                ok = false;
            }
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Sync the Steam client's subscriptions, downloading anything missing through
/// SteamCMD, and print the resulting snapshot.
fn sync() -> ExitCode {
    let Some(steamcmd) = anima_engine_rs::steamcmd::SteamCmd::detect() else {
        eprintln!("steamcmd not found: install it or set STEAMCMD=/path/to/steamcmd");
        return ExitCode::FAILURE;
    };
    match workshop::sync(&steamcmd) {
        Ok(snapshot) => {
            for id in snapshot.ids() {
                if let Some(item) = snapshot.get(id) {
                    println!(
                        "{}  {} asset(s)  {}",
                        item.id,
                        item.assets.len(),
                        item.path.display()
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("sync failed: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "workshop-web")]
fn web(ids: &[String]) -> ExitCode {
    let ids: Vec<u64> = ids.iter().filter_map(|id| id.parse().ok()).collect();
    if ids.is_empty() {
        return usage();
    }
    let client = match anima_engine_rs::workshop_web::WorkshopWeb::new() {
        Ok(client) => client,
        Err(err) => {
            eprintln!("web client: {err}");
            return ExitCode::FAILURE;
        }
    };
    match client.fetch_published_file_details(&ids) {
        Ok(details) => {
            for item in details {
                println!(
                    "{}  {}  {}  [{}]",
                    item.id().unwrap_or(0),
                    item.title,
                    item.file_size_bytes()
                        .map(human_size)
                        .unwrap_or_else(|| "?".to_string()),
                    item.tag_names().collect::<Vec<_>>().join(", ")
                );
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("web fetch failed: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "workshop-web"))]
fn web(_ids: &[String]) -> ExitCode {
    eprintln!("`web` needs the `workshop-web` feature");
    ExitCode::FAILURE
}

#[cfg(feature = "workshop-web")]
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1}{}", UNITS[unit])
}
