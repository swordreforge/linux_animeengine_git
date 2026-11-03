//! Steam Workshop integration — the whole flow, with the Steam SDK as the one
//! pluggable seam.
//!
//! The full pipeline is: `subscribed_ids()` -> `ensure_installed(id)` -> scan
//! the item directory -> a [`WorkshopSnapshot`], and [`diff`] against the
//! previous snapshot. The local half (finding the content directory, parsing
//! Steam's `.acf`/`.vdf` files, scanning assets) needs no Steam at all and is
//! implemented by [`LocalWorkshopSource`]. The Steamworks SDK implementation of
//! [`WorkshopSource`] (subscribe/download/callbacks) slots in later without
//! touching any of this.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::vdf;
use crate::{Error, Result};

/// AnimaEngine's Steam application id.
pub const APP_ID: u64 = 3474900;
/// `<library>/steamapps/workshop/content`.
pub const WORKSHOP_CONTENT_SUBDIR: &str = "steamapps/workshop/content";
/// `libraryfolders.vdf` name.
pub const LIBRARY_FOLDERS_FILE: &str = "libraryfolders.vdf";
/// Extensions treated as playable workshop assets.
pub const ASSET_EXTENSIONS: &[&str] = &["gif", "apng", "webp", "png", "mdaa"];

/// Steam's per-app workshop state file for [`APP_ID`].
pub fn acf_file_name() -> String {
    format!("appworkshop_{APP_ID}.acf")
}

/// Parse `libraryfolders.vdf` into the list of Steam library paths.
pub fn library_folders(vdf_text: &str) -> Result<Vec<PathBuf>> {
    let (_name, root) = vdf::parse(vdf_text)?;
    let block = root
        .entries()
        .iter()
        .filter_map(|(_, entry)| entry.get("path").and_then(vdf::Kv::as_str))
        .map(PathBuf::from)
        .collect();
    Ok(block)
}

/// `<library>/steamapps/workshop/content/<appid>`.
pub fn content_root(library: &Path) -> PathBuf {
    library
        .join(WORKSHOP_CONTENT_SUBDIR)
        .join(APP_ID.to_string())
}

/// Installed-state of one workshop item, from `appworkshop_<appid>.acf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemState {
    pub size: u64,
    pub timeupdated: u64,
    pub manifest: u64,
}

/// Parse the `WorkshopItemsInstalled` table out of an `.acf` document.
pub fn installed_items(acf_text: &str) -> Result<BTreeMap<u64, ItemState>> {
    let (_name, root) = vdf::parse(acf_text)?;
    let table = root
        .get("WorkshopItemsInstalled")
        .ok_or_else(|| Error::Parse("acf has no WorkshopItemsInstalled".to_string()))?;

    let mut items = BTreeMap::new();
    for (id, state) in table.entries() {
        let id: u64 = id
            .parse()
            .map_err(|_| Error::Parse(format!("bad workshop id: {id}")))?;
        items.insert(
            id,
            ItemState {
                size: state.get("size").and_then(vdf::Kv::as_u64).unwrap_or(0),
                timeupdated: state
                    .get("timeupdated")
                    .and_then(vdf::Kv::as_u64)
                    .unwrap_or(0),
                manifest: state.get("manifest").and_then(vdf::Kv::as_u64).unwrap_or(0),
            },
        );
    }
    Ok(items)
}

/// Published file ids present as directories under the content root.
pub fn installed_item_dirs(content_root: &Path) -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(content_root) else {
        return Vec::new();
    };
    let mut ids: Vec<u64> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect();
    ids.sort_unstable();
    ids
}

/// Asset files inside one workshop item directory.
pub fn scan_item(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut assets: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter(|entry| {
            entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ASSET_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
        .map(|entry| entry.path())
        .collect();
    assets.sort();
    assets
}

/// One installed workshop item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkshopItem {
    pub id: u64,
    pub path: PathBuf,
    pub assets: Vec<PathBuf>,
}

/// The set of installed items at a point in time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkshopSnapshot {
    pub items: BTreeMap<u64, WorkshopItem>,
}

impl WorkshopSnapshot {
    pub fn ids(&self) -> Vec<u64> {
        self.items.keys().copied().collect()
    }

    pub fn get(&self, id: u64) -> Option<&WorkshopItem> {
        self.items.get(&id)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// What changed between two snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkshopDiff {
    pub added: Vec<u64>,
    pub removed: Vec<u64>,
}

/// Added / removed item ids going from `previous` to `current`.
pub fn diff(previous: &WorkshopSnapshot, current: &WorkshopSnapshot) -> WorkshopDiff {
    WorkshopDiff {
        added: current
            .items
            .keys()
            .filter(|id| !previous.items.contains_key(id))
            .copied()
            .collect(),
        removed: previous
            .items
            .keys()
            .filter(|id| !current.items.contains_key(id))
            .copied()
            .collect(),
    }
}

/// Scan a content root into a snapshot (no Steam client required).
pub fn scan_workshop(content_root: &Path) -> WorkshopSnapshot {
    let items = installed_item_dirs(content_root)
        .into_iter()
        .map(|id| {
            let path = content_root.join(id.to_string());
            let assets = scan_item(&path);
            (id, WorkshopItem { id, path, assets })
        })
        .collect();
    WorkshopSnapshot { items }
}

/// The Steam-facing seam. [`LocalWorkshopSource`] implements it today;
/// a Steamworks SDK adapter will implement it later.
pub trait WorkshopSource {
    /// Published file ids the user is subscribed to.
    fn subscribed_ids(&self) -> Result<Vec<u64>>;

    /// Ensure the item is installed, returning its directory when available.
    fn ensure_installed(&self, id: u64) -> Result<Option<PathBuf>>;
}

/// Run the full flow against `source`.
pub fn sync(source: &dyn WorkshopSource) -> Result<WorkshopSnapshot> {
    let mut items = BTreeMap::new();
    for id in source.subscribed_ids()? {
        let Some(path) = source.ensure_installed(id)? else {
            continue;
        };
        items.insert(
            id,
            WorkshopItem {
                id,
                assets: scan_item(&path),
                path,
            },
        );
    }
    Ok(WorkshopSnapshot { items })
}

/// Reads Steam's already-downloaded content from disk. Needs no Steam client
/// and is also the fallback when the Steamworks SDK is unavailable.
pub struct LocalWorkshopSource {
    pub content_root: PathBuf,
}

impl LocalWorkshopSource {
    pub fn new(content_root: impl Into<PathBuf>) -> Self {
        Self {
            content_root: content_root.into(),
        }
    }

    /// `<content_root>/../appworkshop_<appid>.acf`.
    fn acf_path(&self) -> Option<PathBuf> {
        let workshop_dir = self.content_root.parent()?.parent()?;
        Some(workshop_dir.join(acf_file_name()))
    }
}

impl WorkshopSource for LocalWorkshopSource {
    fn subscribed_ids(&self) -> Result<Vec<u64>> {
        if let Some(acf) = self.acf_path()
            && let Ok(text) = std::fs::read_to_string(&acf)
            && let Ok(items) = installed_items(&text)
        {
            return Ok(items.keys().copied().collect());
        }
        Ok(installed_item_dirs(&self.content_root))
    }

    fn ensure_installed(&self, id: u64) -> Result<Option<PathBuf>> {
        let dir = self.content_root.join(id.to_string());
        Ok(dir.is_dir().then_some(dir))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY_FOLDERS: &str = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"D:\\steam\\steamapps"
		"label"		""
		"apps" { }
	}
	"1"
	{
		"path"		"/home/user/.steam/steam"
		"label"		""
	}
}
"#;

    const APPWORKSHOP: &str = r#"
"AppWorkshop"
{
	"appid"		"3474900"
	"SizeOnDisk"		"576166082"
	"WorkshopItemsInstalled"
	{
		"3425176851"
		{
			"size"		"237871746"
			"timeupdated"		"1739236561"
			"manifest"		"6840599560970356282"
		}
		"3618631640"
		{
			"size"		"1282175"
			"timeupdated"		"1764998743"
			"manifest"		"659556662846826589"
		}
	}
	"WorkshopItemDetails"
	{
		"3425176851" { "manifest" "6840599560970356282" }
	}
}
"#;

    #[test]
    fn parses_library_folders() {
        let paths = library_folders(LIBRARY_FOLDERS).unwrap();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("D:\\steam\\steamapps"),
                PathBuf::from("/home/user/.steam/steam"),
            ]
        );
    }

    #[test]
    fn parses_installed_items() {
        let items = installed_items(APPWORKSHOP).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(
            items.get(&3425176851),
            Some(&ItemState {
                size: 237_871_746,
                timeupdated: 1_739_236_561,
                manifest: 6_840_599_560_970_356_282,
            })
        );
        assert_eq!(items[&3618631640].size, 1_282_175);
    }

    #[test]
    fn content_root_layout() {
        let root = content_root(Path::new("/lib"));
        assert_eq!(
            root,
            PathBuf::from("/lib/steamapps/workshop/content/3474900")
        );
    }

    #[test]
    fn scans_items_and_assets() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("steamapps/workshop/content/3474900");
        std::fs::create_dir_all(content.join("111")).unwrap();
        std::fs::create_dir_all(content.join("222")).unwrap();
        std::fs::write(content.join("111/miku.apng"), b"x").unwrap();
        std::fs::write(content.join("111/readme.txt"), b"x").unwrap();
        std::fs::write(content.join("222/pack.mdaa"), b"x").unwrap();

        let snapshot = scan_workshop(&content);
        assert_eq!(snapshot.ids(), vec![111, 222]);
        assert_eq!(snapshot.get(111).unwrap().assets.len(), 1);
        assert!(snapshot.get(111).unwrap().assets[0].ends_with("miku.apng"));
        assert!(snapshot.get(222).unwrap().assets[0].ends_with("pack.mdaa"));
    }

    #[test]
    fn local_source_uses_acf_then_falls_back_to_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("steamapps/workshop/content/3474900");
        std::fs::create_dir_all(&content).unwrap();
        let source = LocalWorkshopSource::new(&content);

        // No acf yet, no dirs -> empty.
        assert!(source.subscribed_ids().unwrap().is_empty());

        std::fs::create_dir_all(content.join("999")).unwrap();
        assert_eq!(source.subscribed_ids().unwrap(), vec![999]);
        assert_eq!(
            source.ensure_installed(999).unwrap(),
            Some(content.join("999"))
        );
        assert_eq!(source.ensure_installed(1234).unwrap(), None);

        // With an acf present it wins (Steam's record of subscribed items).
        std::fs::write(
            content
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(acf_file_name()),
            APPWORKSHOP,
        )
        .unwrap();
        assert_eq!(
            source.subscribed_ids().unwrap(),
            vec![3425176851, 3618631640]
        );
    }

    #[test]
    fn sync_and_diff_track_changes() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("steamapps/workshop/content/3474900");
        std::fs::create_dir_all(content.join("111")).unwrap();
        std::fs::write(content.join("111/a.gif"), b"x").unwrap();

        let source = LocalWorkshopSource::new(&content);
        let first = sync(&source).unwrap();
        assert_eq!(first.ids(), vec![111]);

        std::fs::create_dir_all(content.join("222")).unwrap();
        std::fs::write(content.join("222/b.webp"), b"x").unwrap();
        let second = sync(&source).unwrap();

        let changes = diff(&first, &second);
        assert_eq!(changes.added, vec![222]);
        assert!(changes.removed.is_empty());

        std::fs::remove_dir_all(content.join("111")).unwrap();
        let third = sync(&source).unwrap();
        let changes = diff(&second, &third);
        assert!(changes.added.is_empty());
        assert_eq!(changes.removed, vec![111]);
    }
}
