//! `anima-studio` — the main window (Iced).
//!
//! Second slice: Workshop previews and the actions around them.
//!
//! * **Preview**: a `preview.*` file inside the item directory if the author shipped
//!   one, otherwise the Steam CDN image from `preview_url`, cached under
//!   `~/.local/share/AnimaEngine/thumbnails/<id>.img` and decoded with the same
//!   `image` crate the rest of the tree uses.
//! * **Subscribe / Unsubscribe**: both are Steam client actions, so the buttons open
//!   `steam://url/CommunityFilePage/<id>` where the real toggle lives. Claiming
//!   otherwise would be a lie.
//! * **Download**: SteamCMD (`anonymous`), on a detached thread — that is what makes
//!   an item appear locally without the client.
//! * **Remove local copy**: only for items under *our* SteamCMD root, behind a
//!   confirmation click. It never touches the user's own Steam library.
//!
//! Requires the `studio` feature: `cargo run --features studio --bin anima-studio`.
//!
//! Note: this pulls a second `wgpu` (iced 0.14 is on 27.x, the overlay is on 29.x).
//! They never meet — the overlay is a separate process and keeps owning its own GPU
//! stack — so the cost is build time and binary size, not correctness.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anima_engine_rs::control::{ControlState, InputMode, instance_slug};
use anima_engine_rs::i18n::{I18n, PRIMARY_LANGUAGES, STEAM_LANGUAGES, normalize_language};
use anima_engine_rs::paths::{ensure_dir, user_data_dir};
use anima_engine_rs::state::SecondaryState;
use anima_engine_rs::steamcmd::SteamCmd;
use anima_engine_rs::workshop::{WorkshopItem, WorkshopSnapshot, WorkshopWatcher};
use anima_engine_rs::workshop_web::WorkshopWeb;
use iced::widget::button::{danger, primary, secondary, text as text_button};
use iced::widget::{button, column, container, image, pick_list, row, scrollable, space, text};
use iced::{Element, Length, Subscription, Task};
use serde::{Deserialize, Serialize};

const APP_TITLE: &str = "AnimaEngine";
/// Wayland app id / X11 class. It matches `packaging/anima-engine-studio.desktop`, which
/// is what lets the compositor associate the window with its entry (icon, name, grouping).
/// Without it the app id is empty and the window shows up unattached.
const APP_ID: &str = "anima-engine-studio";
/// Fixed height of the preview slot, so items with different aspect ratios do not make
/// everything below them jump.
const PREVIEW_HEIGHT: f32 = 200.0;
/// How often the overlay panel checks whether the control files moved.
const OVERLAY_POLL_MS: u64 = 1000;
/// `preview.*` names an author may have shipped inside the content folder.
const LOCAL_PREVIEWS: &[&str] = &[
    "preview.png",
    "preview.jpg",
    "preview.jpeg",
    "preview.webp",
    "preview.gif",
];

/// The native name of a catalog language (`zh-CN` → `简体中文`), via the Steam codes
/// the catalog is keyed by (`schinese`, `tchinese`, …).
fn native_name(code: &str) -> String {
    STEAM_LANGUAGES
        .iter()
        .find(|(steam, _)| normalize_language(steam) == code)
        .map(|(_, native)| (*native).to_string())
        .unwrap_or_else(|| code.to_string())
}

/// `$HOME` as `~`, with the middle elided when the path is long: workshop paths are
/// unbreakable tokens that overflow a narrow pane.
fn shorten(path: &Path) -> String {
    let text = path.display().to_string();
    let text = match std::env::var_os("HOME") {
        Some(home) => {
            let home = home.to_string_lossy();
            match text.strip_prefix(home.as_ref()) {
                Some(rest) => format!("~{rest}"),
                None => text,
            }
        }
        None => text,
    };
    const LIMIT: usize = 72;
    let count = text.chars().count();
    if count <= LIMIT {
        return text;
    }
    let keep = (LIMIT - 1) / 2;
    let head: String = text.chars().take(keep).collect();
    let tail: String = text.chars().skip(count - keep).collect();
    format!("{head}…{tail}")
}

fn file_name(asset: &Path) -> String {
    asset
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| asset.display().to_string())
}

/// 1px separator. 0.14 dropped the `vertical_rule`/`horizontal_rule` helpers, so a line
/// is a zero-content container that only carries a background.
fn divider(width: Length, height: Length) -> iced::widget::Container<'static, Message> {
    container(space())
        .width(width)
        .height(height)
        .style(|_theme| container::Style {
            background: Some(iced::Background::Color(iced::Color::from_rgba(
                1.0, 1.0, 1.0, 0.10,
            ))),
            ..Default::default()
        })
}

/// Newtype so `pick_list` shows the native name while carrying the catalog code.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Language(String);

impl std::fmt::Display for Language {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&native_name(&self.0))
    }
}

/// Steam metadata for one item (keyless `GetPublishedFileDetails`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Meta {
    #[serde(default)]
    title: String,
    #[serde(default)]
    subscriptions: u64,
    #[serde(default)]
    views: u64,
    #[serde(default)]
    preview_url: String,
    /// True when these values come from the cache because the API could not refresh them
    /// (offline). Never stored: it describes this run, not the data.
    #[serde(skip)]
    stale: bool,
}

/// What `thumbnails/<id>.json` holds: the answer and when Steam gave it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedMeta {
    /// Unix seconds of the answer.
    fetched: u64,
    details: Meta,
}

/// How long a cached answer is trusted before Steam is asked again.
const META_TTL_SECS: u64 = 24 * 60 * 60;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// A clock that moved backwards counts as fresh rather than throwing the cache away.
fn cache_is_fresh(fetched: u64, now: u64) -> bool {
    now.saturating_sub(fetched) < META_TTL_SECS
}

fn meta_cache_path(id: u64) -> PathBuf {
    thumb_dir().join(format!("{id}.json"))
}

/// A decoded preview. The handle is built **once**, when the item loads: every
/// `Handle::from_rgba` gets a fresh unique id, so rebuilding it in `view()` made the
/// renderer treat the image as new and re-upload it on every redraw — which the 1s
/// overlay poll turned into a visible flash.
#[derive(Debug, Clone)]
enum Thumb {
    Loading,
    Ready(image::Handle),
    Failed(String),
}

/// Everything the worker thread produces for one item.
struct Loaded {
    id: u64,
    meta: Option<Meta>,
    thumb: Result<(u32, u32, Vec<u8>), String>,
}

/// One overlay control file. The file name is only a 16-hex slug of the animation path,
/// so the label comes from the persisted instance list (`secondary_instances.json`).
#[derive(Debug, Clone)]
struct OverlayInstance {
    label: String,
    path: PathBuf,
    state: ControlState,
    /// Whether a process actually owns this control file right now
    /// (`ControlState::is_running` — the file outlives the process).
    running: bool,
}

/// What the overlay panel can ask for. Every one of these bumps the generation, exactly
/// like `anima-overlay-ctl` does — the window writes the same file, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayAction {
    ToggleHidden,
    ToggleLocked,
    ToggleClickThrough,
    ScaleUp,
    ScaleDown,
    Rotate,
    FlipH,
    FlipV,
    Reset,
    Quit,
}

/// Newtype so `pick_list` can show instance labels.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstanceLabel(String);

impl std::fmt::Display for InstanceLabel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// `● running` / `○ stopped` in front of the label: the only thing that tells a live
/// instance from one whose file is left over.
fn instance_label(instance: &OverlayInstance) -> String {
    format!(
        "{} {}",
        if instance.running { "●" } else { "○" },
        instance.label
    )
}

/// A cheap fingerprint of everything the panel shows: each control file and the persisted
/// instance list, as `path:mtime:size`. Comparing it is much cheaper than rebuilding the
/// instance list every tick, and it also catches an overlay changing its own state — a
/// drag or a scale writes that instance's control file.
fn overlay_signature() -> String {
    use std::fmt::Write as _;

    fn stamp(path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        Some(format!("{}:{mtime}:{}", path.display(), meta.len()))
    }

    let mut parts: Vec<String> = ControlState::instances()
        .iter()
        .filter_map(|path| stamp(path))
        .collect();
    // Liveness markers too: an instance starting or stopping must refresh the panel even
    // when nobody writes its control file.
    parts.extend(
        ControlState::announcements()
            .iter()
            .filter_map(|path| stamp(path)),
    );
    // …and focus markers, so touching an overlay switches the panel to it.
    parts.extend(
        ControlState::focus_files()
            .iter()
            .filter_map(|path| stamp(path)),
    );
    parts.extend(stamp(&SecondaryState::user_path()));
    // The shepherd's own marker, so its indicator in the panel follows it too.
    parts.extend(stamp(&anima_engine_rs::shepherd::marker_path()));
    parts.sort();
    let mut out = String::new();
    for part in parts {
        let _ = write!(out, "{part};");
    }
    out
}

/// `anima-overlay` next to this binary first (a checkout's `target/release` or an
/// install prefix), then on `PATH`.
fn find_overlay_binary() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("anima-overlay");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    anima_engine_rs::steamcmd::find_in_path("anima-overlay")
}

/// Extensions the overlay can actually play.
fn is_playable(asset: &Path) -> bool {
    asset.extension().is_some_and(|extension| {
        ["apng", "gif", "webp", "png"]
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known))
    })
}

/// Every control file on disk, labelled with the animation it belongs to when the
/// persisted instance list knows that key.
fn overlay_instances() -> Vec<OverlayInstance> {
    let persisted = SecondaryState::load(&SecondaryState::user_path());
    ControlState::instances()
        .into_iter()
        .map(|path| {
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default();
            let label = persisted
                .instances
                .iter()
                .find(|instance| instance_slug(&instance.path) == stem)
                .map(|instance| shorten(Path::new(&instance.path)))
                .unwrap_or_else(|| format!("{stem} (unknown animation)"));
            OverlayInstance {
                label,
                state: ControlState::load(&path),
                running: ControlState::is_running(&path),
                path,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
enum Message {
    Language(String),
    Refresh,
    Select(usize),
    Loaded(u64, Option<Meta>, Result<(u32, u32, Vec<u8>), String>),
    CopyPath(String),
    Download(u64),
    OpenPage(u64),
    ArmRemove(u64),
    Remove(u64),
    /// Put one workshop asset on the overlay: show the instance if it already exists,
    /// otherwise start one.
    PlayAsset(PathBuf),
    RefreshOverlay,
    SelectOverlay(String),
    Overlay(OverlayAction),
    PollOverlay,
    Quit,
}

fn thumb_dir() -> PathBuf {
    user_data_dir().join("thumbnails")
}

/// Ask Steam (keyless `GetPublishedFileDetails`).
fn fetch_meta(id: u64) -> Option<Meta> {
    let web = WorkshopWeb::new().ok()?;
    let details = web.fetch_published_file_details(&[id]).ok()?;
    let first = details.into_iter().next()?;
    Some(Meta {
        title: first.title,
        subscriptions: first.subscriptions,
        views: first.views,
        preview_url: first.preview_url,
        stale: false,
    })
}

/// Metadata for `id`: the cache while it is fresh, otherwise Steam — and when Steam is not
/// reachable the stale cache is still better than showing nothing (it is marked `stale` so
/// the pane can say so).
fn meta_for(id: u64) -> Option<Meta> {
    let path = meta_cache_path(id);
    let cached: Option<CachedMeta> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let now = now_secs();

    if let Some(cached) = &cached
        && cache_is_fresh(cached.fetched, now)
    {
        eprintln!(
            "[studio] metadata {id}: cache ({}s old)",
            now.saturating_sub(cached.fetched)
        );
        return Some(cached.details.clone());
    }

    match fetch_meta(id) {
        Some(meta) => {
            let entry = CachedMeta {
                fetched: now,
                details: meta.clone(),
            };
            if let Err(error) = ensure_dir(&thumb_dir()) {
                eprintln!("[studio] could not create the metadata cache: {error}");
            } else if let Err(error) = anima_engine_rs::paths::atomic_write_json(&path, &entry) {
                eprintln!("[studio] could not cache the metadata: {error}");
            }
            eprintln!("[studio] metadata {id}: fetched");
            Some(meta)
        }
        None => cached.map(|cached| {
            eprintln!("[studio] metadata {id}: stale cache, Steam unreachable");
            Meta {
                stale: true,
                ..cached.details
            }
        }),
    }
}

/// `preview.*` in the item directory, else the cached CDN image, else fetch it.
fn preview_bytes(id: u64, dir: &Path, url: Option<&str>) -> Result<Vec<u8>, String> {
    for name in LOCAL_PREVIEWS {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return std::fs::read(&candidate).map_err(|error| error.to_string());
        }
    }
    let cached = thumb_dir().join(format!("{id}.img"));
    if cached.is_file() {
        return std::fs::read(&cached).map_err(|error| error.to_string());
    }
    let Some(url) = url.filter(|url| !url.trim().is_empty()) else {
        return Err("no preview URL in the Steam metadata".to_string());
    };
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("anima-engine-rs/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|error| error.to_string())?;
    let bytes = client
        .get(url)
        .send()
        .map_err(|error| error.to_string())?
        .bytes()
        .map_err(|error| error.to_string())?
        .to_vec();
    if let Err(error) = ensure_dir(&thumb_dir()) {
        eprintln!("[studio] could not create the thumbnail cache: {error}");
    } else if let Err(error) = std::fs::write(&cached, &bytes) {
        eprintln!("[studio] could not cache the thumbnail: {error}");
    }
    Ok(bytes)
}

/// Blocking work for one item: metadata, then a preview decoded to RGBA.
fn load_item(id: u64, dir: PathBuf) -> Loaded {
    let meta = meta_for(id);
    let url = meta.as_ref().map(|meta| meta.preview_url.clone());
    let thumb = preview_bytes(id, &dir, url.as_deref()).and_then(|bytes| {
        // `::image` — the crate, not the `iced::widget::image` function imported above.
        let format = ::image::guess_format(&bytes).map_err(|error| error.to_string())?;
        let decoded = ::image::load(std::io::Cursor::new(&bytes), format)
            .map_err(|error| error.to_string())?;
        let rgba = decoded.to_rgba8();
        eprintln!(
            "[studio] preview {id}: {format:?} {}x{}",
            rgba.width(),
            rgba.height()
        );
        Ok((rgba.width(), rgba.height(), rgba.into_raw()))
    });
    Loaded { id, meta, thumb }
}

struct Studio {
    i18n: I18n,
    snapshot: WorkshopSnapshot,
    selected: Option<usize>,
    note: String,
    meta: HashMap<u64, Meta>,
    thumbs: HashMap<u64, Thumb>,
    pending_remove: Option<u64>,
    /// Where SteamCMD downloads into — the only place "Remove local copy" may touch.
    steamcmd_root: Option<PathBuf>,
    overlay: Vec<OverlayInstance>,
    overlay_selected: Option<usize>,
    /// Overlays this window started, so their exits can be reaped.
    children: Vec<std::process::Child>,
    /// Fingerprint of the control files as of the last read (see [`overlay_signature`]).
    overlay_signature: String,
    /// Modification time of the focus marker this window already followed, so a new claim
    /// switches the selection exactly once.
    focus_seen: Option<std::time::SystemTime>,
}

impl Studio {
    fn boot() -> Self {
        let mut studio = Self {
            i18n: I18n::discover(),
            snapshot: WorkshopSnapshot::default(),
            selected: None,
            note: String::new(),
            meta: HashMap::new(),
            thumbs: HashMap::new(),
            pending_remove: None,
            steamcmd_root: SteamCmd::detect().map(|steamcmd| steamcmd.content_root()),
            overlay: Vec::new(),
            overlay_selected: None,
            children: Vec::new(),
            overlay_signature: String::new(),
            focus_seen: None,
        };
        studio.refresh();
        studio.reload_overlay();
        studio
    }

    fn refresh(&mut self) {
        let mut watcher = WorkshopWatcher::discover();
        let _ = watcher.poll();
        self.snapshot = watcher.snapshot().clone();
        self.note = format!(
            "{} root(s) · {} item(s) · catalog {} · {} entries",
            watcher.roots().len(),
            self.snapshot.len(),
            self.i18n
                .catalog_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string()),
            self.i18n.entries()
        );
        eprintln!("[studio] {}", self.note);
    }

    fn item(&self, index: usize) -> Option<&WorkshopItem> {
        self.snapshot.items.values().nth(index)
    }

    /// Load metadata + preview for `id` on the task pool (the HTTP + decode are
    /// blocking, so they must not run on the UI thread).
    fn load_thumb_task(&mut self, id: u64, dir: PathBuf) -> Task<Message> {
        self.thumbs.insert(id, Thumb::Loading);
        Task::perform(async move { load_item(id, dir) }, |loaded: Loaded| {
            Message::Loaded(loaded.id, loaded.meta, loaded.thumb)
        })
    }

    /// Re-read every control file. The selection is kept by label, so a refresh (or a
    /// write we just made) does not move it.
    fn reload_overlay(&mut self) {
        let previous = self
            .overlay_selected
            .and_then(|index| self.overlay.get(index))
            .map(|instance| instance.label.clone());
        self.overlay = overlay_instances();
        // Prefer an instance that is actually running. The files are sorted by slug, so the
        // first one is arbitrary — and if it belongs to an instance that exited, every
        // button writes into a file nobody reads, which looks like "nothing happens" while
        // the user drags a different, live overlay.
        let fallback = self
            .overlay
            .iter()
            .position(|instance| instance.running)
            .or(if self.overlay.is_empty() {
                None
            } else {
                Some(0)
            });
        self.overlay_selected = previous
            .and_then(|label| {
                self.overlay
                    .iter()
                    .position(|instance| instance.label == label)
            })
            .or(fallback);
        // Follow the overlay the user last touched (a drag, the tray, a shortcuts portal).
        // The files are sorted by slug, so nothing else tells this window which of a dozen
        // instances they mean — and picking one by hand was the wrong interface.
        let control_dir = user_data_dir().join(anima_engine_rs::control::CONTROL_DIR);
        if let Some(focused) = ControlState::focused_in(&control_dir) {
            let stamp = std::fs::metadata(ControlState::focus_path(&focused))
                .and_then(|meta| meta.modified())
                .ok();
            if stamp.is_some() && stamp != self.focus_seen {
                self.focus_seen = stamp;
                if let Some(index) = self
                    .overlay
                    .iter()
                    .position(|instance| instance.path == focused)
                {
                    self.overlay_selected = Some(index);
                    eprintln!("[studio] following overlay {}", self.overlay[index].label);
                }
            }
        }
        self.overlay_signature = overlay_signature();
    }

    /// One poll tick: re-read only when the fingerprint moved, so an overlay that changes
    /// its own state (drag, scale, tray) shows up here without rebuilding every second.
    fn poll_overlay(&mut self) {
        // Wait for overlays we started: an unreaped child becomes a zombie, and a zombie
        // still shows up in `/proc`, which would make a dead instance look alive.
        self.children
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
        let signature = overlay_signature();
        if signature == self.overlay_signature {
            return;
        }
        self.overlay_signature = signature;
        self.reload_overlay();
        eprintln!(
            "[studio] overlay panel refreshed: {} instance(s)",
            self.overlay.len()
        );
    }

    /// Write one control file, exactly as `anima-overlay-ctl` would: load, change a
    /// field (which bumps the generation), save. The overlay picks up the newest
    /// generation on its next poll.
    fn apply_overlay(&mut self, action: OverlayAction) {
        let Some(index) = self.overlay_selected else {
            self.note = "select an overlay instance first".to_string();
            return;
        };
        let Some(instance) = self.overlay.get(index) else {
            return;
        };
        if !instance.running {
            // Say so instead of writing into a file nobody reads.
            self.note = format!(
                "{} is not running — start it with Play, or `anima-shepherd enable`",
                instance.label
            );
            return;
        }
        let label = instance.label.clone();
        let next = match action {
            OverlayAction::ToggleHidden => instance.state.clone().toggle_hidden(),
            OverlayAction::ToggleLocked => instance.state.clone().toggle_locked(),
            OverlayAction::ToggleClickThrough => instance.state.clone().toggle_click_through(),
            OverlayAction::ScaleUp => instance.state.clone().nudge_scale(10),
            OverlayAction::ScaleDown => instance.state.clone().nudge_scale(-10),
            OverlayAction::Rotate => instance.state.clone().rotate_by(90),
            OverlayAction::FlipH => instance.state.clone().toggle_flip_h(),
            OverlayAction::FlipV => instance.state.clone().toggle_flip_v(),
            OverlayAction::Reset => instance.state.clone().request_reset(),
            OverlayAction::Quit => instance.state.clone().request_quit(),
        };
        self.note = match next.save(&instance.path) {
            Ok(()) => format!("{label}: generation {}", next.generation),
            Err(error) => format!("could not write the control file: {error}"),
        };
        self.overlay_selected = Some(index);
        self.reload_overlay();
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Language(code) => {
                if let Err(error) = self.i18n.set_language(&code) {
                    self.note = format!("language not saved: {error}");
                }
            }
            Message::Refresh => self.refresh(),
            Message::Select(index) => {
                self.selected = Some(index);
                if let Some(item) = self.item(index) {
                    let (id, dir) = (item.id, item.path.clone());
                    if !self.thumbs.contains_key(&id) {
                        return self.load_thumb_task(id, dir);
                    }
                }
            }
            Message::Loaded(id, meta, thumb) => {
                if let Some(meta) = meta {
                    self.meta.insert(id, meta);
                }
                self.thumbs.insert(
                    id,
                    match thumb {
                        Ok((width, height, rgba)) => {
                            Thumb::Ready(image::Handle::from_rgba(width, height, rgba))
                        }
                        Err(error) => Thumb::Failed(error),
                    },
                );
            }
            Message::CopyPath(path) => {
                self.note = format!("copied {path}");
                return iced::clipboard::write(path);
            }
            Message::Download(id) => {
                if self.steamcmd_root.is_none() {
                    self.note = "steamcmd not found (set STEAMCMD / STEAMCMD_DIR)".to_string();
                } else {
                    self.note = format!("downloading {id} with SteamCMD…");
                    // Detached on purpose: a download takes minutes and the window must
                    // stay responsive. `SteamCmd` is built inside the thread so nothing
                    // borrows the UI state.
                    std::thread::spawn(move || match SteamCmd::detect() {
                        Some(steamcmd) => match steamcmd.download(id) {
                            Ok(dir) => eprintln!("[studio] downloaded {id} -> {}", dir.display()),
                            Err(error) => eprintln!("[studio] download {id} failed: {error}"),
                        },
                        None => eprintln!("[studio] download {id}: steamcmd not found"),
                    });
                }
            }
            Message::OpenPage(id) => {
                // Subscribe and unsubscribe both live in the Steam client; this is the
                // page that carries the toggle.
                let url = format!("steam://url/CommunityFilePage/{id}");
                self.note = match std::process::Command::new("xdg-open").arg(&url).spawn() {
                    Ok(_) => format!("opened {url} — subscribe/unsubscribe there"),
                    Err(error) => format!("could not open {url}: {error}"),
                };
            }
            Message::ArmRemove(id) => {
                self.pending_remove = Some(id);
                self.note = format!("remove the local copy of {id}? press again to confirm");
            }
            Message::Remove(id) => {
                self.pending_remove = None;
                let root = self.steamcmd_root.clone();
                let Some(item) = self.snapshot.items.get(&id) else {
                    return Task::none();
                };
                let ours = root
                    .as_ref()
                    .is_some_and(|root| item.path.starts_with(root));
                if !ours {
                    self.note = "only our own SteamCMD downloads can be removed here; \
                                 unsubscribe in Steam"
                        .to_string();
                } else {
                    let path = item.path.clone();
                    self.note = match std::fs::remove_dir_all(&path) {
                        Ok(()) => {
                            self.thumbs.remove(&id);
                            self.meta.remove(&id);
                            format!("removed the local copy of {id}")
                        }
                        Err(error) => format!("remove failed: {error}"),
                    };
                    self.refresh();
                }
            }
            Message::RefreshOverlay => self.reload_overlay(),
            Message::SelectOverlay(label) => {
                self.overlay_selected = self
                    .overlay
                    .iter()
                    .position(|instance| instance.label == label);
            }
            Message::Overlay(action) => self.apply_overlay(action),
            Message::PlayAsset(asset) => self.play_on_overlay(&asset),
            Message::PollOverlay => self.poll_overlay(),
            Message::Quit => return iced::exit(),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let header = row![
            text(self.i18n.tr(APP_TITLE)).size(24),
            space().width(Length::Fill),
            text(self.i18n.tr("Language")),
            pick_list(
                self.languages(),
                Some(Language(self.i18n.language().to_string())),
                |language: Language| Message::Language(language.0),
            ),
            button(text(self.i18n.tr("Refresh")))
                .style(secondary)
                .on_press(Message::Refresh),
            button(text(self.i18n.tr("Quit")))
                .style(text_button)
                .on_press(Message::Quit),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center);

        let mut list = column![].spacing(4);
        for (index, (id, item)) in self.snapshot.items.iter().enumerate() {
            let name = self
                .meta
                .get(id)
                .map(|meta| meta.title.clone())
                .filter(|title| !title.is_empty())
                .or_else(|| {
                    item.assets
                        .first()
                        .and_then(|asset| asset.file_name())
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .unwrap_or_else(|| self.i18n.tr("Workshop"));
            // Two lines per row, and the accent colour is reserved for the primary
            // action, so selection is shown with a marker instead.
            let marker = if self.selected == Some(index) {
                "▸ "
            } else {
                ""
            };
            list = list.push(
                button(
                    column![
                        text(format!("{marker}{name}")).size(14),
                        text(format!(
                            "{id} · {} {}",
                            item.assets.len(),
                            self.i18n.tr("assets")
                        ))
                        .size(11),
                    ]
                    .spacing(2),
                )
                .style(secondary)
                .on_press(Message::Select(index))
                .width(Length::Fill),
            );
        }

        let details: Element<'_, Message> = match self.selected.and_then(|index| self.item(index)) {
            Some(item) => {
                let id = item.id;
                // The pane reads: where it lives → what it looks like → what it is →
                // what you can do → what it contains. Raw paths and the CDN URL are not
                // rendered: they are unbreakable tokens that wreck the wrapping, so the
                // directory is shortened and the full path sits behind "Copy path".
                let mut pane = column![
                    row![
                        text(shorten(&item.path)).size(11),
                        button(text(self.i18n.tr("Copy path")))
                            .style(secondary)
                            .on_press(Message::CopyPath(item.path.display().to_string())),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center)
                ]
                .spacing(10);

                match self.thumbs.get(&id) {
                    Some(Thumb::Ready(handle)) => {
                        pane = pane.push(
                            container(image(handle.clone()).content_fit(iced::ContentFit::Contain))
                                .width(Length::Fill)
                                .height(Length::Fixed(PREVIEW_HEIGHT)),
                        );
                    }
                    Some(Thumb::Loading) => {
                        pane = pane.push(
                            container(text("…").size(12)).height(Length::Fixed(PREVIEW_HEIGHT)),
                        );
                    }
                    Some(Thumb::Failed(error)) => {
                        pane = pane.push(
                            container(
                                text(format!("{} · {error}", self.i18n.tr("No preview"))).size(12),
                            )
                            .height(Length::Fixed(PREVIEW_HEIGHT)),
                        );
                    }
                    None => {}
                }

                if let Some(meta) = self.meta.get(&id) {
                    pane = pane.push(text(meta.title.clone()).size(20));
                    let mut line = format!(
                        "{} {} · {} {}",
                        meta.subscriptions,
                        self.i18n.tr("subscriptions"),
                        meta.views,
                        self.i18n.tr("views")
                    );
                    if meta.stale {
                        line.push_str(&format!(" · {}", self.i18n.tr("cached")));
                    }
                    pane = pane.push(text(line).size(12));
                }

                pane = pane.push(divider(Length::Fill, Length::Fixed(1.0)));

                let mut actions = row![];
                if let Some(playable) = item.assets.iter().find(|asset| is_playable(asset)) {
                    actions = actions.push(
                        button(text(self.i18n.tr("Play")))
                            .style(primary)
                            .on_press(Message::PlayAsset(playable.clone())),
                    );
                }
                actions = actions
                    .push(
                        button(text(format!("{} (SteamCMD)", self.i18n.tr("Download"))))
                            .style(secondary)
                            .on_press(Message::Download(id)),
                    )
                    .push(
                        button(text(self.i18n.tr("Subscribe")))
                            .style(secondary)
                            .on_press(Message::OpenPage(id)),
                    )
                    .push(
                        button(text(self.i18n.tr("Unsubscribe")))
                            .style(secondary)
                            .on_press(Message::OpenPage(id)),
                    );
                actions = if self.pending_remove == Some(id) {
                    actions.push(
                        button(text(self.i18n.tr("Confirm remove")))
                            .style(danger)
                            .on_press(Message::Remove(id)),
                    )
                } else {
                    actions.push(
                        button(text(self.i18n.tr("Remove local copy")))
                            .style(secondary)
                            .on_press(Message::ArmRemove(id)),
                    )
                };
                pane = pane.push(actions);

                pane = pane.push(divider(Length::Fill, Length::Fixed(1.0)));
                pane = pane.push(
                    text(format!("{} {}", item.assets.len(), self.i18n.tr("assets"))).size(12),
                );
                for asset in &item.assets {
                    if is_playable(asset) {
                        pane = pane.push(
                            button(text(format!("▶ {}", file_name(asset))))
                                .style(text_button)
                                .on_press(Message::PlayAsset(asset.clone())),
                        );
                    } else {
                        pane = pane.push(text(file_name(asset)).size(12));
                    }
                }
                scrollable(pane).into()
            }
            None => text(self.note.clone()).into(),
        };

        let left = column![
            scrollable(list).height(Length::Fill),
            divider(Length::Fill, Length::Fixed(1.0)),
            self.overlay_panel(),
        ]
        .width(Length::FillPortion(2))
        .spacing(10);

        let bodies = row![
            left,
            divider(Length::Fixed(1.0), Length::Fill),
            container(details).width(Length::FillPortion(3)),
        ]
        .spacing(16)
        .height(Length::Fill);

        container(column![header, bodies, text(self.note.clone()).size(11)].spacing(12))
            .padding(16)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The overlay control panel. Same channel as the tray, the portal and the CLI:
    /// everything it does is one write to a control file.
    fn overlay_panel(&self) -> Element<'_, Message> {
        let mut panel = column![
            row![
                text(self.i18n.tr("Overlay")).size(14),
                space().width(Length::Fill),
                text(format!(
                    "shepherd {}",
                    if anima_engine_rs::shepherd::is_up() {
                        "●"
                    } else {
                        "○"
                    }
                ))
                .size(11),
                button(text(self.i18n.tr("Refresh")))
                    .style(text_button)
                    .on_press(Message::RefreshOverlay),
            ]
            .align_y(iced::Alignment::Center)
        ]
        .spacing(6);

        if self.overlay.is_empty() {
            return panel
                .push(text("no control file yet — start an overlay first").size(11))
                .into();
        }

        panel = panel.push(pick_list(
            self.overlay
                .iter()
                .map(|instance| InstanceLabel(instance_label(instance)))
                .collect::<Vec<_>>(),
            self.overlay_selected
                .and_then(|index| self.overlay.get(index))
                .map(|instance| InstanceLabel(instance_label(instance))),
            |label: InstanceLabel| Message::SelectOverlay(label.0),
        ));

        let Some(instance) = self
            .overlay_selected
            .and_then(|index| self.overlay.get(index))
        else {
            return panel.into();
        };
        let state = &instance.state;
        let mode = match state.input_mode {
            InputMode::Full => "full",
            InputMode::Opaque => "opaque",
            InputMode::PassThrough => "pass-through",
        };
        let mut summary = format!(
            "{} {}% · {}° · {mode}",
            if instance.running { "●" } else { "○" },
            state.scale_percent,
            state.rotation
        );
        for (set, key) in [
            (state.hidden, "hidden"),
            (state.locked, "locked"),
            (state.flip_h, "Flip H"),
            (state.flip_v, "Flip V"),
        ] {
            if set {
                summary.push_str(&format!(" · {}", self.i18n.tr(key)));
            }
        }
        panel = panel.push(text(summary).size(11));

        let hide = if state.hidden {
            self.i18n.tr("Show")
        } else {
            self.i18n.tr("Hide")
        };
        let lock = if state.locked {
            self.i18n.tr("Unlock")
        } else {
            self.i18n.tr("Lock")
        };
        panel = panel.push(
            row![
                button(text(hide))
                    .style(secondary)
                    .on_press(Message::Overlay(OverlayAction::ToggleHidden)),
                button(text(lock))
                    .style(secondary)
                    .on_press(Message::Overlay(OverlayAction::ToggleLocked)),
                button(text(self.i18n.tr("Click-through")))
                    .style(secondary)
                    .on_press(Message::Overlay(OverlayAction::ToggleClickThrough)),
            ]
            .spacing(6),
        );
        panel = panel.push(
            row![
                button(text("−10%"))
                    .style(secondary)
                    .on_press(Message::Overlay(OverlayAction::ScaleDown)),
                button(text("+10%"))
                    .style(secondary)
                    .on_press(Message::Overlay(OverlayAction::ScaleUp)),
                button(text("⟳ 90°"))
                    .style(secondary)
                    .on_press(Message::Overlay(OverlayAction::Rotate)),
                button(text(self.i18n.tr("Flip H")))
                    .style(secondary)
                    .on_press(Message::Overlay(OverlayAction::FlipH)),
                button(text(self.i18n.tr("Flip V")))
                    .style(secondary)
                    .on_press(Message::Overlay(OverlayAction::FlipV)),
            ]
            .spacing(6),
        );
        panel = panel.push(
            row![
                button(text(self.i18n.tr("Reset")))
                    .style(secondary)
                    .on_press(Message::Overlay(OverlayAction::Reset)),
                button(text(self.i18n.tr("Quit overlay")))
                    .style(danger)
                    .on_press(Message::Overlay(OverlayAction::Quit)),
            ]
            .spacing(6),
        );
        panel.into()
    }

    /// Put one asset on the overlay. The layer-shell surface belongs to the overlay
    /// process, so this either re-shows the instance that already plays this animation
    /// or starts one for it — the window deliberately does not try to own a layer
    /// surface next to iced's loop and its own wgpu stack.
    fn play_on_overlay(&mut self, asset: &Path) {
        let key = anima_engine_rs::paths::canonical_path(&asset.to_string_lossy());
        let control = ControlState::path_for(Some(&key));
        // The control file survives the process, so "it exists" is not "it is running":
        // ask the liveness marker instead, or a Play after the tray quit would write into
        // a file nobody is reading and nothing would open.
        if ControlState::is_running(&control) {
            let next = ControlState::load(&control).set_hidden(false);
            self.note = match next.save(&control) {
                Ok(()) => format!("showing {} on the overlay", file_name(asset)),
                Err(error) => format!("could not write the control file: {error}"),
            };
            self.reload_overlay();
            return;
        }
        // A shepherd, when one is up, owns starting things: hand the animation over by
        // enabling it in the registry. The registry file is the channel, like every other
        // piece of this program.
        if anima_engine_rs::shepherd::is_up() {
            let registry_path = anima_engine_rs::shepherd::registry_path();
            let mut registry = anima_engine_rs::shepherd::Registry::load(&registry_path);
            self.note = if registry.enable(&asset.to_string_lossy()) {
                match registry.save(&registry_path) {
                    Ok(()) => format!("handed {} to the shepherd", file_name(asset)),
                    Err(error) => format!("could not update the shepherd registry: {error}"),
                }
            } else {
                format!("{} is already wanted by the shepherd", file_name(asset))
            };
            return;
        }
        let Some(binary) = find_overlay_binary() else {
            self.note = "anima-overlay not found — build or install it first (--features overlay)"
                .to_string();
            return;
        };
        self.note = match std::process::Command::new(&binary).arg(asset).spawn() {
            Ok(child) => {
                // Keep the handle so `poll_overlay` can wait for it: a killed overlay that
                // nobody reaps stays a zombie in `/proc`, where it still looks alive.
                self.children.push(child);
                format!("started an overlay for {}", file_name(asset))
            }
            Err(error) => format!("could not start {}: {error}", binary.display()),
        };
    }

    /// The panel follows the control files on its own; the Refresh buttons are for
    /// impatience rather than correctness.
    fn subscription(&self) -> Subscription<Message> {
        iced::time::every(std::time::Duration::from_millis(OVERLAY_POLL_MS))
            .map(|_| Message::PollOverlay)
    }

    /// The four primary languages, labelled with their native names.
    fn languages(&self) -> Vec<Language> {
        PRIMARY_LANGUAGES
            .iter()
            .map(|code| Language((*code).to_string()))
            .collect()
    }
}

fn main() -> iced::Result {
    iced::application(Studio::boot, Studio::update, Studio::view)
        .title(APP_TITLE)
        // A minimum size keeps the two panes from collapsing into overflow.
        .window(iced::window::Settings {
            size: iced::Size::new(1100.0, 720.0),
            min_size: Some(iced::Size::new(760.0, 520.0)),
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: APP_ID.to_string(),
                ..Default::default()
            },
            ..Default::default()
        })
        .subscription(Studio::subscription)
        .run()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Boot does not touch a window or the GPU, so this runs headless: it proves the
    /// catalog and the Workshop scan are wired the same way the rest of the tree is.
    #[test]
    fn boots_with_the_catalog_and_the_workshop() {
        let studio = Studio::boot();
        assert_eq!(studio.languages().len(), PRIMARY_LANGUAGES.len());
        assert!(PRIMARY_LANGUAGES.contains(&studio.i18n.language()));
        assert_ne!(native_name("zh-CN"), "zh-CN");
        assert_ne!(native_name("ja"), "ja");
        assert!(!studio.note.is_empty());
        eprintln!("[studio] {}", studio.note);
    }

    #[test]
    fn thumbnail_cache_lives_under_the_data_dir() {
        let dir = thumb_dir();
        assert!(dir.ends_with("thumbnails"));
        assert!(dir.starts_with(user_data_dir()));
    }

    /// The panel must target exactly the files `anima-overlay-ctl` targets. Read-only, so
    /// it is safe to run against the real data dir.
    #[test]
    fn overlay_instances_match_the_control_dir() {
        let instances = overlay_instances();
        assert_eq!(instances.len(), ControlState::instances().len());
        for instance in &instances {
            assert!(instance.path.is_file());
            assert!(!instance.label.is_empty());
        }
        eprintln!("[studio] {} overlay control file(s)", instances.len());
    }

    /// "Play" must find the instance that already plays an asset, or decide to start one.
    /// Read-only: it only computes the control-file path the same way the CLI does.
    #[test]
    fn play_targets_the_right_control_file() {
        let studio = Studio::boot();
        let instances = ControlState::instances();
        let mut matched = 0;
        for item in studio.snapshot.items.values() {
            for asset in item.assets.iter().filter(|asset| is_playable(asset)) {
                let key = anima_engine_rs::paths::canonical_path(&asset.to_string_lossy());
                if instances.contains(&ControlState::path_for(Some(&key))) {
                    matched += 1;
                    break;
                }
            }
        }
        eprintln!("[studio] {matched} item(s) already have an overlay instance");
        let sample = ControlState::path_for(Some("/tmp/anima-studio-test.apng"));
        assert!(sample.starts_with(user_data_dir()));
        assert_eq!(
            sample.extension().and_then(|extension| extension.to_str()),
            Some("json")
        );
    }

    /// The fingerprint must be stable while nothing changes, and cover the control dir.
    #[test]
    fn the_metadata_cache_expires_after_a_day() {
        let now = 1_000_000u64;
        assert!(cache_is_fresh(now, now), "just fetched");
        assert!(cache_is_fresh(now - (META_TTL_SECS - 1), now), "23h59m");
        assert!(!cache_is_fresh(now - META_TTL_SECS, now), "a day old");
        // A clock that jumped backwards must not invalidate everything.
        assert!(cache_is_fresh(now + 60, now));
    }

    #[test]
    fn cached_metadata_round_trips_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("42.json");
        let entry = CachedMeta {
            fetched: 1234,
            details: Meta {
                title: "hatsune".to_string(),
                subscriptions: 3,
                views: 2,
                preview_url: "https://example.invalid/p.png".to_string(),
                stale: true,
            },
        };
        anima_engine_rs::paths::atomic_write_json(&path, &entry).unwrap();
        let back: CachedMeta =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back.fetched, 1234);
        assert_eq!(back.details.title, "hatsune");
        assert_eq!(back.details.subscriptions, 3);
        assert!(
            !back.details.stale,
            "`stale` describes this run, so it is not written"
        );
    }

    #[test]
    fn overlay_signature_is_stable_and_covers_the_control_dir() {
        let first = overlay_signature();
        assert_eq!(first, overlay_signature());
        if !ControlState::instances().is_empty() {
            assert!(first.contains("overlay_control"));
            assert!(first.contains(".json"));
        }
    }

    /// Opt-in (hits the network): `cargo test --features studio --bin anima-studio --
    /// --ignored --nocapture`. Fetches the metadata and the CDN preview for the first
    /// installed item, decodes it, and proves the second load comes from the cache.
    #[test]
    #[ignore = "network: Steam Web API + CDN"]
    fn fetches_and_decodes_a_real_preview() {
        let studio = Studio::boot();
        let (id, dir) = studio
            .snapshot
            .items
            .iter()
            .next()
            .map(|(id, item)| (*id, item.path.clone()))
            .expect("no Workshop item installed");
        let first = load_item(id, dir.clone());
        assert!(first.meta.is_some(), "metadata came back empty");
        let (width, height, rgba) = first.thumb.expect("preview");
        assert!(width > 8 && height > 8);
        assert_eq!(rgba.len(), (width * height * 4) as usize);

        let cached = thumb_dir().join(format!("{id}.img"));
        eprintln!(
            "[studio] {id}: {width}x{height} decoded, cache {} ({} bytes)",
            cached.display(),
            cached.metadata().map(|meta| meta.len()).unwrap_or(0)
        );
        assert!(cached.is_file(), "the preview was not cached");

        // The metadata is cached too, and a second load must reuse it rather than asking
        // Steam again: the `fetched` stamp would move if it had been rewritten.
        let meta_cache = meta_cache_path(id);
        assert!(meta_cache.is_file(), "the metadata was not cached");
        let stamp = std::fs::read_to_string(&meta_cache).unwrap();
        assert!(stamp.contains("fetched"));

        let second = load_item(id, dir);
        assert!(second.thumb.is_ok());
        assert_eq!(
            std::fs::read_to_string(&meta_cache).unwrap(),
            stamp,
            "the cache was refetched instead of reused"
        );
    }

    /// Opt-in, and it needs a proxy that goes nowhere:
    ///   HTTPS_PROXY=http://127.0.0.1:9 cargo test --features studio --bin anima-studio -- \
    ///     --ignored --nocapture offline
    /// Run the online test once first so the cache exists.
    #[test]
    #[ignore = "proxy: point HTTPS_PROXY at a closed port"]
    fn offline_serves_the_stale_cache() {
        let studio = Studio::boot();
        let (id, _) = studio
            .snapshot
            .items
            .iter()
            .next()
            .map(|(id, item)| (*id, item.path.clone()))
            .expect("no Workshop item installed");
        // Age the cache past the TTL, so the branch under test is "Steam is unreachable and
        // the copy we have is old" rather than "the copy is fresh".
        let path = meta_cache_path(id);
        let mut entry: CachedMeta =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        entry.fetched = now_secs() - META_TTL_SECS - 60;
        anima_engine_rs::paths::atomic_write_json(&path, &entry).unwrap();

        let meta = meta_for(id).expect("the cache should still answer");
        assert!(meta.stale, "useless data must be labelled stale");
        assert!(!meta.title.is_empty());
        eprintln!("[studio] offline: served \"{}\" from the cache", meta.title);
    }
}
