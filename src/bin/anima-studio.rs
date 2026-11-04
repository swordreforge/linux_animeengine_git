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
use iced::widget::{
    button, column, container, image, pick_list, row, scrollable, space, stack, text, text_input,
};
use iced::{Element, Length, Subscription, Task};
use iced_material::button::{filled as primary, outlined as secondary, text as text_button};
use serde::{Deserialize, Serialize};

const APP_TITLE: &str = "AnimaEngine";
/// Wayland app id / X11 class. It matches `packaging/anima-engine-studio.desktop`, which
/// is what lets the compositor associate the window with its entry (icon, name, grouping).
/// Without it the app id is empty and the window shows up unattached.
const APP_ID: &str = "anima-engine-studio";
/// Fixed height of the preview slot, so items with different aspect ratios do not make
/// everything below them jump.
const PREVIEW_HEIGHT: f32 = 200.0;

/// Longest edge of a cached asset still. The pane draws it into `PREVIEW_HEIGHT`, so the
/// cache does not need the file's full size.
const ASSET_PREVIEW_PX: u32 = 512;

/// Marks a cached asset still: a magic, the size, then straight RGBA.
const ASSET_PREVIEW_MAGIC: &[u8; 8] = b"ANIPREV1";

/// One decoded still: width, height, straight RGBA.
type Still = (u32, u32, Vec<u8>);
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

/// The element type of this window: `iced_material`'s theme is what the whole view speaks.
type El<'a> = Element<'a, Message, iced_material::Theme>;

/// iced_material has no danger style, and Material 3 does not either — destructive actions use
/// the error role, which is what this is.
fn danger(
    theme: &iced_material::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    use iced::widget::button;
    let colors = theme.colors();
    let (background, text_color) = match status {
        button::Status::Hovered | button::Status::Pressed => {
            (colors.error.container, colors.error.container_text)
        }
        button::Status::Disabled => (colors.surface.container.high, colors.surface.text_variant),
        _ => (colors.error.color, colors.error.text),
    };
    button::Style {
        background: Some(background.into()),
        text_color,
        // Same pill radius iced_material's `styled()` gives every other button.
        border: iced::Border {
            radius: 9999.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// A round Material 3 floating-action button: primary container, full radius. Used for refresh,
/// where the action is an affordance rather than a word.
fn fab(
    theme: &iced_material::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    use iced::widget::button;
    let colors = theme.colors();
    let (background, text_color) = match status {
        button::Status::Hovered | button::Status::Pressed => {
            (colors.primary.color, colors.primary.text)
        }
        button::Status::Disabled => (colors.surface.container.high, colors.surface.text_variant),
        _ => (colors.primary.container, colors.primary.container_text),
    };
    button::Style {
        background: Some(background.into()),
        text_color,
        border: iced::Border {
            radius: 9999.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

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

/// Where noctalia (or any Material You renderer following the same convention) leaves the
/// dynamic palette: `$XDG_CACHE_HOME/nyxuri/palette.toml`, key = "#rrggbb".
const PALETTE_HINT: &str = "nyxuri/palette.toml";

fn palette_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(path) if Path::new(&path).is_absolute() => PathBuf::from(path),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".cache"),
    };
    let path = base.join(PALETTE_HINT);
    path.is_file().then_some(path)
}

/// The `key = "#rrggbb"` roles in a Material You palette file.
fn palette_roles(text: &str) -> std::collections::HashMap<&str, iced::Color> {
    let mut roles: std::collections::HashMap<&str, iced::Color> = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim();
        if let Some(color) = parse_hex(value) {
            roles.insert(key.trim(), color);
        }
    }
    roles
}

fn role(roles: &std::collections::HashMap<&str, iced::Color>, name: &str) -> Option<iced::Color> {
    roles.get(name).copied()
}

/// Build `iced_material`'s own colour scheme from the palette file: every role lines up
/// one-to-one with Material 3's names.
fn parse_color_scheme(text: &str) -> Option<iced_material::ColorScheme> {
    use iced_material::{ColorQuartet, ColorScheme, Inverse, Outline, Surface, SurfaceContainer};

    let roles = palette_roles(text);
    let quartet = |base: &str, on: &str, container: &str, on_container: &str| {
        Some(ColorQuartet {
            color: role(&roles, base)?,
            text: role(&roles, on)?,
            container: role(&roles, container)?,
            container_text: role(&roles, on_container)?,
        })
    };
    let surface = role(&roles, "surface").or_else(|| role(&roles, "background"))?;
    let on_surface = role(&roles, "on_surface").or_else(|| role(&roles, "on_background"))?;

    Some(ColorScheme {
        primary: quartet(
            "primary",
            "on_primary",
            "primary_container",
            "on_primary_container",
        )?,
        secondary: quartet(
            "secondary",
            "on_secondary",
            "secondary_container",
            "on_secondary_container",
        )?,
        tertiary: quartet(
            "tertiary",
            "on_tertiary",
            "tertiary_container",
            "on_tertiary_container",
        )?,
        error: quartet("error", "on_error", "error_container", "on_error_container")?,
        surface: Surface {
            color: surface,
            text: on_surface,
            text_variant: role(&roles, "on_surface_variant").unwrap_or(on_surface),
            container: SurfaceContainer {
                lowest: role(&roles, "surface_container_lowest").unwrap_or(surface),
                low: role(&roles, "surface_container_low").unwrap_or(surface),
                base: role(&roles, "surface_container").unwrap_or(surface),
                high: role(&roles, "surface_container_high").unwrap_or(surface),
                highest: role(&roles, "surface_container_highest").unwrap_or(surface),
            },
        },
        // Material 3's inverse roles are not in this file; swapping the surface pair is the
        // honest approximation rather than inventing colours.
        inverse: Inverse {
            inverse_surface: on_surface,
            inverse_surface_text: surface,
            inverse_primary: role(&roles, "primary")?,
        },
        outline: Outline {
            color: role(&roles, "outline").unwrap_or(on_surface),
            variant: role(&roles, "outline_variant").unwrap_or(on_surface),
        },
        shadow: iced::Color::BLACK,
        scrim: iced::Color::BLACK,
    })
}

/// `#rrggbb` or `#rrggbbaa` to a colour.
fn parse_hex(value: &str) -> Option<iced::Color> {
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    let channel = |range: std::ops::Range<usize>| {
        u8::from_str_radix(&hex[range], 16)
            .ok()
            .map(|value| f32::from(value) / 255.0)
    };
    Some(iced::Color {
        r: channel(0..2)?,
        g: channel(2..4)?,
        b: channel(4..6)?,
        a: if hex.len() == 8 { channel(6..8)? } else { 1.0 },
    })
}

/// The window's theme, resolved once at boot: noctalia's dynamic Material You scheme when it is
/// there, otherwise iced_material's Material 3 dark scheme.
fn resolve_theme() -> iced_material::Theme {
    if let Some(path) = palette_path()
        && let Ok(text) = std::fs::read_to_string(&path)
        && let Some(scheme) = parse_color_scheme(&text)
    {
        eprintln!("[studio] theme: Material You from {}", path.display());
        return iced_material::Theme::new("Material You (noctalia)", scheme);
    }
    iced_material::Theme::Dark
}

/// 1px separator. 0.14 dropped the `vertical_rule`/`horizontal_rule` helpers, so a line
/// is a zero-content container that only carries a background.
fn divider<'a>(
    width: Length,
    height: Length,
) -> iced::widget::Container<'a, Message, iced_material::Theme> {
    container(space())
        .width(width)
        .height(height)
        .style(|theme: &iced_material::Theme| container::Style {
            background: Some(theme.colors().surface.container.high.into()),
            ..Default::default()
        })
}

/// A box the size of a preview, for the states that have no picture yet or cannot make one.
/// Same height and surface as a real preview, so the pane does not jump when one arrives.
fn placeholder<'a>(
    icon: &'a str,
    note: String,
) -> iced::widget::Container<'a, Message, iced_material::Theme> {
    container(
        column![
            text(icon)
                .size(40)
                .style(iced_material::text::surface_variant),
            text(note)
                .size(11)
                .style(iced_material::text::surface_variant),
        ]
        .spacing(6)
        .align_x(iced::Alignment::Center),
    )
    .style(iced_material::container::surface_container_low)
    .width(Length::Fill)
    .height(Length::Fixed(PREVIEW_HEIGHT))
    .align_x(iced::Alignment::Center)
    .align_y(iced::Alignment::Center)
}

/// The click-catcher behind the floating picker: a scrim that takes the press which dismisses
/// it. Esc would be nicer, but iced 0.14 removed the key-press API (see docs/next-steps.md).
fn scrim(
    _theme: &iced_material::Theme,
    _status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    iced::widget::button::Style {
        background: Some(
            iced::Color {
                a: 0.35,
                ..iced::Color::BLACK
            }
            .into(),
        ),
        ..Default::default()
    }
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

/// One result from the Steam Workshop search. It is not necessarily installed — that is the
/// point of the search.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteRow {
    id: u64,
    title: String,
    subscriptions: u64,
}

/// What to send Steam as `search_text`: trimmed, or nothing to browse with.
fn search_text(query: &str) -> Option<String> {
    let text = query.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Ask Steam for Workshop items. Needs `STEAM_WEB_API_KEY`; a missing key is reported by name
/// so the caller can say exactly what is wrong instead of showing an empty list.
fn fetch_remote(query: Option<String>) -> Result<Vec<RemoteRow>, String> {
    let web = WorkshopWeb::new().map_err(|error| error.to_string())?;
    if !web.has_api_key() {
        return Err(format!(
            "{} is not set — a Steam Web API key is needed for browsing; installed items work without one",
            anima_engine_rs::workshop_web::API_KEY_ENV
        ));
    }
    let details = web
        .query_files(anima_engine_rs::workshop::APP_ID, 1, 50, query.as_deref())
        .map_err(|error| error.to_string())?;
    Ok(details
        .into_iter()
        .filter_map(|details| {
            let id = details.id()?;
            Some(RemoteRow {
                id,
                title: details.title,
                subscriptions: details.subscriptions,
            })
        })
        .collect())
}

/// Does `item` match `query`? The id, the Steam title and the asset file names are searched,
/// case-insensitively; an empty query matches everything. Pure, so list filtering is tested
/// without a window.
fn matches(item: &WorkshopItem, title: Option<&str>, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    if item.id.to_string().contains(&query) {
        return true;
    }
    if title.is_some_and(|title| title.to_lowercase().contains(&query)) {
        return true;
    }
    item.assets.iter().any(|asset| {
        asset
            .file_name()
            .map(|name| name.to_string_lossy().to_lowercase().contains(&query))
            .unwrap_or(false)
    })
}

/// A `pick_list` entry that carries the control file (the identity) and shows the label.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstanceChoice {
    control: PathBuf,
    label: String,
}

impl std::fmt::Display for InstanceChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

/// The `pick_list` entry for one instance.
fn instance_choice(instance: &OverlayInstance) -> InstanceChoice {
    InstanceChoice {
        control: instance.path.clone(),
        label: instance_label(instance),
    }
}

/// `● running` / `○ stopped` in front of the label: the only thing that tells a live
/// instance from one whose file is left over.
fn instance_label(instance: &OverlayInstance) -> String {
    format!(
        "{} {}",
        if instance.running { "●" } else { "○" },
        // A control-file slug says nothing, and the animation path is long enough to push the
        // row out of a narrow pane, so it is shortened here rather than at every use.
        shorten(Path::new(&instance.label))
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
    Search(String),
    ClearSearch,
    /// Search the Workshop itself (needs `STEAM_WEB_API_KEY`).
    SearchSteam,
    RemoteResults(Result<Vec<RemoteRow>, String>),
    ShowInstalled,
    /// Open the floating picker: typing, or the button in the search row.
    OpenPicker,
    /// Dismiss it without choosing: the ×, or a press anywhere outside it.
    ClosePicker,
    /// Turn "follow the overlay you touched" on or off.
    ToggleFollow,
    Select(u64),
    Loaded(u64, Option<Meta>, Result<(u32, u32, Vec<u8>), String>),
    CopyPath(String),
    Download(u64),
    OpenPage(u64),
    ArmRemove(u64),
    Remove(u64),
    /// Put one workshop asset on the overlay: show the instance if it already exists,
    /// otherwise start one.
    PlayAsset(PathBuf),
    /// Show one asset's first frame in the tuning area. This is what a row's click does now,
    /// which is why the rows no longer carry their own play button.
    SelectAsset(PathBuf),
    /// A first frame came back from the worker — decoded once, then cached on disk.
    AssetPreview(PathBuf, Result<Still, String>),
    /// Tune the previewed asset before it plays: the same writes as the overlay panel, but
    /// aimed at the control file of an animation that is not running yet. That is the point
    /// of staging — the file is the channel, so the parameters are already there when the
    /// overlay starts.
    StageOverlay(OverlayAction),
    RefreshOverlay,
    SelectOverlay(PathBuf),
    Overlay(OverlayAction),
    PollOverlay,
    Quit,
}

fn thumb_dir() -> PathBuf {
    user_data_dir().join("thumbnails")
}

/// What one action does to a state. The panel (a running instance) and the tuning area (an
/// asset that has not started) both go through here, so the two cannot drift apart.
fn apply_action(state: ControlState, action: OverlayAction) -> ControlState {
    match action {
        OverlayAction::ToggleHidden => state.toggle_hidden(),
        OverlayAction::ToggleLocked => state.toggle_locked(),
        OverlayAction::ToggleClickThrough => state.toggle_click_through(),
        OverlayAction::ScaleUp => state.nudge_scale(10),
        OverlayAction::ScaleDown => state.nudge_scale(-10),
        OverlayAction::Rotate => state.rotate_by(90),
        OverlayAction::FlipH => state.toggle_flip_h(),
        OverlayAction::FlipV => state.toggle_flip_v(),
        OverlayAction::Reset => state.request_reset(),
        OverlayAction::Quit => state.request_quit(),
    }
}

/// Where one asset's cached first frame lives. The key is the file's own identity — path,
/// size and mtime — so a re-encoded asset is read again and nothing else is.
fn asset_cache_path(asset: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};

    let meta = std::fs::metadata(asset).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    asset.as_os_str().hash(&mut hasher);
    meta.len().hash(&mut hasher);
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs()
        .hash(&mut hasher);
    Some(thumb_dir().join(format!("a{:016x}.img", hasher.finish())))
}

/// Decode one asset's first frame, shrink it and keep it in the cache. Blocking — a large
/// APNG takes long enough that this has to run on the task pool, not the UI thread.
fn load_asset_preview(asset: &Path) -> (PathBuf, Result<Still, String>) {
    let cache = asset_cache_path(asset);
    if let Some(cache) = &cache
        && let Ok(bytes) = std::fs::read(cache)
        && let Some(still) = decode_asset_preview(&bytes)
    {
        return (asset.to_path_buf(), Ok(still));
    }
    let loaded = match anima_engine_rs::anim::first_frame(asset) {
        Ok((width, height, rgba)) => {
            let still = shrink_preview(width, height, rgba);
            if still.2.is_empty() {
                Err("could not read a first frame".to_string())
            } else {
                if let Some(cache) = &cache {
                    if let Err(error) = ensure_dir(&thumb_dir()) {
                        eprintln!("[studio] could not create the preview cache: {error}");
                    } else if let Err(error) = std::fs::write(cache, encode_asset_preview(&still)) {
                        eprintln!("[studio] could not cache the asset preview: {error}");
                    }
                }
                Ok(still)
            }
        }
        Err(error) => Err(error.to_string()),
    };
    (asset.to_path_buf(), loaded)
}

/// Scale a decoded frame down for the pane. A no-op for anything small enough already.
fn shrink_preview(width: u32, height: u32, rgba: Vec<u8>) -> Still {
    let longest = width.max(height);
    if longest <= ASSET_PREVIEW_PX || width == 0 || height == 0 {
        return (width, height, rgba);
    }
    if rgba.len() != (width as usize) * (height as usize) * 4 {
        return (width, height, rgba);
    }
    let scale = ASSET_PREVIEW_PX as f32 / longest as f32;
    let target = (
        ((width as f32 * scale).round() as u32).max(1),
        ((height as f32 * scale).round() as u32).max(1),
    );
    let Some(source) = ::image::RgbaImage::from_raw(width, height, rgba) else {
        // Unreachable after the length check; returning is better than panicking on a worker.
        return (width, height, Vec::new());
    };
    let resized = ::image::imageops::resize(
        &source,
        target.0,
        target.1,
        ::image::imageops::FilterType::Triangle,
    );
    (resized.width(), resized.height(), resized.into_raw())
}

fn encode_asset_preview((width, height, rgba): &Still) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + rgba.len());
    out.extend_from_slice(ASSET_PREVIEW_MAGIC);
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(rgba);
    out
}

/// The bytes [`encode_asset_preview`] wrote, or `None` when the file is not ours (an older
/// shape, a truncated write) — a cache miss, never an error.
fn decode_asset_preview(bytes: &[u8]) -> Option<Still> {
    if bytes.len() < 16 || &bytes[..8] != ASSET_PREVIEW_MAGIC {
        return None;
    }
    let width = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let height = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
    let expected = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    if expected == 0 {
        return None;
    }
    let rgba = bytes.get(16..16 + expected)?;
    Some((width, height, rgba.to_vec()))
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
    /// The selected Workshop item, by id: an index moves when the list changes under it.
    selected_item: Option<u64>,
    /// List filter. The selection is kept by id, so hiding the selected row does not lose it.
    query: String,
    /// Material 3 colours, resolved at boot: noctalia's dynamic scheme when present, else
    /// iced_material's dark theme.
    theme: iced_material::Theme,
    /// Whether the panel follows the overlay the user last touched. Locking it keeps the
    /// selection on one instance while several are being dragged in turn.
    follow: bool,
    /// Steam search results, when the list is showing them instead of what is installed.
    remote: Option<Vec<RemoteRow>>,
    /// Why a Steam search produced nothing (missing key, offline), shown in the status line.
    remote_note: String,
    /// Whether the floating picker is up. The workshop list used to be a permanent region of
    /// the left column, where it squeezed the overlay panel; now it floats over the left half
    /// and the panel owns the column.
    picker: bool,
    note: String,
    meta: HashMap<u64, Meta>,
    thumbs: HashMap<u64, Thumb>,
    /// First frames of assets, by path: a path is what the tuning area acts on, and two
    /// items can share a directory.
    asset_previews: HashMap<PathBuf, Thumb>,
    /// The asset being previewed, by path. Nothing is shown until one is picked.
    previewed_asset: Option<PathBuf>,
    /// The previewed asset's staged state as last read or written. The view must not read a
    /// JSON file on every frame, and this is what the summary line is built from.
    staged: Option<(PathBuf, ControlState)>,
    pending_remove: Option<u64>,
    /// Where SteamCMD downloads into — the only place "Remove local copy" may touch.
    steamcmd_root: Option<PathBuf>,
    overlay: Vec<OverlayInstance>,
    /// The selected overlay instance, by its control file: labels and order change.
    overlay_selected: Option<PathBuf>,
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
            selected_item: None,
            query: String::new(),
            follow: true,
            theme: resolve_theme(),
            remote: None,
            remote_note: String::new(),
            picker: false,
            note: String::new(),
            meta: HashMap::new(),
            thumbs: HashMap::new(),
            asset_previews: HashMap::new(),
            previewed_asset: None,
            staged: None,
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

    fn item(&self, id: u64) -> Option<&WorkshopItem> {
        self.snapshot.items.get(&id)
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
        // The control file is the identity: labels and order can change between reads.
        let previous = self.overlay_selected.clone();
        self.overlay = overlay_instances();
        // Prefer an instance that is actually running. The files are sorted by slug, so the
        // first one is arbitrary — and if it belongs to an instance that exited, every
        // button writes into a file nobody reads, which looks like "nothing happens" while
        // the user drags a different, live overlay.
        let fallback = self
            .overlay
            .iter()
            .find(|instance| instance.running)
            .or_else(|| self.overlay.first())
            .map(|instance| instance.path.clone());
        self.overlay_selected = previous
            .filter(|selected| {
                self.overlay
                    .iter()
                    .any(|instance| instance.path == *selected)
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
                // The stamp is recorded either way, so locking and unlocking the follow does
                // not make the panel jump to some old claim.
                if self.follow
                    && let Some(instance) = self
                        .overlay
                        .iter()
                        .find(|instance| instance.path == focused)
                {
                    self.overlay_selected = Some(instance.path.clone());
                    eprintln!("[studio] following overlay {}", instance.label);
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
        let Some(selected) = self.overlay_selected.clone() else {
            self.note = "select an overlay instance first".to_string();
            return;
        };
        let Some(instance) = self
            .overlay
            .iter()
            .find(|instance| instance.path == selected)
        else {
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
        let next = apply_action(instance.state.clone(), action);
        self.note = match next.save(&instance.path) {
            Ok(()) => format!("{label}: generation {}", next.generation),
            Err(error) => format!("could not write the control file: {error}"),
        };
        self.overlay_selected = Some(selected);
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
            Message::Search(query) => {
                self.query = query;
                // Typing is how you look for something, so it brings the picker up.
                self.picker = true;
            }
            Message::ClearSearch => self.query.clear(),
            Message::SearchSteam => {
                let query = search_text(&self.query);
                self.remote_note = "searching the Workshop…".to_string();
                self.picker = true;
                return Task::perform(async move { fetch_remote(query) }, Message::RemoteResults);
            }
            Message::RemoteResults(Ok(rows)) => {
                self.remote_note = match rows.len() {
                    0 => "no Workshop results".to_string(),
                    count => format!("{count} Workshop result(s)"),
                };
                self.remote = Some(rows);
                self.picker = true;
            }
            Message::RemoteResults(Err(error)) => {
                self.remote = None;
                self.remote_note = error;
            }
            Message::ShowInstalled => {
                self.remote = None;
                self.remote_note.clear();
                self.picker = true;
            }
            Message::OpenPicker => self.picker = true,
            Message::ClosePicker => self.picker = false,
            Message::Select(id) => {
                // A different item means a different set of assets: the one on show, and the
                // parameters staged for it, belong to the old item and go with it.
                if self.selected_item != Some(id) {
                    self.previewed_asset = None;
                    self.staged = None;
                }
                // Choosing is what the picker is for, so choosing closes it.
                self.picker = false;
                self.selected_item = Some(id);
                // A remote result has no directory yet; its preview comes from the CDN.
                let dir = self
                    .item(id)
                    .map(|item| item.path.clone())
                    .unwrap_or_default();
                if !self.thumbs.contains_key(&id) {
                    return self.load_thumb_task(id, dir);
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
                // The tuning area must not keep pointing at a file the removal takes away.
                let gone = self.previewed_asset.as_ref().is_some_and(|asset| {
                    self.item(id)
                        .is_some_and(|item| asset.starts_with(&item.path))
                });
                if gone {
                    self.previewed_asset = None;
                    self.staged = None;
                }
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
            Message::ToggleFollow => {
                self.follow = !self.follow;
                self.note = format!(
                    "{}: {}",
                    self.i18n.tr("follow"),
                    if self.follow { "●" } else { "○" }
                );
            }
            Message::SelectOverlay(control) => self.overlay_selected = Some(control),
            Message::Overlay(action) => self.apply_overlay(action),
            Message::SelectAsset(asset) => return self.select_asset(asset),
            Message::AssetPreview(asset, loaded) => {
                self.asset_previews.insert(
                    asset,
                    match loaded {
                        Ok((width, height, rgba)) => {
                            Thumb::Ready(image::Handle::from_rgba(width, height, rgba))
                        }
                        Err(error) => Thumb::Failed(error),
                    },
                );
            }
            Message::StageOverlay(action) => self.stage_overlay(action),
            Message::PlayAsset(asset) => self.play_on_overlay(&asset),
            Message::PollOverlay => self.poll_overlay(),
            Message::Quit => return iced::exit(),
        }
        Task::none()
    }

    fn view(&self) -> El<'_> {
        let header = row![
            text(self.i18n.tr(APP_TITLE)).size(24),
            space().width(Length::Fill),
            text(self.i18n.tr("Language")),
            pick_list(
                self.languages(),
                Some(Language(self.i18n.language().to_string())),
                |language: Language| Message::Language(language.0),
            )
            .style(iced_material::pick_list::default),
            button(text("↻").size(18))
                .style(fab)
                .width(Length::Fixed(36.0))
                .height(Length::Fixed(36.0))
                .on_press(Message::Refresh),
            button(text(self.i18n.tr("Quit")))
                .style(text_button)
                .on_press(Message::Quit),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center);

        // The rows live in the floating picker now: the left column is the overlay panel's.
        let selected_item = self.selected_item.and_then(|id| self.item(id).cloned());
        let selected_remote = self.selected_item.and_then(|id| {
            self.remote
                .iter()
                .flatten()
                .find(|row| row.id == id)
                .cloned()
        });
        let details: El<'_> = match (selected_item, selected_remote) {
            (Some(item), _) => {
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
                        pane = pane.push(placeholder("▦", "…".to_string()));
                    }
                    Some(Thumb::Failed(error)) => {
                        pane = pane.push(placeholder(
                            "⚠",
                            format!("{} · {error}", self.i18n.tr("No preview")),
                        ));
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

                // Playing belongs to the tuning area below, where the asset on show is the
                // one that plays: an item-level Play had to guess which asset that was.
                let mut actions = row![];
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
                pane = pane.push(actions.spacing(6).wrap());

                pane = pane.push(divider(Length::Fill, Length::Fixed(1.0)));
                pane = pane.push(
                    text(format!("{} {}", item.assets.len(), self.i18n.tr("assets"))).size(12),
                );
                // A row is a choice, not an action: clicking it shows that file's first frame
                // in the tuning area below, where the play button and the parameters live.
                for asset in &item.assets {
                    if is_playable(asset) {
                        let marker = if self.previewed_asset.as_ref() == Some(asset) {
                            "▸ "
                        } else {
                            ""
                        };
                        pane = pane.push(
                            button(text(format!("{marker}{}", file_name(asset))))
                                .style(text_button)
                                .on_press(Message::SelectAsset(asset.clone())),
                        );
                    } else {
                        // Not an animation: there is no first frame to show.
                        pane = pane.push(text(file_name(asset)).size(12));
                    }
                }
                pane = pane.push(divider(Length::Fill, Length::Fixed(1.0)));
                pane = pane.push(self.asset_preview_area());
                scrollable(pane)
                    .style(iced_material::scrollable::default)
                    .into()
            }
            (None, Some(row)) => {
                let id = row.id;
                let mut pane = column![
                    text(format!("☁ {}", row.title)).size(20),
                    text(format!(
                        "{} · {} {}",
                        id,
                        row.subscriptions,
                        self.i18n.tr("subscriptions")
                    ))
                    .size(12),
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
                        pane = pane.push(placeholder("▦", "…".to_string()));
                    }
                    Some(Thumb::Failed(error)) => {
                        pane = pane.push(placeholder(
                            "⚠",
                            format!("{} · {error}", self.i18n.tr("No preview")),
                        ));
                    }
                    None => {}
                }

                pane = pane.push(divider(Length::Fill, Length::Fixed(1.0)));
                pane = pane.push(
                    row![
                        button(text(format!("{} (SteamCMD)", self.i18n.tr("Download"))))
                            .style(primary)
                            .on_press(Message::Download(id)),
                        button(text(self.i18n.tr("Subscribe")))
                            .style(secondary)
                            .on_press(Message::OpenPage(id)),
                    ]
                    .spacing(8),
                );
                pane = pane.push(text(format!("({})", self.i18n.tr("not installed"))).size(11));
                scrollable(pane)
                    .style(iced_material::scrollable::default)
                    .into()
            }
            (None, None) => text(self.note.clone()).into(),
        };

        let placeholder = self.i18n.tr("Search");
        let mut search = row![
            text_input(&placeholder, &self.query)
                .on_input(Message::Search)
                // iced_material has no text_input catalog, but it does ship this style.
                .style(iced_material::text_input::default)
                .width(Length::Fill)
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        if !self.query.trim().is_empty() {
            search = search.push(
                button(text("×"))
                    .style(secondary)
                    .on_press(Message::ClearSearch),
            );
        }
        search = search.push(
            button(text(format!("{} Steam", self.i18n.tr("Search"))))
                .style(secondary)
                .on_press(Message::SearchSteam),
        );
        // The list lives in the floating picker now, so the search row is the way to it.
        search = search.push(
            button(text(self.i18n.tr("Workshop")))
                .style(if self.picker { primary } else { secondary })
                .on_press(Message::OpenPicker),
        );

        let left = column![search, self.overlay_panel()]
            .width(Length::FillPortion(2))
            .spacing(10);

        let bodies = row![
            left,
            divider(Length::Fixed(1.0), Length::Fill),
            container(details)
                .style(iced_material::container::surface_container_low)
                .width(Length::FillPortion(3)),
        ]
        .spacing(16)
        .height(Length::Fill);

        let scene = container(
            column![
                header,
                bodies,
                text(if self.remote_note.is_empty() {
                    self.note.clone()
                } else {
                    self.remote_note.clone()
                })
                .size(11)
            ]
            .spacing(12),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill);

        let Some(picker) = self.picker_layer() else {
            return scene.into();
        };
        // The picker floats over the left half: the scene behind it, then a scrim that takes
        // the press that dismisses it, then the picker itself.
        stack![
            scene,
            button(space())
                .style(scrim)
                .width(Length::Fill)
                .height(Length::Fill)
                .on_press(Message::ClosePicker),
            container(row![picker, space().width(Length::FillPortion(1))])
                .padding(16)
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .into()
    }

    /// The installed items whose title, id or asset names match the query.
    fn filtered_installed(&self) -> Vec<(&u64, &WorkshopItem)> {
        self.snapshot
            .items
            .iter()
            .filter(|(id, item)| {
                matches(
                    item,
                    self.meta.get(id).map(|meta| meta.title.as_str()),
                    &self.query,
                )
            })
            .collect()
    }

    /// The picker's rows: whatever is installed (already filtered), or the Steam results.
    fn result_rows(&self, installed: &[(&u64, &WorkshopItem)]) -> El<'_> {
        let mut list = column![].spacing(4);
        for &(id, item) in installed {
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
            // Two lines per row, and the accent colour is reserved for the primary action, so
            // selection is shown with a marker instead.
            let marker = if self.selected_item == Some(*id) {
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
                .on_press(Message::Select(*id))
                .width(Length::Fill),
            );
        }
        if let Some(rows) = &self.remote {
            for row in rows {
                let marker = if self.selected_item == Some(row.id) {
                    "▸ "
                } else {
                    ""
                };
                list = list.push(
                    button(
                        column![
                            text(format!("{marker}☁ {}", row.title)).size(14),
                            text(format!(
                                "{} · {} {}",
                                row.id,
                                row.subscriptions,
                                self.i18n.tr("subscriptions")
                            ))
                            .size(11),
                        ]
                        .spacing(2),
                    )
                    .style(secondary)
                    .on_press(Message::Select(row.id))
                    .width(Length::Fill),
                );
            }
        }
        if installed.is_empty() && self.remote.is_none() && !self.snapshot.is_empty() {
            list = list.push(text("—").size(12));
        }
        list.into()
    }

    /// The floating workspace: the workshop browser over the left half of the window. It exists
    /// because the list used to be a permanent region of the left column, where it both squeezed
    /// the overlay panel and had no height to show more than a row or two.
    fn picker_layer(&self) -> Option<El<'_>> {
        if !self.picker {
            return None;
        }
        let installed = self.filtered_installed();
        let count = match &self.remote {
            Some(rows) => rows.len(),
            None => installed.len(),
        };
        let title = if self.remote.is_some() {
            format!("{} · {count}", self.i18n.tr("Search"))
        } else {
            format!(
                "{} · {count}/{}",
                self.i18n.tr("Workshop"),
                self.snapshot.len()
            )
        };
        let mut header = row![text(title).size(14), space().width(Length::Fill)]
            .spacing(8)
            .align_y(iced::Alignment::Center);
        if self.remote.is_some() {
            header = header.push(
                button(text(self.i18n.tr("Workshop")))
                    .style(secondary)
                    .on_press(Message::ShowInstalled),
            );
        }
        header = header.push(
            button(text("×"))
                .style(secondary)
                .on_press(Message::ClosePicker),
        );

        let mut body = column![
            scrollable(self.result_rows(&installed))
                .style(iced_material::scrollable::default)
                .height(Length::Fill)
        ]
        .spacing(8);
        if !self.remote_note.is_empty() {
            body = body.push(text(self.remote_note.clone()).size(11));
        }
        Some(
            container(column![header, divider(Length::Fill, Length::Fixed(1.0)), body].spacing(10))
                .style(iced_material::container::surface_container_low)
                .padding(12)
                .width(Length::FillPortion(1))
                .height(Length::Fill)
                .into(),
        )
    }

    /// The overlay control panel. Same channel as the tray, the portal and the CLI:
    /// everything it does is one write to a control file.
    fn overlay_panel(&self) -> El<'_> {
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
                button(text(format!(
                    "{} {}",
                    self.i18n.tr("follow"),
                    if self.follow { "●" } else { "○" }
                )))
                .style(if self.follow { primary } else { secondary })
                .on_press(Message::ToggleFollow),
                button(text("↻").size(14))
                    .style(fab)
                    .width(Length::Fixed(28.0))
                    .height(Length::Fixed(28.0))
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

        panel = panel.push(
            pick_list(
                self.overlay.iter().map(instance_choice).collect::<Vec<_>>(),
                self.overlay_selected
                    .as_ref()
                    .and_then(|selected| {
                        self.overlay
                            .iter()
                            .find(|instance| instance.path == *selected)
                    })
                    .map(instance_choice),
                |choice: InstanceChoice| Message::SelectOverlay(choice.control),
            )
            .style(iced_material::pick_list::default),
        );

        let Some(instance) = self.overlay_selected.as_ref().and_then(|selected| {
            self.overlay
                .iter()
                .find(|instance| instance.path == *selected)
        }) else {
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

        panel = panel.push(self.overlay_params(Some(state), Message::Overlay));
        panel = panel.push(
            row![
                button(text(self.i18n.tr("Quit overlay")))
                    .style(danger)
                    .on_press(Message::Overlay(OverlayAction::Quit))
            ]
            .spacing(6),
        );
        panel.into()
    }

    /// Show one asset's first frame in the tuning area. The cache answers instantly;
    /// otherwise the decode runs on the task pool and comes back as [`Message::AssetPreview`].
    /// The staged state is read here, once, so the summary line never opens a file per frame.
    fn select_asset(&mut self, asset: PathBuf) -> Task<Message> {
        self.previewed_asset = Some(asset.clone());
        let key = anima_engine_rs::paths::canonical_path(&asset.to_string_lossy());
        let control = ControlState::path_for(Some(&key));
        self.staged = Some((asset.clone(), ControlState::load(&control)));
        if self.asset_previews.contains_key(&asset) {
            return Task::none();
        }
        self.asset_previews.insert(asset.clone(), Thumb::Loading);
        Task::perform(
            async move { load_asset_preview(&asset) },
            |(asset, loaded)| Message::AssetPreview(asset, loaded),
        )
    }

    /// Stage one parameter change for the previewed asset: load its control file (which may
    /// not exist yet), change a field — bumping the generation — and save. Play then starts
    /// an overlay that reads exactly this file, so what is set here is what it starts with.
    fn stage_overlay(&mut self, action: OverlayAction) {
        let Some(asset) = self.previewed_asset.clone() else {
            self.note =
                "pick an asset first: its parameters are staged in its control file".to_string();
            return;
        };
        if matches!(action, OverlayAction::Quit) {
            self.note = format!("{} is not running", file_name(&asset));
            return;
        }
        let key = anima_engine_rs::paths::canonical_path(&asset.to_string_lossy());
        let control = ControlState::path_for(Some(&key));
        let state = self
            .staged
            .as_ref()
            .filter(|(path, _)| *path == asset)
            .map(|(_, state)| state.clone())
            .unwrap_or_else(|| ControlState::load(&control));
        let next = apply_action(state, action);
        self.note = match next.save(&control) {
            Ok(()) => format!(
                "{}: staged generation {}",
                file_name(&asset),
                next.generation
            ),
            Err(error) => format!("could not stage the parameters: {error}"),
        };
        self.staged = Some((asset, next));
    }

    /// What the tuning area says it will start with — the same summary the panel shows, from
    /// the copy this window last read or wrote.
    fn staged_summary(&self, asset: &Path) -> String {
        let Some((path, state)) = &self.staged else {
            return String::new();
        };
        if path != asset {
            return String::new();
        }
        let mode = match state.input_mode {
            InputMode::Full => "full",
            InputMode::Opaque => "opaque",
            InputMode::PassThrough => "pass-through",
        };
        let mut summary = format!("{}% · {}° · {mode}", state.scale_percent, state.rotation);
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
        summary
    }

    /// The parameter buttons, shared by the panel (a running instance) and the tuning area
    /// (an asset that has not started): one widget set, two targets, so they cannot drift.
    fn overlay_params(
        &self,
        state: Option<&ControlState>,
        tag: fn(OverlayAction) -> Message,
    ) -> El<'_> {
        let hidden = state.is_some_and(|state| state.hidden);
        let locked = state.is_some_and(|state| state.locked);
        column![
            row![
                button(text(if hidden {
                    self.i18n.tr("Show")
                } else {
                    self.i18n.tr("Hide")
                }))
                .style(if hidden { primary } else { secondary })
                .on_press(tag(OverlayAction::ToggleHidden)),
                button(text(if locked {
                    self.i18n.tr("Unlock")
                } else {
                    self.i18n.tr("Lock")
                }))
                .style(if locked { primary } else { secondary })
                .on_press(tag(OverlayAction::ToggleLocked)),
                button(text(self.i18n.tr("Click-through")))
                    .style(secondary)
                    .on_press(tag(OverlayAction::ToggleClickThrough)),
            ]
            .spacing(6)
            .wrap(),
            row![
                button(text("−10%"))
                    .style(secondary)
                    .on_press(tag(OverlayAction::ScaleDown)),
                button(text("+10%"))
                    .style(secondary)
                    .on_press(tag(OverlayAction::ScaleUp)),
                button(text("⟳ 90°"))
                    .style(secondary)
                    .on_press(tag(OverlayAction::Rotate)),
                button(text(self.i18n.tr("Flip H")))
                    .style(secondary)
                    .on_press(tag(OverlayAction::FlipH)),
                button(text(self.i18n.tr("Flip V")))
                    .style(secondary)
                    .on_press(tag(OverlayAction::FlipV)),
            ]
            .spacing(6)
            .wrap(),
            row![
                button(text(self.i18n.tr("Reset")))
                    .style(secondary)
                    .on_press(tag(OverlayAction::Reset)),
            ]
            .spacing(6),
        ]
        .spacing(6)
        .into()
    }

    /// The tuning area: the asset that was clicked, its first frame, and the parameters it
    /// will start with. Empty until an asset is picked, so nothing here acts on a guess.
    fn asset_preview_area(&self) -> El<'_> {
        let Some(asset) = self.previewed_asset.as_ref() else {
            return placeholder(
                "▦",
                self.i18n.tr("Click an asset to preview it").to_string(),
            )
            .into();
        };
        let mut area = column![text(file_name(asset)).size(14)].spacing(8);
        match self.asset_previews.get(asset) {
            Some(Thumb::Ready(handle)) => {
                area = area.push(
                    container(image(handle.clone()).content_fit(iced::ContentFit::Contain))
                        .width(Length::Fill)
                        .height(Length::Fixed(PREVIEW_HEIGHT)),
                );
            }
            Some(Thumb::Loading) => {
                area = area.push(placeholder("▦", "…".to_string()));
            }
            Some(Thumb::Failed(error)) => {
                area = area.push(placeholder(
                    "⚠",
                    format!("{} · {error}", self.i18n.tr("No preview")),
                ));
            }
            None => {}
        }
        area = area.push(
            row![
                button(text(self.i18n.tr("Play")))
                    .style(primary)
                    .on_press(Message::PlayAsset(asset.clone())),
                text(self.staged_summary(asset)).size(11),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        area = area.push(
            self.overlay_params(
                self.staged
                    .as_ref()
                    .filter(|(path, _)| path == asset)
                    .map(|(_, state)| state),
                Message::StageOverlay,
            ),
        );
        area.into()
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
        // Keyboard shortcuts are not here yet: iced 0.14 removed `keyboard::on_key_press`, so
        // they have to go through the event-listening API. Noted in docs/next-steps.md.
        iced::time::every(std::time::Duration::from_millis(OVERLAY_POLL_MS))
            .map(|_| Message::PollOverlay)
    }

    /// Material 3's dark colour scheme, mapped onto iced's own palette so the widgets we
    /// already have pick it up — the theme *type* stays `iced::Theme`, which keeps every view
    /// signature and button style untouched. Reversible: delete this and the window falls back
    /// to iced's theme.
    fn theme(&self) -> iced_material::Theme {
        self.theme.clone()
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
        .theme(Studio::theme)
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

    #[test]
    fn noctalia_maps_onto_a_material_colour_scheme() {
        let fixture = "primary = \"#8bd0ef\"\non_primary = \"#003546\"\n\
                       primary_container = \"#004d64\"\non_primary_container = \"#bde9ff\"\n\
                       secondary = \"#b4cad6\"\non_secondary = \"#1f333c\"\n\
                       secondary_container = \"#354a53\"\non_secondary_container = \"#d0e6f2\"\n\
                       tertiary = \"#c6c2ea\"\non_tertiary = \"#2e2d4d\"\n\
                       tertiary_container = \"#454364\"\non_tertiary_container = \"#e3dfff\"\n\
                       error = \"#ffb4ab\"\non_error = \"#690005\"\n\
                       error_container = \"#93000a\"\non_error_container = \"#ffdad6\"\n\
                       surface = \"#0f1417\"\non_surface = \"#dfe3e7\"\n\
                       surface_container = \"#1b2023\"\nsurface_container_high = \"#262b2d\"\n\
                       on_surface_variant = \"#c0c8cd\"\noutline = \"#8a9297\"\n\
                       outline_variant = \"#40484c\"\n";
        let scheme = parse_color_scheme(fixture).expect("scheme");
        assert_eq!(scheme.primary.color, parse_hex("#8bd0ef").unwrap());
        assert_eq!(scheme.primary.text, parse_hex("#003546").unwrap());
        assert_eq!(scheme.primary.container, parse_hex("#004d64").unwrap());
        assert_eq!(scheme.primary.container_text, parse_hex("#bde9ff").unwrap());
        assert_eq!(scheme.error.color, parse_hex("#ffb4ab").unwrap());
        assert_eq!(scheme.surface.color, parse_hex("#0f1417").unwrap());
        assert_eq!(scheme.surface.text, parse_hex("#dfe3e7").unwrap());
        assert_eq!(scheme.surface.container.base, parse_hex("#1b2023").unwrap());
        assert_eq!(scheme.surface.container.high, parse_hex("#262b2d").unwrap());
        assert_eq!(scheme.outline.color, parse_hex("#8a9297").unwrap());
        assert_eq!(scheme.outline.variant, parse_hex("#40484c").unwrap());
        // The inverse roles are not in the file, so they come from the swapped surface pair.
        assert_eq!(scheme.inverse.inverse_surface, scheme.surface.text);
        assert_eq!(scheme.inverse.inverse_surface_text, scheme.surface.color);

        // The live file, when it exists, is the format that actually matters.
        if let Some(path) = palette_path()
            && let Ok(live) = std::fs::read_to_string(&path)
        {
            assert!(
                parse_color_scheme(&live).is_some(),
                "the live palette must parse: {}",
                path.display()
            );
        }
    }

    #[test]
    fn the_palette_parser_rejects_junk() {
        assert!(parse_color_scheme("# only a comment").is_none());
        assert!(parse_color_scheme("primary = \"#12345\"").is_none());
        assert!(parse_hex("#8bd0ef").is_some());
        assert!(parse_hex("8bd0ef").is_none());
    }

    #[test]
    fn the_query_is_trimmed_and_empty_means_browse() {
        assert_eq!(search_text("  miku  ").as_deref(), Some("miku"));
        assert_eq!(search_text("   "), None);
        assert_eq!(search_text(""), None);
    }

    #[test]
    fn a_missing_api_key_is_explained() {
        if std::env::var(anima_engine_rs::workshop_web::API_KEY_ENV).is_ok() {
            return; // this machine has a key; nothing to assert
        }
        let error = fetch_remote(None).expect_err("no key means an error, not an empty list");
        assert!(error.contains("STEAM_WEB_API_KEY"), "{error}");
        eprintln!("[studio] no key: {error}");
    }

    #[test]
    #[ignore = "needs a local Workshop item with an animation"]
    fn decodes_and_caches_a_real_asset_first_frame() {
        let mut watcher = WorkshopWatcher::discover();
        let _ = watcher.poll();
        let asset = watcher
            .snapshot()
            .items
            .values()
            .flat_map(|item| item.assets.iter())
            .find(|asset| is_playable(asset))
            .cloned()
            .expect("a playable asset in the local Workshop");

        let (path, loaded) = load_asset_preview(&asset);
        assert_eq!(path, asset);
        let (width, height, rgba) = loaded.unwrap();
        assert!(width > 0 && height > 0);
        assert_eq!(rgba.len(), (width as usize) * (height as usize) * 4);
        assert!(width.max(height) <= ASSET_PREVIEW_PX);

        // The second call is the cache: it must agree with what was just written.
        let (_, cached) = load_asset_preview(&asset);
        assert_eq!(cached.unwrap(), (width, height, rgba));
        eprintln!("[test] {} → {width}×{height}", file_name(&asset));
    }

    #[test]
    fn a_new_item_drops_the_asset_on_show() {
        let mut studio = Studio::boot();
        let asset = PathBuf::from("/tmp/whatever.apng");
        studio.selected_item = Some(1);
        studio.previewed_asset = Some(asset.clone());
        studio.staged = Some((asset, ControlState::default()));

        // Clicking the row that is already selected is not a reset: the preview stays.
        let _ = studio.update(Message::Select(1));
        assert!(studio.previewed_asset.is_some());

        // Another item has a different set of assets, so the one on show goes with the old one.
        // The task this returns is dropped rather than run, so nothing touches the network.
        let _ = studio.update(Message::Select(2));
        assert!(studio.previewed_asset.is_none());
        assert!(studio.staged.is_none());
    }

    #[test]
    fn typing_opens_the_picker_and_choosing_closes_it() {
        let mut studio = Studio::boot();
        assert!(!studio.picker);

        // Looking for something is what the picker is for, so typing brings it up.
        let _ = studio.update(Message::Search("teto".to_string()));
        assert!(studio.picker);

        // Choosing ends the search; the task this returns is dropped, not run.
        let _ = studio.update(Message::Select(3799603054));
        assert!(!studio.picker);

        let _ = studio.update(Message::OpenPicker);
        assert!(studio.picker);
        let _ = studio.update(Message::ClosePicker);
        assert!(!studio.picker);
    }

    #[test]
    fn a_still_survives_the_preview_cache() {
        let still = (2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        let bytes = encode_asset_preview(&still);
        assert_eq!(decode_asset_preview(&bytes), Some(still));
        // Not ours: too short, another magic, or a size that does not add up.
        assert_eq!(decode_asset_preview(&bytes[..8]), None);
        assert_eq!(decode_asset_preview(b"NOTOURS!________"), None);
        let mut truncated = encode_asset_preview(&(4, 4, vec![0; 64]));
        truncated.truncate(40);
        assert_eq!(decode_asset_preview(&truncated), None);
    }

    #[test]
    fn big_stills_are_shrunk_to_the_cache_cap() {
        let small = vec![0u8; 64 * 32 * 4];
        let (width, height, bytes) = shrink_preview(64, 32, small.clone());
        assert_eq!((width, height), (64, 32));
        assert_eq!(bytes.len(), small.len());

        let (width, height, bytes) = shrink_preview(1024, 512, vec![0u8; 1024 * 512 * 4]);
        assert_eq!((width, height), (ASSET_PREVIEW_PX, ASSET_PREVIEW_PX / 2));
        assert_eq!(bytes.len(), (width as usize) * (height as usize) * 4);
    }

    #[test]
    fn staging_goes_through_the_same_actions_as_the_panel() {
        let state = ControlState::default();
        // 100% is the ceiling: the overlay only ever shrinks, so "up" from the default is a
        // no-op by design, and the test steps down instead.
        let next = apply_action(state.clone(), OverlayAction::ScaleDown);
        assert_eq!(next.scale_percent, state.scale_percent - 10);
        assert!(next.generation > state.generation);
        let rotated = apply_action(next.clone(), OverlayAction::Rotate);
        assert_ne!(rotated.rotation, next.rotation);
    }

    #[test]
    fn the_filter_searches_id_title_and_assets() {
        let item = WorkshopItem {
            id: 3_828_587_753,
            path: PathBuf::from("/w/3828587753"),
            assets: vec![
                PathBuf::from("/w/3828587753/Miku12.APNG"),
                PathBuf::from("/w/3828587753/teto.png"),
            ],
        };
        // Empty and whitespace-only queries keep everything.
        assert!(matches(&item, None, ""));
        assert!(matches(&item, None, "   "));
        // Id, title (either case) and asset names, case-insensitively.
        assert!(matches(&item, None, "3828"));
        assert!(matches(&item, Some("Hatsune Miku"), "hatsune"));
        assert!(matches(&item, Some("Hatsune Miku"), "MIKU"));
        assert!(matches(&item, None, "apng"));
        assert!(matches(&item, None, "teto"));
        // A title is optional, and a miss is a miss.
        assert!(!matches(&item, Some("something else"), "zzz"));
        assert!(!matches(&item, None, "zzz"));
    }

    /// Selections are keyed by id / control path, so rebuilding the list or the instance set
    /// cannot move them to something else. Read-only against the real data dir.
    #[test]
    fn selection_identity_is_stable() {
        // The Workshop side: an id survives the list changing under it.
        let mut studio = Studio::boot();
        let Some(first) = studio.snapshot.items.keys().next().copied() else {
            return;
        };
        studio.selected_item = Some(first);
        assert_eq!(studio.item(first).map(|item| item.id), Some(first));
        assert!(studio.item(first.wrapping_add(1)).is_none() || first == u64::MAX);

        // The overlay side: the control file is the identity, so a reload keeps it.
        let Some(instance) = studio.overlay.first() else {
            return;
        };
        let path = instance.path.clone();
        studio.overlay_selected = Some(path.clone());
        studio.reload_overlay();
        assert_eq!(
            studio.overlay_selected.as_deref(),
            Some(path.as_path()),
            "the control path must survive a reload"
        );
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
