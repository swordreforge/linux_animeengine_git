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

/// `<library>/steamapps/workshop/appworkshop_<appid>.acf`.
pub fn acf_path(library: &Path) -> PathBuf {
    library
        .join("steamapps")
        .join("workshop")
        .join(acf_file_name())
}

/// `<library>/steamapps/workshop/content/<appid>`.
pub fn content_root(library: &Path) -> PathBuf {
    library
        .join(WORKSHOP_CONTENT_SUBDIR)
        .join(APP_ID.to_string())
}

/// Candidate Steam roots on Linux: `STEAM_ROOT` first, then the common install
/// locations. Only existing directories are returned, deduplicated canonically.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(root) = std::env::var_os("STEAM_ROOT") {
        candidates.push(PathBuf::from(root));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.extend([
            home.join(".steam").join("steam"),
            home.join(".steam").join("root"),
            home.join(".steam").join("debian-installation"),
            home.join(".local").join("share").join("Steam"),
            home.join(".var/app/com.valvesoftware.Steam/data/Steam"),
        ]);
    }
    dedupe_dirs(candidates)
}

/// Libraries described by each root's `steamapps/libraryfolders.vdf`, plus the
/// roots themselves that exist on disk.
pub fn libraries_from_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut libraries: Vec<PathBuf> = Vec::new();
    for root in roots {
        push_dir(&mut libraries, root.clone());
        let vdf_path = root.join("steamapps").join(LIBRARY_FOLDERS_FILE);
        if let Ok(text) = std::fs::read_to_string(&vdf_path)
            && let Ok(paths) = library_folders(&text)
        {
            for path in paths {
                push_dir(&mut libraries, path);
            }
        }
    }
    libraries
}

/// All Steam libraries discoverable on this machine.
pub fn discover_libraries() -> Vec<PathBuf> {
    libraries_from_roots(&steam_roots())
}

fn push_dir(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !path.is_dir() {
        return;
    }
    let canonical = std::fs::canonicalize(&path).unwrap_or(path);
    if !paths.contains(&canonical) {
        paths.push(canonical);
    }
}

fn dedupe_dirs(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        push_dir(&mut out, path);
    }
    out
}

/// Installed-state of one workshop item, from `appworkshop_<appid>.acf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemState {
    pub size: u64,
    pub timeupdated: u64,
    pub manifest: u64,
    /// The manifest the client *wants*. Steam writes it into `WorkshopItemDetails` as soon as it decides
    /// an item needs downloading or updating, and only then moves `manifest` up to meet it — so the two
    /// disagreeing is Steam's own way of saying "this one is owed a download", whether that download is
    /// running now or queued behind others.
    pub latest_manifest: u64,
}

impl ItemState {
    /// Whether Steam still owes this item a download. `0` means the detail table had nothing to say,
    /// which is not the same as "up to date" — it is "no opinion", and no opinion must not grey a button.
    pub fn pending(&self) -> bool {
        self.latest_manifest != 0 && self.latest_manifest != self.manifest
    }
}

/// Parse the `WorkshopItemsInstalled` table out of an `.acf` document.
pub fn installed_items(acf_text: &str) -> Result<BTreeMap<u64, ItemState>> {
    let (_name, root) = vdf::parse(acf_text)?;
    let table = root
        .get("WorkshopItemsInstalled")
        .ok_or_else(|| Error::Parse("acf has no WorkshopItemsInstalled".to_string()))?;

    // `WorkshopItemDetails` is the other half of the picture and used to be ignored here: it carries the
    // manifest Steam wants, which is how a download in progress is visible at all.
    let details = root.get("WorkshopItemDetails");
    let latest = |id: &str| -> u64 {
        details
            .and_then(|details| details.get(id))
            .and_then(|state| {
                state
                    .get("latest_manifest")
                    .or_else(|| state.get("manifest"))
            })
            .and_then(vdf::Kv::as_u64)
            .unwrap_or(0)
    };

    let mut items = BTreeMap::new();
    for (key, state) in table.entries() {
        let id: u64 = key
            .parse()
            .map_err(|_| Error::Parse(format!("bad workshop id: {key}")))?;
        items.insert(
            id,
            ItemState {
                size: state.get("size").and_then(vdf::Kv::as_u64).unwrap_or(0),
                timeupdated: state
                    .get("timeupdated")
                    .and_then(vdf::Kv::as_u64)
                    .unwrap_or(0),
                manifest: state.get("manifest").and_then(vdf::Kv::as_u64).unwrap_or(0),
                latest_manifest: latest(key),
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
    /// Same id, different content on disk: a Workshop item was updated.
    pub updated: Vec<u64>,
    pub removed: Vec<u64>,
}

impl WorkshopDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.updated.is_empty() && self.removed.is_empty()
    }
}

/// Added / updated / removed item ids going from `previous` to `current`.
///
/// `updated` is what a Workshop re-publish looks like: the id stays, the files
/// change — without it a hot reload would never fire for an updated item.
pub fn diff(previous: &WorkshopSnapshot, current: &WorkshopSnapshot) -> WorkshopDiff {
    WorkshopDiff {
        added: current
            .items
            .keys()
            .filter(|id| !previous.items.contains_key(id))
            .copied()
            .collect(),
        updated: current
            .items
            .iter()
            .filter(|(id, item)| previous.items.get(*id).is_some_and(|old| old != *item))
            .map(|(id, _)| *id)
            .collect(),
        removed: previous
            .items
            .keys()
            .filter(|id| !current.items.contains_key(id))
            .copied()
            .collect(),
    }
}

/// Polls the local workshop content and reports what changed since the last poll.
///
/// This is the fallback "hot reload" signal: it needs no Steamworks SDK (and no
/// `libsteam_api.so`) — the same role the Python build gives its SteamCMD fallback.
/// When the SDK *is* loaded, `steam::FlatSteamApi::run_callbacks` can drive the same
/// diff instead of polling. A poll costs one `read_dir` per item directory, so it is
/// cheap for the handful of items a user has installed.
pub struct WorkshopWatcher {
    roots: Vec<PathBuf>,
    snapshot: WorkshopSnapshot,
}

impl WorkshopWatcher {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        // Keep paths that do not exist yet: a SteamCMD download root only appears
        // after the first download, and the watcher must notice that.
        let snapshot = Self::scan(&roots);
        Self { roots, snapshot }
    }

    /// Watch every discovered library plus our own SteamCMD download root.
    pub fn discover() -> Self {
        let mut roots: Vec<PathBuf> = discover_libraries()
            .iter()
            .map(|library| content_root(library))
            .collect();

        let install_dir = std::env::var_os(crate::steamcmd::STEAMCMD_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::paths::user_data_dir().join("steamcmd"));
        roots.push(
            install_dir
                .join(WORKSHOP_CONTENT_SUBDIR)
                .join(APP_ID.to_string()),
        );
        Self::new(roots)
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    pub fn snapshot(&self) -> &WorkshopSnapshot {
        &self.snapshot
    }

    /// Re-scan and return the change, or `None` when nothing moved.
    pub fn poll(&mut self) -> Option<WorkshopDiff> {
        let current = Self::scan(&self.roots);
        let change = diff(&self.snapshot, &current);
        self.snapshot = current;
        if change.is_empty() {
            None
        } else {
            Some(change)
        }
    }

    /// Merge every root; the first root that has an id wins (Steam's copy first).
    fn scan(roots: &[PathBuf]) -> WorkshopSnapshot {
        let mut items = BTreeMap::new();
        for root in roots {
            for (id, item) in scan_workshop(root).items {
                items.entry(id).or_insert(item);
            }
        }
        WorkshopSnapshot { items }
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
		"3700000001"
		{
			"size"		"4096"
			"timeupdated"		"1764998800"
			"manifest"		"111"
		}
	}
	"WorkshopItemDetails"
	{
		"3425176851" { "manifest" "6840599560970356282" }
		"3700000001" { "latest_manifest" "222" }
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
        assert_eq!(items.len(), 3);
        assert_eq!(
            items.get(&3425176851),
            Some(&ItemState {
                size: 237_871_746,
                timeupdated: 1_739_236_561,
                manifest: 6_840_599_560_970_356_282,
                latest_manifest: 6_840_599_560_970_356_282,
            })
        );
        assert_eq!(items[&3618631640].size, 1_282_175);
        // The detail table agreeing with the installed one is "up to date", and saying nothing at all is
        // "no opinion" — neither is a download Steam owes.
        assert!(!items[&3425176851].pending());
        assert!(!items[&3618631640].pending());
        // `latest_manifest` past `manifest` is Steam having decided to fetch something and not yet done
        // it, which is the only way a download in flight is visible from outside the client.
        assert_eq!(items[&3700000001].latest_manifest, 222);
        assert!(items[&3700000001].pending(), "manifest 111 vs latest 222");
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
    fn discovers_libraries_from_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("steam");
        let lib1 = dir.path().join("lib1");
        let lib2 = dir.path().join("lib2");
        for path in [&root, &lib1, &lib2] {
            std::fs::create_dir_all(path.join("steamapps")).unwrap();
        }
        let vdf = format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\"{}\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\"{}\"\n\t}}\n}}\n",
            lib1.display(),
            lib2.display()
        );
        std::fs::write(root.join("steamapps").join(LIBRARY_FOLDERS_FILE), vdf).unwrap();

        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap();
        let libraries = libraries_from_roots(std::slice::from_ref(&root));
        assert_eq!(libraries.len(), 3);
        assert!(libraries.contains(&canonical(&root)));
        assert!(libraries.contains(&canonical(&lib1)));
        assert!(libraries.contains(&canonical(&lib2)));

        // Missing roots and duplicate entries are ignored.
        assert!(libraries_from_roots(&[dir.path().join("absent")]).is_empty());
        let roots = [root.clone(), root];
        let again = libraries_from_roots(&roots);
        assert_eq!(again.len(), 3);
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
            vec![3425176851, 3618631640, 3700000001]
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

    #[test]
    fn diff_reports_updated_items() {
        let root = PathBuf::from("/w");
        let previous = WorkshopSnapshot {
            items: BTreeMap::from([(
                7,
                WorkshopItem {
                    id: 7,
                    path: root.join("7"),
                    assets: vec![root.join("7/a.apng")],
                },
            )]),
        };
        let current = WorkshopSnapshot {
            items: BTreeMap::from([(
                7,
                WorkshopItem {
                    id: 7,
                    path: root.join("7"),
                    assets: vec![root.join("7/a.apng"), root.join("7/b.apng")],
                },
            )]),
        };

        let change = diff(&previous, &current);
        assert_eq!(change.updated, vec![7]);
        assert!(change.added.is_empty() && change.removed.is_empty());
        assert!(!change.is_empty());
        assert!(diff(&current, &current).is_empty());
    }

    #[test]
    fn watcher_reports_added_updated_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("content");

        let mut watcher = WorkshopWatcher::new(vec![root.clone()]);
        assert_eq!(watcher.poll(), None, "nothing installed yet");

        let item = root.join("42");
        std::fs::create_dir_all(&item).unwrap();
        std::fs::write(item.join("a.apng"), b"one").unwrap();
        let change = watcher.poll().expect("added");
        assert_eq!(change.added, vec![42]);
        assert!(change.updated.is_empty());

        std::fs::write(item.join("b.apng"), b"two").unwrap();
        let change = watcher.poll().expect("updated");
        assert_eq!(change.updated, vec![42]);
        assert!(change.added.is_empty());

        std::fs::remove_dir_all(&item).unwrap();
        let change = watcher.poll().expect("removed");
        assert_eq!(change.removed, vec![42]);
        assert_eq!(watcher.poll(), None);
    }
}
