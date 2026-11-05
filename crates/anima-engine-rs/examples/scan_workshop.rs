//! Scan a Steam Workshop content directory and print what AnimaEngine would see.
//!
//! ```text
//! cargo run --example scan_workshop -- ~/.local/share/Steam/steamapps/workshop/content/3474900
//! ```

use std::path::{Path, PathBuf};

use anima_engine_rs::workshop::{self, LocalWorkshopSource, acf_file_name, installed_items};

fn main() {
    let Some(root) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: scan_workshop <content_root>");
        std::process::exit(2);
    };

    // The ACF lives two levels up from the content root: <library>/steamapps/workshop/.
    if let Some(acf) = acf_path(&root) {
        match std::fs::read_to_string(&acf).map_err(|e| e.to_string()) {
            Ok(text) => match installed_items(&text) {
                Ok(items) => println!("acf {}: {} item(s)", acf.display(), items.len()),
                Err(err) => println!("acf {}: parse error: {err}", acf.display()),
            },
            Err(err) => println!("acf {}: {err}", acf.display()),
        }
    }

    let source = LocalWorkshopSource::new(&root);
    match workshop::sync(&source) {
        Ok(snapshot) => {
            println!("subscribed/installed items: {}", snapshot.len());
            for (id, item) in &snapshot.items {
                println!("{id} -> {}", item.path.display());
                for asset in &item.assets {
                    println!("  - {}", asset.display());
                }
            }
        }
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}

fn acf_path(content_root: &Path) -> Option<PathBuf> {
    let workshop_dir = content_root.parent()?.parent()?;
    Some(workshop_dir.join(acf_file_name()))
}
