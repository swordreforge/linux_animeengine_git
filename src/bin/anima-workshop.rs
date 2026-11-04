//! `anima-workshop` — inspect the local Steam Workshop content for AnimaEngine.
//!
//! ```text
//! anima-workshop roots              # discovered Steam roots
//! anima-workshop libraries          # all Steam libraries
//! anima-workshop list               # installed items + asset counts
//! anima-workshop show <id>          # one item's directory and assets
//! anima-workshop download <id...>   # via SteamCMD (STEAMCMD, STEAMCMD_DIR)
//! anima-workshop play <id> [asset]  # install if needed, run in anima-overlay
//! anima-workshop upload <dir> [--title T] [--id N] [--user U] [--dry-run]
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
        Some("play") => play(&args[1..]),
        Some("upload") => upload(&args[1..]),
        Some("sync") => sync(),
        Some("web") => web(&args[1..]),
        _ => usage(),
    }
}

/// Consume the value that follows a flag.
fn take(args: &[String], index: &mut usize) -> String {
    let value = args.get(*index).cloned().unwrap_or_default();
    *index += 1;
    value
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: anima-workshop <roots|libraries|list|show <id>|download <id...>|play <id> [asset]|upload <dir> [opts]|sync|web <id...>>"
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

/// Resolve an item, downloading it first if needed, then run it in the overlay.
fn play(args: &[String]) -> ExitCode {
    use anima_engine_rs::workshop::WorkshopSource;

    let Some(id) = args.first().and_then(|value| value.parse::<u64>().ok()) else {
        return usage();
    };
    let selector = args.get(1);

    let dir = content_roots()
        .into_iter()
        .map(|root| root.join(id.to_string()))
        .find(|path| path.is_dir())
        .or_else(|| {
            let steamcmd = anima_engine_rs::steamcmd::SteamCmd::detect()?;
            match steamcmd.ensure_installed(id) {
                Ok(path) => path,
                Err(err) => {
                    eprintln!("download of {id} failed: {err}");
                    None
                }
            }
        });
    let Some(dir) = dir else {
        eprintln!("item {id} is not installed and could not be downloaded");
        return ExitCode::FAILURE;
    };

    let assets = workshop::scan_item(&dir);
    let asset = match selector {
        Some(needle) => assets
            .iter()
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(needle.as_str()))
            })
            .cloned(),
        None => assets.first().cloned(),
    };
    let Some(asset) = asset else {
        match selector {
            Some(needle) => eprintln!("item {id} has no asset matching {needle:?}"),
            None => eprintln!("item {id} has no playable asset"),
        }
        return ExitCode::FAILURE;
    };

    let overlay = overlay_binary();
    eprintln!("[workshop] {} → {}", asset.display(), overlay.display());
    match std::process::Command::new(&overlay).arg(&asset).status() {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1) as u8),
        Err(err) => {
            eprintln!("could not launch {}: {err}", overlay.display());
            ExitCode::FAILURE
        }
    }
}

/// Build a `workshop.vdf` for a content folder and upload it with SteamCMD.
fn upload(args: &[String]) -> ExitCode {
    use anima_engine_rs::steamcmd::{SteamCmd, Visibility, WorkshopUpload};

    let mut content_dir: Option<PathBuf> = None;
    let mut title: Option<String> = None;
    let mut description = String::new();
    let mut preview: Option<PathBuf> = None;
    let mut published_file_id: Option<u64> = None;
    let mut visibility = Visibility::Public;
    let mut changenote = String::new();
    let mut user = std::env::var("STEAM_USER").ok();
    let mut dry_run = false;

    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].clone();
        index += 1;
        match arg.as_str() {
            "--title" => title = Some(take(args, &mut index)),
            "--description" | "--desc" => description = take(args, &mut index),
            "--preview" => preview = Some(PathBuf::from(take(args, &mut index))),
            "--id" => published_file_id = take(args, &mut index).parse().ok(),
            "--changenote" => changenote = take(args, &mut index),
            "--user" => user = Some(take(args, &mut index)),
            "--visibility" => match Visibility::parse(&take(args, &mut index)) {
                Some(parsed) => visibility = parsed,
                None => {
                    eprintln!("visibility must be public, friends, private or unlisted");
                    return usage();
                }
            },
            "--dry-run" => dry_run = true,
            _ if arg.starts_with('-') => return usage(),
            other => content_dir = Some(PathBuf::from(other)),
        }
    }

    let Some(content_dir) = content_dir else {
        eprintln!(
            "usage: anima-workshop upload <dir> [--title T] [--description D] [--preview PNG] \
             [--id <publishedfileid>] [--visibility public|friends|private|unlisted] \
             [--changenote N] [--user U] [--dry-run]"
        );
        return ExitCode::FAILURE;
    };
    if !content_dir.is_dir() {
        eprintln!("{} is not a directory", content_dir.display());
        return ExitCode::FAILURE;
    }
    let title = title.unwrap_or_else(|| {
        content_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "AnimaEngine item".to_string())
    });

    let item = WorkshopUpload {
        title,
        description,
        content_dir,
        preview,
        published_file_id,
        visibility,
        changenote,
    };
    if dry_run {
        print!("{}", item.to_vdf());
        return ExitCode::SUCCESS;
    }

    let Some(user) = user else {
        eprintln!("upload needs an account: pass --user <steam login> or set STEAM_USER");
        return ExitCode::FAILURE;
    };
    let Some(steamcmd) = SteamCmd::detect() else {
        eprintln!("steamcmd not found: install it or set STEAMCMD=/path/to/steamcmd");
        return ExitCode::FAILURE;
    };
    match steamcmd.upload(&item, &user) {
        Ok(Some(id)) => {
            println!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}");
            ExitCode::SUCCESS
        }
        Ok(None) => {
            eprintln!("steamcmd finished but did not report a published file id");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("upload failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Locate the overlay binary: `ANIMA_OVERLAY`, then a sibling, then `PATH`.
fn overlay_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("ANIMA_OVERLAY") {
        return PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("anima-overlay");
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("anima-overlay")
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
