//! `anima-studio` — the main window (Iced).
//!
//! Second slice: Workshop previews and the actions around them.
//!
//! * **Preview**: a `preview.*` file inside the item directory if the author shipped
//!   one, otherwise the Steam CDN image from `preview_url`, cached under
//!   `~/.local/share/AnimaEngine/thumbnails/<id>.img` and decoded with the same
//!   `image` crate the rest of the tree uses.
//! * **Subscribe / Unsubscribe**: one button, labelled for the way the toggle will go, which opens
//!   `steam://url/CommunityFilePage/<id>`. The toggle is the Steam client's to make, and the label
//!   follows the client's own list so it cannot claim a state Steam disagrees with.
//! * **Download**: SteamCMD (`anonymous`), on a detached thread, fetching a second copy under our own
//!   root. Greyed until the Steam client has the item, because that is the order the real flow has: the
//!   workshop *item* (wallpaper-style artwork, not a paid add-on) is subscribed in Steam, Steam does its
//!   own download, and `ItemState::pending` — the wanted manifest past the installed one — is how that
//!   download is visible from out here.
//! * **Remove local copy**: only for items under *our* SteamCMD root, behind a
//!   confirmation click. It never touches the user's own Steam library.
//!
//! Requires the `studio` feature: `cargo run --features studio --bin anima-studio`.
//!
//! Note: this pulls a second `wgpu` (iced 0.14 is on 27.x, the overlay is on 29.x).
//! They never meet — the overlay is a separate process and keeps owning its own GPU
//! stack — so the cost is build time and binary size, not correctness.

// The Tools tab lives in its own file: it is the part of this window that changes most, and a
// 5000-line file is no place to be making careful edits.
#[path = "anima-studio/library_update.rs"]
pub(crate) mod library_update;
#[path = "anima-studio/settings_update.rs"]
pub(crate) mod settings_update;
#[path = "anima-studio/tools/mod.rs"]
pub(crate) mod tools;
#[path = "anima-studio/workshop_update.rs"]
pub(crate) mod workshop_update;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anima_engine_rs::control::{ControlState, InputMode, instance_slug};
use anima_engine_rs::i18n::{I18n, PRIMARY_LANGUAGES, STEAM_LANGUAGES, normalize_language};
use anima_engine_rs::log;
use anima_engine_rs::paths::{ensure_dir, user_data_dir};
use anima_engine_rs::state::SecondaryState;
use anima_engine_rs::steamcmd::SteamCmd;
use anima_engine_rs::workshop::{WorkshopItem, WorkshopSnapshot, WorkshopWatcher};
use anima_engine_rs::workshop_web::WorkshopWeb;
use std::time::Instant;

use iced::animation::{Animation, Easing};
use iced::widget::{
    button, column, container, image, pick_list, row, scrollable, slider, space, stack, text,
    text_input, tooltip,
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
/// How often the panel falls back to asking, now that a watcher answers first. The watcher is what
/// makes a tray action or a drag appear immediately; this is the safety net for the events a watcher
/// can miss (an inotify queue that overflowed, a directory replaced under it), so it stays.
const OVERLAY_POLL_MS: u64 = 2000;
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

/// Everything one write needs, owned, so the task can move it off this thread.
struct KeyWriteRequest {
    source: String,
    background: [u8; 3],
    tolerance: u8,
    feather: u8,
    command: String,
    output: String,
    fps: u8,
    quality: u8,
    lossless: bool,
}

/// Decode every frame, key it, write the frames out as PNGs, and hand them to ffmpeg.
///
/// The frames go through a temporary directory because that is the interface the original tools used and
/// the one every encoder accepts — a `%04d.png` pattern — and the point of the template is that the
/// command around it is the user's to change. Returns the path written, or what went wrong: both are data,
/// so the caller is the only place that has to know about language.
fn key_write(request: KeyWriteRequest) -> Result<String, String> {
    let animation = anima_engine_rs::anim::decode(std::path::Path::new(request.source.trim()))
        .map_err(|error| format!("{error}"))?;
    let (width, height) = animation.dimensions();
    let dir = std::env::temp_dir().join(format!("anima-key-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|error| format!("{error}"))?;

    let mut written = 0usize;
    for (index, frame) in animation.frames.iter().enumerate() {
        let mut rgba = frame.rgba.clone();
        anima_engine_rs::keying::key_out(
            &mut rgba,
            request.background,
            request.tolerance,
            request.feather,
        );
        // `::image`, with the leading colons: the widget import above binds the name `image` to
        // `iced::widget::image`, so an unqualified path finds the widget and not the crate.
        let still = ::image::RgbaImage::from_raw(width, height, rgba)
            .ok_or_else(|| "frame does not match the animation's size".to_string())?;
        let path = dir.join(format!("frame_{index:04}.png"));
        still.save(&path).map_err(|error| format!("{error}"))?;
        written += 1;
    }
    if written == 0 {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("the animation has no frames".to_string());
    }

    let pattern = dir.join("frame_%04d.png").display().to_string();
    let fps = request.fps.to_string();
    let quality = request.quality.to_string();
    let lossless = if request.lossless { "1" } else { "0" }.to_string();
    let command = anima_engine_rs::ffmpeg::expand(
        &request.command,
        &[
            ("{frames}", pattern.as_str()),
            ("{output}", request.output.trim()),
            ("{fps}", fps.as_str()),
            ("{quality}", quality.as_str()),
            ("{lossless}", lossless.as_str()),
        ],
    );
    let rendered = anima_engine_rs::ffmpeg::run(&command);
    let _ = std::fs::remove_dir_all(&dir);
    if rendered.ok {
        Ok(request.output.trim().to_string())
    } else {
        Err(rendered.detail)
    }
}

/// Everything the AI worker shares with the UI: how far it has got, and whether it should stop.
/// Atomics rather than channels, because the tick already runs and reading two numbers is not a message.
#[derive(Debug, Default)]
struct AiRunning {
    done: std::sync::atomic::AtomicU64,
    total: std::sync::atomic::AtomicU64,
    stopped: std::sync::atomic::AtomicBool,
}

impl AiRunning {
    fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    fn reset(&self, total: Option<u64>) {
        use std::sync::atomic::Ordering;
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total.unwrap_or(0), Ordering::Relaxed);
        self.stopped.store(false, Ordering::Relaxed);
    }
}

/// One AI pass: ffmpeg gives up the frames, the model cuts them, the results land in a folder.
struct AiRequest {
    video: String,
    output: std::path::PathBuf,
    running: std::sync::Arc<AiRunning>,
}

fn ai_cut(request: AiRequest) -> Result<String, String> {
    use std::sync::atomic::Ordering;

    // A folder beside the user's output rather than in it, so a cancelled run leaves one thing to delete.
    let work = std::env::temp_dir().join(format!("anima-ai-{}", std::process::id()));
    let frames = work.join("frames");
    let _ = std::fs::remove_dir_all(&work);

    let extracted = anima_ai::extract_frames(&request.video, &frames)?;
    let total = Some(extracted);
    request.running.total.store(extracted, Ordering::Relaxed);

    let mut model = anima_ai::Removal::load()?;
    let running = request.running.clone();
    let cut = anima_ai::cut_animation(
        &frames,
        &request.output,
        total,
        |frame| model.remove_background(frame),
        |progress| {
            running.done.store(progress.done, Ordering::Relaxed);
            !running.stopped.load(Ordering::Relaxed)
        },
    )?;
    let _ = std::fs::remove_dir_all(&work);

    if running.stopped.load(Ordering::Relaxed) {
        Ok(format!(
            "{} {cut} {}",
            request.output.display(),
            "frames, then stopped"
        ))
    } else {
        Ok(format!("{} {cut}", request.output.display()))
    }
}

/// One video render: a single ffmpeg call. ffmpeg reads the video itself, so unlike the keying path there
/// is no frame directory to write and clean up.
struct VideoWriteRequest {
    command: String,
    input: String,
    output: String,
    fps: u8,
    quality: u8,
    lossless: bool,
}

fn video_write(request: VideoWriteRequest) -> Result<String, String> {
    let fps = request.fps.to_string();
    let quality = request.quality.to_string();
    let lossless = if request.lossless { "1" } else { "0" }.to_string();
    let command = anima_engine_rs::ffmpeg::expand(
        &request.command,
        &[
            ("{input}", request.input.trim()),
            ("{output}", request.output.trim()),
            ("{fps}", fps.as_str()),
            ("{quality}", quality.as_str()),
            ("{lossless}", lossless.as_str()),
        ],
    );
    let rendered = anima_engine_rs::ffmpeg::run(&command);
    if rendered.ok {
        Ok(request.output.trim().to_string())
    } else {
        Err(rendered.detail)
    }
}

/// How often the window measures the items' directories: twice a second. A download can land between two
/// ticks, so the point is to catch the state while it is true rather than to be frugal — the walk is a few
/// hundred `stat` calls over small trees, and it happens on this tick only.
const WORKSHOP_POLL_MS: u64 = 500;
/// How many quiet ticks before something that stopped growing is no longer shown as downloading. Three at
/// 500 ms means a finished download stays on screen about a second and a half, which is the difference
/// between "I never saw it" and "I saw it finish".
const QUIET_TICKS: u8 = 3;
/// The floor: a full rescan — the list, the states, the measurement — every two seconds no matter what the
/// watcher and the fast tick managed. Whatever they miss, this catches.
const WORKSHOP_FALLBACK_MS: u64 = 2000;

/// The download cell: it exists while the Steam client owes the item a download, and reports once it does
/// not. Nothing to press either way — the download is the client's, and its progress is what Steam writes
/// into the manifest — which is why this widget never has an `on_press` and always renders disabled.
fn download_state(studio: &Studio, pending: bool, bytes: u64) -> El<'_> {
    tooltip(
        button(text(if pending {
            // The number that moves is the progress. No percentage: the `.acf` has no total until the
            // download ends, so a percentage here would be a guess dressed as a measurement.
            fill(
                &studio.i18n.tr("downloading · {}"),
                &[format!("{:.1} MB", bytes as f64 / 1_048_576.0)],
            )
        } else {
            studio.i18n.tr("downloaded")
        }))
        .style(if pending { secondary } else { primary }),
        text(if pending {
            studio
                .i18n
                .tr("Steam is downloading this one; this is how much has landed so far")
        } else {
            studio.i18n.tr("the Steam client has this item")
        }),
        tooltip::Position::Top,
    )
    .into()
}

/// The selected state, carried by colour instead of a "▸" glyph in front of the text.
///
/// A filled accent with the text in `on_primary` is the literal inversion, and it is what the tab bar
/// and single-line buttons use: the button widget colours its own label, so nothing else is needed.
fn selected_fill(
    theme: &iced_material::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    use iced::widget::button;
    let colors = theme.colors();
    let (background, text_color) = match status {
        button::Status::Hovered | button::Status::Pressed => {
            (colors.primary.container, colors.primary.container_text)
        }
        _ => (colors.primary.color, colors.primary.text),
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

/// The selected state for two-line rows, which is *tonal* rather than filled, and not for want of
/// trying: a widget's style colours the widget, not the `text` children inside it, so a filled row would
/// leave its second line in the on-surface colour and unreadable on the accent. `secondary_container`
/// with its own text colour is what Material 3 uses for a selected list item anyway, and it stays
/// legible in every palette because both halves come from the same role.
fn selected_row(
    theme: &iced_material::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    use iced::widget::button;
    let colors = theme.colors();
    let (background, _) = match status {
        button::Status::Hovered | button::Status::Pressed => {
            (colors.secondary.container, colors.secondary.container_text)
        }
        _ => (colors.secondary.container, colors.secondary.container_text),
    };
    button::Style {
        background: Some(background.into()),
        text_color: colors.secondary.container_text,
        border: iced::Border {
            radius: 10.0.into(),
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
        log::event(
            log::Level::Info,
            "studio.theme.material_you",
            &[("from", &path.display())],
        );
        return iced_material::Theme::new("Material You (noctalia)", scheme);
    }
    iced_material::Theme::Dark
}

/// 1px separator. 0.14 dropped the `vertical_rule`/`horizontal_rule` helpers, so a line
/// is a zero-content container that only carries a background.
/// Watches the control directory and the data directory, so a write from the tray, the CLI, the portal
/// or another instance shows up as an event rather than at the next poll.
///
/// The watcher lives on its own thread (it is blocking and must outlive the stream), and the stream only
/// carries "something moved" to the update loop; deciding whether anything actually changed stays with
/// the fingerprint check that already existed.
fn control_events() -> Subscription<Message> {
    iced::Subscription::run(|| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        std::thread::spawn(move || {
            use notify::{RecursiveMode, Watcher};

            let handler = move |event: notify::Result<notify::Event>| {
                if event.is_ok() {
                    let _ = tx.send(());
                }
            };
            let Ok(mut watcher) = notify::recommended_watcher(handler) else {
                log::event(log::Level::Info, "studio.watch.unavailable", &[]);
                return;
            };
            let control = user_data_dir().join(anima_engine_rs::control::CONTROL_DIR);
            // Non-recursive on both: recursive would also report every thumbnail written during a
            // decode, which is a refresh nobody asked for.
            if let Err(error) = watcher.watch(&control, RecursiveMode::NonRecursive) {
                log::event(
                    log::Level::Info,
                    "studio.watch.failed",
                    &[("path", &control.display()), ("error", &error)],
                );
            }
            if let Err(error) = watcher.watch(&user_data_dir(), RecursiveMode::NonRecursive) {
                log::event(
                    log::Level::Info,
                    "studio.watch.data_dir_failed",
                    &[("error", &error)],
                );
            }
            log::event(
                log::Level::Info,
                "studio.watch",
                &[("path", &control.display())],
            );
            // The watcher stops when it is dropped, so this thread keeps it alive for the session.
            loop {
                std::thread::park();
            }
        });
        futures_util::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|()| (Message::ControlChanged, rx))
        })
    })
}

/// 1px separator. 0.14 dropped the `vertical_rule`/`horizontal_rule` helpers, so a line
/// is a zero-content container that only carries a background.
/// A button whose label flips with a flag. The label always says **what pressing it will do** — "Show"
/// while the thing is hidden, "Unlock" while it is locked — and the fill says what the state is. One
/// helper, so the toggles cannot each invent their own convention, which is how "Hide" ends up meaning
/// two different things two rows apart.
fn toggle<'a>(
    theme: &'a Studio,
    on: bool,
    when_on: &str,
    when_off: &str,
    message: Message,
) -> El<'a> {
    button(text(theme.i18n.tr(if on { when_on } else { when_off })))
        .style(if on { primary } else { secondary })
        .on_press(message)
        .into()
}

/// Fill a translated template: `{}` in the catalog entry, the values here. Languages reorder
/// sentences, so the template has to come from the catalog whole rather than being assembled from
/// fragments.
fn fill(template: &str, values: &[String]) -> String {
    let mut out = template.to_string();
    for value in values {
        out = out.replacen("{}", value, 1);
    }
    out
}

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

/// A box for the states that have no picture or no selection: a large muted glyph and one line
/// of explanation, on the same surface as its neighbours. The height is the caller's, so the
/// tuning area can match a preview exactly while a whole pane can fill.
fn placeholder<'a>(
    icon: &'a str,
    note: String,
    height: Length,
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
    .height(height)
    .align_x(iced::Alignment::Center)
    .align_y(iced::Alignment::Center)
}

/// The full path as a hover tip. The panes are previews, so a path only appears when it is
/// asked for instead of holding a row of its own above the picture.
fn path_tip<'a>(path: &Path) -> Element<'a, Message, iced_material::Theme> {
    container(text(path.display().to_string()).size(11))
        .style(iced_material::container::surface_container_high)
        .padding(6)
        .into()
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

/// Which screen the window is showing. One field and one `match`: iced builds only the selected
/// branch, so switching tabs needs no teardown and no callback — the message loop is the callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Workshop,
    Overlays,
    Tools,
    Settings,
    Diagnostics,
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

/// How many results one Workshop request asks for.
const REMOTE_PER_PAGE: u32 = 50;

/// One page of Workshop search results: what Steam says the total is, where the next page starts,
/// and the rows themselves.
#[derive(Debug, Clone)]
struct RemotePage {
    total: u64,
    next_cursor: Option<String>,
    rows: Vec<RemoteRow>,
}

/// How many pages `total` results make, at least one: a pager that says "1/0" is worse than no
/// pager at all.
fn page_count(total: u64, per_page: u32) -> u32 {
    let per_page = u64::from(per_page.max(1));
    (total.div_ceil(per_page)).max(1).min(u64::from(u32::MAX)) as u32
}

/// Ask Steam for one page of Workshop items. Needs `STEAM_WEB_API_KEY`; a missing key is reported
/// by name — and with where to get one — so the caller can say exactly what is wrong instead of
/// showing an empty list.
fn fetch_remote(query: Option<String>, cursor: Option<String>) -> Result<RemotePage, String> {
    let web = WorkshopWeb::new().map_err(|error| error.to_string())?;
    if !web.has_api_key() {
        // Name both places it can live, and say which file: a status line that only complains is
        // not much of a status line. The path is shortened because it is the long part.
        // The window checks this before it starts a task and says so in the user's language; this line
        // is the technical fallback for any other caller, and is never what the UI shows.
        return Err(format!(
            "{} is not set and {} has no steam_api_key",
            anima_engine_rs::workshop_web::API_KEY_ENV,
            shorten(&anima_engine_rs::workshop_web::studio_settings_path())
        ));
    }
    let page = web
        .query_files_page(
            anima_engine_rs::workshop::APP_ID,
            cursor.as_deref(),
            REMOTE_PER_PAGE,
            query.as_deref(),
        )
        .map_err(|error| error.to_string())?;
    Ok(RemotePage {
        total: page.total,
        next_cursor: page.next_cursor,
        rows: page
            .items
            .into_iter()
            .filter_map(|details| {
                let id = details.id()?;
                Some(RemoteRow {
                    id,
                    title: details.title,
                    subscriptions: details.subscriptions,
                })
            })
            .collect(),
    })
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

/// The last path component of a control file's animation, or the label itself when it is not a
/// path (an instance list from an older build).
fn asset_file_name(label: &str) -> String {
    Path::new(label)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| label.to_string())
}

/// A name that fits. Steam assets often carry a timestamp in the middle, so the head and the
/// tail are what is kept and the middle is what goes — the extension survives that way.
fn truncate_middle(name: &str, max: usize) -> String {
    let length = name.chars().count();
    if length <= max || max < 5 {
        return name.to_string();
    }
    let keep_end = max / 2;
    let keep_start = max - keep_end - 1;
    let head: String = name.chars().take(keep_start).collect();
    let tail: String = name.chars().skip(length - keep_end).collect();
    format!("{head}…{tail}")
}

/// Longest a pick-list label gets before its middle is dropped.
const INSTANCE_LABEL_MAX: usize = 28;

/// One animation frame. The window only ticks while something is actually moving, so this is
/// what a spinner costs and nothing more when the window is idle.
const FRAME_MS: u64 = 16;
/// A spinner frame every this many milliseconds: four of them is one turn in 360ms.
const SPINNER_STEP_MS: u64 = 90;
/// How long a preview takes to come up. Short on purpose: it removes the pop, it is not a show.
const FADE_MS: u64 = 160;
/// The spinner's phase and the preview fades both need a frame, and nothing else does: when
/// this is false the window's subscription is a single one-second poll.
fn fade_in(at: Instant, motion: bool) -> Animation<f32> {
    if !motion {
        return Animation::new(1.0);
    }
    Animation::new(0.0)
        .duration(std::time::Duration::from_millis(FADE_MS))
        .easing(Easing::EaseOutCubic)
        .go(1.0, at)
}

/// Motion is decoration, and not everyone can use it. The preference is stored in `studio.json` and
/// shown on the settings tab; `ANIMA_NO_MOTION` overrides it, exactly like the environment overrides
/// the Steam key (a deliberate, temporary override beats a saved preference).
fn motion_enabled() -> bool {
    !reduce_motion()
}

/// Whether motion should be off. Pure parts are split out so the precedence can be tested without
/// touching the environment or the disk.
fn reduce_motion() -> bool {
    use anima_engine_rs::settings::{ConfigFile, StudioSettings};

    let stored = StudioSettings::load(&anima_engine_rs::workshop_web::studio_settings_path());
    resolve_reduce_motion(
        std::env::var("ANIMA_NO_MOTION").ok().as_deref(),
        stored.reduce_motion,
    )
}

/// `ANIMA_NO_MOTION` wins when it says anything but `0`; blank means "not set"; otherwise the stored
/// preference decides.
fn resolve_reduce_motion(env: Option<&str>, stored: bool) -> bool {
    match env.map(str::trim) {
        Some(value) if !value.is_empty() => value != "0",
        _ => stored,
    }
}

/// The loading spinner. Four quadrants in step: enough at this size, and it needs no rotation
/// support — iced cannot rotate text, and an image would mean shipping one.
fn spinner_frame(elapsed_ms: u64) -> &'static str {
    const FRAMES: [&str; 4] = ["◐", "◓", "◑", "◒"];
    FRAMES[(elapsed_ms / SPINNER_STEP_MS % FRAMES.len() as u64) as usize]
}

/// `● running` / `○ stopped` in front of the animation's **file name**: a full Steam path is a
/// mouthful in a closed drop-down, and the path is still in the item pane. Two instances can
/// ship assets with the same name, and only then does the label need the directory in front.
fn instance_label(instance: &OverlayInstance, disambiguate: bool) -> String {
    let name = truncate_middle(&asset_file_name(&instance.label), INSTANCE_LABEL_MAX);
    let name = if disambiguate {
        match Path::new(&instance.label)
            .parent()
            .and_then(Path::file_name)
        {
            Some(directory) => format!("{}/{}", directory.to_string_lossy(), name),
            None => name,
        }
    } else {
        name
    };
    format!("{} {}", if instance.running { "●" } else { "○" }, name)
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
    /// The colour-key panel: the typed path, its background colour, its two numbers.
    KeyPath(String),
    KeyPickFile,
    /// A path the user chose in the dialog: unlike typing, this one is finished, so it loads.
    KeyChosen(String),
    /// Pick one of the shipped presets: replaces the command with that preset's template.
    KeyFormat(String),
    KeyCommand(String),
    KeyFps(u8),
    KeyQuality(u8),
    KeyLossless(bool),
    KeyOutput(String),
    /// Choose where to write, through the same dialog as the input.
    KeyPickOutput,
    /// Encode what is in the previews and write it.
    KeyWrite,
    KeyWrote(Result<String, String>),
    VideoPath(String),
    VideoChosen(String),
    VideoPickFile,
    VideoFormat(String),
    VideoCommand(String),
    VideoFps(u8),
    VideoQuality(u8),
    VideoLossless(bool),
    VideoOutput(String),
    VideoPickOutput,
    VideoWrite,
    VideoWrote(Result<String, String>),
    AiPath(String),
    AiChosen(String),
    AiPickFile,
    AiOutput(String),
    AiPickOutput,
    AiStart,
    AiCancel,
    AiDone(Result<String, String>),
    AiTick,
    KeyLoad,
    KeyBackground([u8; 3]),
    /// Take the background from the image's top-left corner — the one control the original tool did not
    /// have, and the one that saves picking a colour by hand.
    KeyCorner,
    KeyTolerance(u8),
    KeyFeather(u8),
    Refresh,
    Search(String),
    ClearSearch,
    /// Search the Workshop itself (needs `STEAM_WEB_API_KEY`).
    SearchSteam,
    RemoteResults(u64, u32, Result<RemotePage, String>),
    /// Metadata for a batch of installed items, from the startup request.
    MetaLoaded(Vec<(u64, Meta)>),
    /// Ask for another page of the same search.
    RemotePage(u32),
    ShowInstalled,
    Select(u64),
    Loaded(u64, Option<Meta>, Result<(u32, u32, Vec<u8>), String>),
    CopyPath(String),
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
    /// One animation frame, while something is moving.
    Frame(Instant),
    /// A control file or the instance list moved. The panel re-reads, but only if its fingerprint
    /// actually changed, so a burst of events costs one rebuild.
    ControlChanged,
    /// Show another tab. Nothing is cleared: coming back shows what you left.
    Tab(Tab),
    /// Turn "follow the overlay you touched" on or off. It lived on the old panel; it belongs with the
    /// instances, so it moved to the overlays tab's heading.
    ToggleFollow,
    /// What is in the settings tab's key field.
    KeyDraft(String),
    /// Write the key field to `studio.json`.
    SaveKey,
    PollWorkshop,
    /// Close the removal warning without deleting anything.
    CancelRemove,
    /// Flip the motion preference in `studio.json` and re-resolve it.
    ToggleReduceMotion,
    /// Put the diagnostics on the clipboard, for a bug report.
    CopyDiagnostics,
    /// One parameter change, aimed at one instance: the overlays tab has a row per instance, so the
    /// control file has to travel with the action.
    OverlayOn(PathBuf, OverlayAction),
    /// The scale slider moved. Fine-grained on purpose: 1% steps between the same bounds the overlay
    /// enforces (10..=100), instead of only the coarse ±10% buttons.
    SetScale(PathBuf, u32),
    /// The scale entry: type the exact percentage instead of dragging to it. Only digits, three at most,
    /// because 100 is the largest the overlay accepts and anything else is a typo rather than input.
    ScaleDraft(PathBuf, String),
    /// Apply whatever the scale entry says.
    CommitScale(PathBuf),
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
                        log::failure("studio.preview_cache.create_failed", &error);
                    } else if let Err(error) = std::fs::write(cache, encode_asset_preview(&still)) {
                        log::failure("studio.preview_cache.write_failed", &error);
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

/// One `Meta` from an API record. Both the single-item path and the batch path build it here, so the
/// two cannot drift.
fn meta_from(details: anima_engine_rs::workshop_web::PublishedFileDetails) -> Meta {
    Meta {
        title: details.title,
        subscriptions: details.subscriptions,
        views: details.views,
        preview_url: details.preview_url,
        stale: false,
    }
}

/// How many installed items the startup batch asks about. One keyless request covers this many, which
/// is more than a page of the list; anything past it falls back to the file name and is fetched when
/// it is selected, exactly as before.
const INSTALLED_META_LIMIT: usize = 100;

/// The ids worth asking Steam about, in batches of at most [`MAX_IDS_PER_REQUEST`]. The caller says
/// what counts as fresh, so this stays pure and testable: it is the decision, not the request.
fn meta_batches(ids: &[u64], is_fresh: impl Fn(u64) -> bool) -> Vec<Vec<u64>> {
    ids.iter()
        .copied()
        .filter(|id| !is_fresh(*id))
        .collect::<Vec<u64>>()
        .chunks(anima_engine_rs::workshop_web::MAX_IDS_PER_REQUEST)
        .map(|batch| batch.to_vec())
        .collect()
}

/// Fetch metadata for a batch of ids — one keyless request — and cache each answer the same way the
/// single-item path does, so a later [`meta_for`] is a cache hit rather than a request. Blocking:
/// this is the worker side of [`Message::MetaLoaded`].
fn load_metadata(ids: Vec<u64>) -> Vec<(u64, Meta)> {
    let Ok(web) = WorkshopWeb::new() else {
        return Vec::new();
    };
    let details = match web.fetch_published_file_details(&ids) {
        Ok(details) => details,
        Err(error) => {
            // Not fatal, and not worth a status line: the rows keep their file names.
            log::event(
                log::Level::Info,
                "studio.metadata.batch_failed",
                &[("ids", &ids.len()), ("error", &error)],
            );
            return Vec::new();
        }
    };
    let now = now_secs();
    let mut loaded = Vec::with_capacity(details.len());
    for record in details {
        let Some(id) = record.id() else { continue };
        let meta = meta_from(record);
        let entry = CachedMeta {
            fetched: now,
            details: meta.clone(),
        };
        if let Err(error) = ensure_dir(&thumb_dir()) {
            log::failure("studio.metadata_cache.create_failed", &error);
        } else if let Err(error) =
            anima_engine_rs::paths::atomic_write_json(&meta_cache_path(id), &entry)
        {
            log::event(
                log::Level::Info,
                "studio.metadata_cache.write_failed",
                &[("id", &id), ("error", &error)],
            );
        }
        loaded.push((id, meta));
    }
    loaded
}

/// The metadata cached on disk for `id`, if there is any. Both the decision about what to fetch and
/// the act of showing it go through here, so a warm cache cannot be honoured by one and ignored by
/// the other.
fn read_cached_meta(id: u64) -> Option<CachedMeta> {
    std::fs::read_to_string(meta_cache_path(id))
        .ok()
        .and_then(|text| serde_json::from_str::<CachedMeta>(&text).ok())
}

/// The fresh answers among `ids`, as the map the rows read. Pure, so the rule can be tested without a
/// cache on disk: the caller says where a cached answer comes from.
fn hydrate_from(
    ids: impl IntoIterator<Item = u64>,
    now: u64,
    read: impl Fn(u64) -> Option<CachedMeta>,
) -> HashMap<u64, Meta> {
    ids.into_iter()
        .filter_map(|id| {
            let cached = read(id)?;
            cache_is_fresh(cached.fetched, now).then_some((id, cached.details))
        })
        .collect()
}

/// Ask Steam for the metadata of the installed list in one batch, so rows can show titles without
/// being clicked one at a time. Everything the cache already has fresh is skipped, which is what makes
/// a second run offline and instant. `None` when there is nothing to ask about.
fn installed_meta_task(studio: &Studio) -> Option<Task<Message>> {
    let now = now_secs();
    let ids: Vec<u64> = studio
        .snapshot
        .items
        .keys()
        .copied()
        .take(INSTALLED_META_LIMIT)
        .collect();
    let batch = meta_batches(&ids, |id| {
        read_cached_meta(id).is_some_and(|cached| cache_is_fresh(cached.fetched, now))
    })
    .into_iter()
    .next()?;
    log::event(
        log::Level::Debug,
        "studio.metadata.asking",
        &[("batch", &batch.len()), ("installed", &ids.len())],
    );
    Some(Task::perform(
        async move { load_metadata(batch) },
        Message::MetaLoaded,
    ))
}

/// Ask Steam (keyless `GetPublishedFileDetails`) for one item.
fn fetch_meta(id: u64) -> Option<Meta> {
    let web = WorkshopWeb::new().ok()?;
    let details = web.fetch_published_file_details(&[id]).ok()?;
    Some(meta_from(details.into_iter().next()?))
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
        log::event(
            log::Level::Debug,
            "studio.metadata.cached",
            &[
                ("id", &id),
                ("age_seconds", &now.saturating_sub(cached.fetched)),
            ],
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
                log::failure("studio.metadata_cache.create_failed", &error);
            } else if let Err(error) = anima_engine_rs::paths::atomic_write_json(&path, &entry) {
                log::failure("studio.metadata_cache.write_failed", &error);
            }
            log::event(log::Level::Debug, "studio.metadata.fetched", &[("id", &id)]);
            Some(meta)
        }
        None => cached.map(|cached| {
            log::event(log::Level::Debug, "studio.metadata.stale", &[("id", &id)]);
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
        log::failure("studio.thumbnail_cache.create_failed", &error);
    } else if let Err(error) = std::fs::write(&cached, &bytes) {
        log::failure("studio.thumbnail_cache.write_failed", &error);
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
        log::event(
            log::Level::Debug,
            "studio.preview",
            &[
                ("id", &id),
                ("format", &format!("{format:?}")),
                ("width", &rgba.width()),
                ("height", &rgba.height()),
            ],
        );
        Ok((rgba.width(), rgba.height(), rgba.into_raw()))
    });
    Loaded { id, meta, thumb }
}

struct Studio {
    i18n: I18n,
    snapshot: WorkshopSnapshot,
    /// Per-item ACF state, refreshed with the snapshot. `ItemState::pending` is Steam saying it still owes
    /// this item a download, which is the only progress signal available from outside the client.
    states: std::collections::BTreeMap<u64, anima_engine_rs::workshop::ItemState>,
    /// Bytes on disk for the items Steam still owes, measured with the states: the only progress there is,
    /// since the `.acf` records a final size only once the download finishes.
    progress: std::collections::BTreeMap<u64, u64>,
    /// Ticks since each id last grew, for the ones that are growing. A download that lands between two
    /// ticks still gets a moment on screen, which a state read straight from Steam's metadata cannot give.
    downloading: std::collections::BTreeMap<u64, u8>,
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
    /// Which page of the search is showing, how many there are in total, and the cursor each page
    /// started from — Steam paginates by cursor, and going back needs the one we used.
    remote_page: u32,
    remote_total: u64,
    remote_cursors: Vec<Option<String>>,
    /// The cursor for the page after this one, as Steam last reported it.
    remote_next: Option<String>,
    /// The query the current results are for: editing the search box must not change what "next
    /// page" means.
    remote_query: Option<String>,
    remote_loading: bool,
    /// Bumped for every Steam request. A reply that belongs to an older one is dropped: leaving
    /// the results while a search is in flight used to let the answer arrive later and switch the
    /// list back to Steam, over whatever the user had moved on to.
    remote_request: u64,
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
    /// The clock the view reads: set on every message, so easing and the spinner never need to
    /// ask for the time themselves.
    now: Instant,
    /// Milliseconds of animation frames so far — the spinner's phase.
    spin_ms: u64,
    /// When the previous frame arrived, so `Frame` can add the elapsed time.
    last_frame: Instant,
    /// How far the item preview has come up, and the same for the asset preview. Two of them
    /// because the two panes swap independently.
    item_fade: Animation<f32>,
    asset_fade: Animation<f32>,
    /// The status line: how far its fade has come, and what it said when the fade started.
    note_fade: Animation<f32>,
    shown_seen: String,
    /// Whether motion is allowed at all (see [`motion_enabled`]).
    motion: bool,
    /// Which screen is showing.
    tab: Tab,
    /// The colour-key panel: a typed path, the frame it decoded (full size, untouched), the key and its
    /// band, and the two previews. The previews are rebuilt when a control changes rather than per frame:
    /// the pass itself is cheap for one scaled frame, but running it inside `view` would make it part of
    /// the render loop, which is a habit worth not starting.
    key_path: String,
    key_source: Option<(u32, u32, Vec<u8>)>,
    key_background: [u8; 3],
    key_tolerance: u8,
    key_feather: u8,
    key_before: Option<iced::widget::image::Handle>,
    key_after: Option<iced::widget::image::Handle>,
    /// What the last action said: a failed load, or how the write went.
    key_note: String,
    /// The write half of the panel. `key_command` starts from the chosen preset and is editable, which is
    /// the whole point: if a future ffmpeg renames `libwebp_anim`, the fix is a line in this field.
    key_format: String,
    key_command: String,
    key_fps: u8,
    key_quality: u8,
    key_lossless: bool,
    key_output: String,
    /// A render is running: the button goes quiet until it answers.
    key_writing: bool,
    /// The MP4 card. Its own fields rather than a shared abstraction, because the two panels differ in
    /// the interesting place: one reads a video, the other reads frames and writes a directory.
    video_path: String,
    video_command: String,
    video_output: String,
    video_fps: u8,
    video_quality: u8,
    video_lossless: bool,
    video_writing: bool,
    video_note: String,
    /// What ffprobe said about the chosen file, if it had anything to say.
    video_probe: Option<String>,
    /// The AI card: one video in, a folder of cut-out frames out.
    ai_path: String,
    ai_output: String,
    ai_running: bool,
    ai_note: String,
    /// Shared with the worker thread: the tick reads these to draw a bar, and to ask it to stop.
    ai_state: std::sync::Arc<AiRunning>,
    /// The settings tab's key field. Deliberately starts empty and is cleared after a save: the stored
    /// key is never rendered, only whether one is stored.
    key_draft: String,
    /// The scale entry, when a row is being typed into: which instance, and what has been typed.
    scale_draft: Option<(PathBuf, String)>,
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
            states: std::collections::BTreeMap::new(),
            progress: std::collections::BTreeMap::new(),
            downloading: std::collections::BTreeMap::new(),
            selected_item: None,
            query: String::new(),
            follow: true,
            theme: resolve_theme(),
            remote: None,
            remote_note: String::new(),
            remote_page: 1,
            remote_total: 0,
            remote_cursors: vec![None],
            remote_next: None,
            remote_query: None,
            remote_loading: false,
            remote_request: 0,
            note: String::new(),
            meta: HashMap::new(),
            thumbs: HashMap::new(),
            asset_previews: HashMap::new(),
            previewed_asset: None,
            staged: None,
            now: Instant::now(),
            spin_ms: 0,
            last_frame: Instant::now(),
            // Start settled: nothing fades in until something actually arrives.
            item_fade: Animation::new(1.0),
            asset_fade: Animation::new(1.0),
            note_fade: Animation::new(1.0),
            shown_seen: String::new(),
            motion: motion_enabled(),
            tab: Tab::default(),
            key_path: String::new(),
            key_source: None,
            // White, because that is what the original tool starts on. The corner button is the better
            // move and says so.
            key_background: [255, 255, 255],
            key_tolerance: 50,
            // Non-zero by default, unlike the original: a hard key is the thing that made its output look
            // cut out, so the default is the rounded one and zero is the option.
            key_feather: 12,
            key_before: None,
            key_after: None,
            key_note: String::new(),
            key_format: "gif".to_string(),
            key_command: anima_engine_rs::ffmpeg::PRESETS[0].1.to_string(),
            // 60 was the number the Python tools hard-coded in three places. It is a field now.
            key_fps: 30,
            key_quality: 90,
            key_lossless: true,
            key_output: String::new(),
            key_writing: false,
            video_path: String::new(),
            video_command: anima_engine_rs::ffmpeg::VIDEO_PRESETS[0].1.to_string(),
            video_output: String::new(),
            video_fps: 30,
            video_quality: 90,
            video_lossless: true,
            video_writing: false,
            video_note: String::new(),
            video_probe: None,
            ai_path: String::new(),
            ai_output: String::new(),
            ai_running: false,
            ai_note: String::new(),
            ai_state: AiRunning::new(),
            key_draft: String::new(),
            scale_draft: None,
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

    /// Re-read the client's `.acf` alone. This is the whole of "progress": one small file, and the only
    /// thing that changes while Steam works through a download queue.
    fn refresh_states(&mut self, roots: &[PathBuf]) {
        let library = anima_engine_rs::workshop::libraries_from_roots(roots)
            .into_iter()
            .find(|library| anima_engine_rs::workshop::acf_path(library).is_file());
        let Some(library) = library else {
            self.states.clear();
            self.progress.clear();
            return;
        };
        let acf = std::fs::read_to_string(anima_engine_rs::workshop::acf_path(&library));
        self.states = acf
            .ok()
            .and_then(|text| anima_engine_rs::workshop::installed_items(&text).ok())
            .unwrap_or_default();
        // Measured for everything the client knows about, and the growth is the signal — deliberately not
        // Steam's metadata, which is written when Steam feels like writing it. A directory that got bigger
        // since the last look is a download happening, whatever any `.acf` has got around to saying.
        let content = anima_engine_rs::workshop::content_root(&library);
        let previous = std::mem::take(&mut self.progress);
        self.progress = self
            .snapshot
            .items
            .keys()
            .chain(self.states.keys())
            .map(|id| (*id, anima_engine_rs::workshop::item_bytes(&content, *id)))
            .collect();
        for (id, bytes) in &self.progress {
            let grew = previous.get(id).is_some_and(|was| bytes > was);
            let quiet = self.downloading.entry(*id).or_insert(0);
            if grew {
                *quiet = 0;
            } else if *quiet < QUIET_TICKS {
                *quiet += 1;
            }
        }
        self.downloading.retain(|_, quiet| *quiet < QUIET_TICKS);
    }

    fn refresh(&mut self) {
        let mut watcher = WorkshopWatcher::discover();
        let _ = watcher.poll();
        self.snapshot = watcher.snapshot().clone();
        self.states = anima_engine_rs::workshop::libraries_from_roots(watcher.roots())
            .iter()
            .find_map(|library| {
                let text =
                    std::fs::read_to_string(anima_engine_rs::workshop::acf_path(library)).ok()?;
                anima_engine_rs::workshop::installed_items(&text).ok()
            })
            .unwrap_or_default();
        // Read what the cache already knows into memory. Without this step a warm cache is used only
        // to decide *not* to fetch, leaving `meta` empty and the rows showing file names — which is
        // exactly backwards, and exactly what happened.
        self.hydrate_meta(INSTALLED_META_LIMIT);
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
        log::event(log::Level::Info, "studio.refresh", &[("note", &self.note)]);
    }

    /// Put the cached answers for the installed list into `meta`, which is what the rows read. No
    /// network, and nothing that is already in memory is rewritten.
    fn hydrate_meta(&mut self, limit: usize) {
        let now = now_secs();
        let ids: Vec<u64> = self
            .snapshot
            .items
            .keys()
            .copied()
            .take(limit)
            .filter(|id| !self.meta.contains_key(id))
            .collect();
        let hydrated = hydrate_from(ids, now, read_cached_meta);
        if !hydrated.is_empty() {
            log::event(
                log::Level::Info,
                "studio.metadata.hydrated",
                &[("items", &hydrated.len())],
            );
        }
        self.meta.extend(hydrated);
    }

    fn item(&self, id: u64) -> Option<&WorkshopItem> {
        self.snapshot.items.get(&id)
    }

    /// Leave the Steam results: forget the page, the cursor history and the note, and make any
    /// reply still in flight stale. It deliberately does **not** touch whether the picker is open —
    /// "show the local list" and "open the list" are different questions, and conflating them is how
    /// the two buttons that both mean "installed" came to mean different things.
    fn leave_remote(&mut self) {
        self.remote = None;
        self.remote_note.clear();
        self.remote_query = None;
        self.remote_page = 1;
        self.remote_cursors = vec![None];
        self.remote_next = None;
        self.remote_total = 0;
        self.remote_loading = false;
        self.remote_request = self.remote_request.wrapping_add(1);
    }

    /// Load one page of the Workshop search. Page 1 starts at the beginning; any other page needs
    /// the cursor that identifies it, which is remembered in `remote_cursors` — or, for a page we
    /// have not been to, the cursor Steam gave us for the page after this one.
    fn goto_remote_page(&mut self, page: u32) -> Task<Message> {
        let page = page.max(1);
        let cursor = if page == 1 {
            None
        } else {
            self.remote_cursors
                .get(page as usize - 1)
                .cloned()
                .flatten()
                .or_else(|| self.remote_next.clone())
        };
        // Record what this page is asking for before asking, so coming back to it later is a
        // request with the same cursor rather than a guess.
        if self.remote_cursors.len() < page as usize {
            self.remote_cursors.resize(page as usize, None);
        }
        self.remote_cursors[page as usize - 1] = cursor.clone();

        // "No key" is a precondition, not a failure: catch it here, where the message can be in the
        // user's language, instead of letting the task fail and display a technical error.
        if WorkshopWeb::new()
            .map(|web| !web.has_api_key())
            .unwrap_or(false)
        {
            self.remote_loading = false;
            self.remote_note = fill(
                &self.i18n.tr(
                    "{} is not set and {} has no steam_api_key — browsing the Workshop needs a Steam \
                     Web API key ({}); installed items work without one",
                ),
                &[
                    anima_engine_rs::workshop_web::API_KEY_ENV.to_string(),
                    shorten(&anima_engine_rs::workshop_web::studio_settings_path()),
                    "https://steamcommunity.com/dev/apikey".to_string(),
                ],
            );
            return Task::none();
        }

        let query = self.remote_query.clone();
        self.remote_loading = true;
        // Every request carries the number it belongs to, so a reply to a search the user has left
        // can be recognised and dropped instead of landing on top of whatever is on screen now.
        self.remote_request = self.remote_request.wrapping_add(1);
        let request = self.remote_request;
        self.remote_note = format!("searching the Workshop… (page {page})");
        Task::perform(async move { fetch_remote(query, cursor) }, move |result| {
            Message::RemoteResults(request, page, result)
        })
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
                    log::event(
                        log::Level::Info,
                        "studio.overlay.following",
                        &[("overlay", &instance.label)],
                    );
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
        log::event(
            log::Level::Debug,
            "studio.overlay.refreshed",
            &[("instances", &self.overlay.len())],
        );
    }

    /// Write one control file, exactly as `anima-overlay-ctl` would: load, change a
    /// field (which bumps the generation), save. The overlay picks up the newest
    /// generation on its next poll.
    /// Write one instance's control file, for the row that asked. The running check stays: writing into
    /// a file nobody reads looks like "nothing happened", which is worse than saying so.
    fn apply_overlay_to(&mut self, control: PathBuf, action: OverlayAction) {
        let Some(instance) = self
            .overlay
            .iter()
            .find(|instance| instance.path == control)
        else {
            return;
        };
        if !instance.running {
            self.note = format!(
                "{} · {}",
                instance.label,
                self.i18n
                    .tr("not running — start it with Play, or anima-shepherd enable")
            );
            return;
        }
        let label = instance.label.clone();
        let next = apply_action(instance.state.clone(), action);
        self.note = match next.save(&control) {
            Ok(()) => format!(
                "{label}: {} {}",
                self.i18n.tr("generation"),
                next.generation
            ),
            Err(error) => format!(
                "{}: {error}",
                self.i18n.tr("could not write the control file")
            ),
        };
        self.reload_overlay();
    }

    /// Set one instance's scale to an exact percentage — what the slider sends.
    fn set_scale_on(&mut self, control: PathBuf, percent: u32) {
        let Some(instance) = self
            .overlay
            .iter()
            .find(|instance| instance.path == control)
        else {
            return;
        };
        if !instance.running {
            self.note = format!("{} · {}", instance.label, self.i18n.tr("not running"));
            return;
        }
        let label = instance.label.clone();
        let next = instance.state.clone().set_scale_percent(percent);
        self.note = match next.save(&control) {
            Ok(()) => format!("{label}: {}%", next.scale_percent),
            Err(error) => format!(
                "{}: {error}",
                self.i18n.tr("could not write the control file")
            ),
        };
        self.reload_overlay();
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        // One clock for the view: the spinner's phase and the easings read this instead of
        // asking for the time themselves, so a redraw cannot land between two different "nows".
        self.now = Instant::now();
        let task = self.handle(message);
        // The status line is written in nineteen places. Rather than route all of them through a
        // setter, the fade starts here — and here is the only place that matters: it has to be the
        // same message as the write. Starting it one message later would draw the new line at full
        // opacity once and then fade it, which reads as a flash.
        let shown = if self.remote_note.is_empty() {
            &self.note
        } else {
            &self.remote_note
        };
        if *shown != self.shown_seen {
            self.shown_seen = shown.clone();
            self.note_fade = fade_in(self.now, self.motion);
        }
        task
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Frame(at) => return self.settings_frame(at),
            Message::Tab(tab) => self.tab = tab,
            Message::ToggleFollow => return self.settings_toggle_follow(),
            Message::KeyDraft(draft) => self.key_draft = draft,
            Message::SaveKey => return self.settings_save_key(),
            Message::ToggleReduceMotion => return self.settings_toggle_reduce_motion(),
            Message::CopyDiagnostics => return self.settings_copy_diagnostics(),
            Message::Language(code) => return self.settings_language(code),
            Message::KeyPath(path) => self.key_path = path,
            Message::KeyPickFile => return self.key_pick_file(),
            Message::KeyLoad => self.key_load(),
            Message::KeyFormat(chosen) => return self.key_format(chosen),
            Message::KeyCommand(command) => self.key_command = command,
            Message::KeyFps(value) => self.key_fps = value,
            Message::KeyQuality(value) => self.key_quality = value,
            Message::KeyLossless(value) => self.key_lossless = value,
            Message::KeyOutput(path) => self.key_output = path,
            Message::KeyPickOutput => return self.key_pick_output(),
            Message::KeyWrite => return self.key_write(),
            Message::AiPath(path) => self.ai_path = path,
            Message::AiPickFile => return self.ai_pick_file(),
            Message::AiChosen(path) => return self.ai_chosen(path),
            Message::AiOutput(path) => self.ai_output = path,
            Message::AiPickOutput => return self.ai_pick_output(),
            Message::AiStart => return self.ai_start(),
            Message::AiCancel => return self.ai_cancel(),
            // Nothing to do: the message exists so the subscription repaints the bar.
            Message::AiTick => {}
            Message::AiDone(result) => return self.ai_done(result),
            Message::VideoPath(path) => self.video_path = path,
            Message::VideoPickFile => return self.video_pick_file(),
            Message::VideoChosen(path) => return self.video_chosen(path),
            Message::VideoFormat(chosen) => return self.video_format(chosen),
            Message::VideoCommand(command) => self.video_command = command,
            Message::VideoFps(value) => self.video_fps = value,
            Message::VideoQuality(value) => self.video_quality = value,
            Message::VideoLossless(value) => self.video_lossless = value,
            Message::VideoOutput(path) => self.video_output = path,
            Message::VideoPickOutput => return self.video_pick_output(),
            Message::VideoWrite => return self.video_write(),
            Message::VideoWrote(result) => return self.video_wrote(result),
            Message::KeyWrote(result) => return self.key_wrote(result),
            Message::KeyChosen(path) => return self.key_chosen(path),
            Message::KeyBackground(rgb) => return self.key_background(rgb),
            Message::KeyCorner => return self.key_corner(),
            Message::KeyTolerance(value) => return self.key_tolerance(value),
            Message::KeyFeather(value) => return self.key_feather(value),
            Message::Refresh => return self.workshop_refresh(),
            Message::Search(query) => return self.workshop_search(query),
            Message::ClearSearch => return self.workshop_clear_search(),
            Message::SearchSteam => return self.workshop_search_steam(),
            Message::RemotePage(page) => return self.goto_remote_page(page),
            Message::RemoteResults(request, page, Ok(loaded)) => {
                return self.workshop_remote_results(request, page, loaded);
            }
            Message::RemoteResults(request, _, Err(error)) => {
                return self.workshop_remote_results_2(request, error);
            }
            // Back to the installed list, from the Steam results.
            Message::ShowInstalled => self.leave_remote(),
            Message::Select(id) => return self.library_select(id),
            Message::Loaded(id, meta, thumb) => return self.library_loaded(id, meta, thumb),
            Message::CopyPath(path) => return self.workshop_copy_path(path),
            Message::OpenPage(id) => return self.workshop_open_page(id),
            Message::ArmRemove(id) => return self.workshop_arm_remove(id),
            Message::Remove(id) => return self.workshop_remove(id),
            Message::OverlayOn(control, action) => self.apply_overlay_to(control, action),
            Message::SetScale(control, percent) => self.set_scale_on(control, percent),
            Message::PollWorkshop => return self.workshop_poll_workshop(),
            Message::CancelRemove => return self.workshop_cancel_remove(),
            Message::ScaleDraft(control, text) => return self.settings_scale_draft(control, text),
            Message::CommitScale(control) => return self.settings_commit_scale(control),
            Message::MetaLoaded(loaded) => return self.workshop_meta_loaded(loaded),
            Message::SelectAsset(asset) => return self.select_asset(asset),
            Message::AssetPreview(asset, loaded) => {
                return self.settings_asset_preview(asset, loaded);
            }
            Message::StageOverlay(action) => self.stage_overlay(action),
            Message::PlayAsset(asset) => self.play_on_overlay(&asset),
            Message::PollOverlay => self.poll_overlay(),
            Message::ControlChanged => self.poll_overlay(),
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

        // Everything below builds the workshop tab. The other tabs return here, before any of the
        // two-pane body exists, so switching really does cost less per frame — the plan's promise
        // kept in the shape of the code rather than in a comment.
        if self.tab != Tab::Workshop {
            let body: El<'_> = match self.tab {
                Tab::Overlays => self.tab_overlays(),
                Tab::Tools => self.tab_tools(),
                Tab::Settings => self.tab_settings(),
                _ => self.tab_diagnostics(),
            };
            return self.frame(header, body);
        }

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
                // The pane reads: what it looks like → what it is → what you can do → what it
                // contains. A raw path is an unbreakable token that wrecks the wrapping, so it is
                // not laid out at all: the cover's hover tip shows one and "Copy path" copies it,
                // which leaves the pane opening on the picture.
                let mut pane = column![].spacing(10);

                match self.thumbs.get(&id) {
                    Some(Thumb::Ready(handle)) => {
                        pane = pane.push(tooltip(
                            container(
                                image(handle.clone())
                                    .content_fit(iced::ContentFit::Contain)
                                    .opacity(self.fade(&self.item_fade)),
                            )
                            .width(Length::Fill)
                            .height(Length::Fixed(PREVIEW_HEIGHT)),
                            path_tip(&item.path),
                            tooltip::Position::Bottom,
                        ));
                    }
                    Some(Thumb::Loading) => {
                        pane = pane.push(placeholder(
                            self.spinner(),
                            "…".to_string(),
                            Length::Fixed(PREVIEW_HEIGHT),
                        ));
                    }
                    Some(Thumb::Failed(error)) => {
                        pane = pane.push(placeholder(
                            "⚠",
                            format!("{} · {error}", self.i18n.tr("No preview")),
                            Length::Fixed(PREVIEW_HEIGHT),
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
                // The path moved off its own row and in here, next to the rest of what can be
                // done with the item.
                actions = actions.push(
                    button(text(self.i18n.tr("Copy path")))
                        .style(secondary)
                        .on_press(Message::CopyPath(item.path.display().to_string())),
                );
                let subscribed = self.snapshot.items.contains_key(&id);
                let pending = self.states.get(&id).is_some_and(|state| state.pending());
                // Before subscribing there is nothing to download and nothing to press — the subscribe
                // button is the whole story — and once the client has the item, Steam's copy *is* the
                // copy, so the cell reports instead of offering a second one.
                if subscribed {
                    let growing = self.downloading.contains_key(&id);
                    let bytes = self.progress.get(&id).copied().unwrap_or(0);
                    actions = actions.push(download_state(self, pending || growing, bytes));
                }
                // One button whose word says which way the toggle goes. The state is the Steam client's
                // own list, so it cannot drift from what Steam thinks — and the tooltip says where the
                // toggle actually lives, because we do not own it.
                actions = actions.push(tooltip(
                    toggle(
                        self,
                        subscribed,
                        "Unsubscribe",
                        "Subscribe",
                        Message::OpenPage(id),
                    ),
                    text(
                        self.i18n
                            .tr("opens the item's page in Steam; the toggle lives there"),
                    ),
                    tooltip::Position::Top,
                ));
                // The button does not change its meaning any more: it opens the warning, and the warning
                // is where the yes and the no live.
                actions = actions.push(
                    button(text(self.i18n.tr("Remove local copy")))
                        .style(danger)
                        .on_press(Message::ArmRemove(id)),
                );
                pane = pane.push(actions.spacing(6).wrap());

                pane = pane.push(divider(Length::Fill, Length::Fixed(1.0)));
                pane = pane.push(
                    text(format!("{} {}", item.assets.len(), self.i18n.tr("assets"))).size(12),
                );
                // A row is a choice, not an action: clicking it shows that file's first frame
                // in the tuning area below, where the play button and the parameters live.
                for asset in &item.assets {
                    if is_playable(asset) {
                        let selected = self.previewed_asset.as_ref() == Some(asset);
                        pane = pane.push(
                            button(text(file_name(asset)))
                                .style(if selected { selected_fill } else { text_button })
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
                            container(
                                image(handle.clone())
                                    .content_fit(iced::ContentFit::Contain)
                                    .opacity(self.fade(&self.item_fade)),
                            )
                            .width(Length::Fill)
                            .height(Length::Fixed(PREVIEW_HEIGHT)),
                        );
                    }
                    Some(Thumb::Loading) => {
                        pane = pane.push(placeholder(
                            self.spinner(),
                            "…".to_string(),
                            Length::Fixed(PREVIEW_HEIGHT),
                        ));
                    }
                    Some(Thumb::Failed(error)) => {
                        pane = pane.push(placeholder(
                            "⚠",
                            format!("{} · {error}", self.i18n.tr("No preview")),
                            Length::Fixed(PREVIEW_HEIGHT),
                        ));
                    }
                    None => {}
                }

                pane = pane.push(divider(Length::Fill, Length::Fixed(1.0)));
                let subscribed = self.snapshot.items.contains_key(&id);
                let pending = self.states.get(&id).is_some_and(|state| state.pending());
                pane = pane.push(
                    row![
                        // Present only once the client has it: there is no download to offer before
                        // that, and afterwards the client's copy is the copy.
                        if subscribed {
                            download_state(
                                self,
                                pending || self.downloading.contains_key(&id),
                                self.progress.get(&id).copied().unwrap_or(0),
                            )
                        } else {
                            space().into()
                        },
                        tooltip(
                            toggle(
                                self,
                                self.snapshot.items.contains_key(&id),
                                "Unsubscribe",
                                "Subscribe",
                                Message::OpenPage(id),
                            ),
                            text(
                                self.i18n
                                    .tr("opens the item's page in Steam; the toggle lives there",)
                            ),
                            tooltip::Position::Top,
                        ),
                    ]
                    .spacing(8),
                );
                pane = pane.push(text(format!("({})", self.i18n.tr("not installed"))).size(11));
                scrollable(pane)
                    .style(iced_material::scrollable::default)
                    .into()
            }
            // Nothing selected: an empty pane. The note belongs to the status bar below, and
            // rendering it here as well made the same line appear twice.
            (None, None) => placeholder(
                "▤",
                self.i18n.tr("Pick an item to see its details").to_string(),
                Length::Fill,
            )
            .into(),
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

        let left = column![search, self.result_panel()]
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

        self.frame(header, bodies)
    }

    /// The chrome every tab shares: the header, the tab strip, the body, the status line, and the
    /// picker on top when it is open. One place, so the tabs cannot drift apart in their frame.
    fn frame<'a>(&'a self, header: impl Into<El<'a>>, body: impl Into<El<'a>>) -> El<'a> {
        // The status line fades in when it says something new: the text colour carries the alpha
        // because iced has no way to fade a container, and this is a one-line widget.
        let status_alpha = self.fade(&self.note_fade);
        let scene = container(
            column![
                header.into(),
                self.tab_strip(),
                body.into(),
                text(if self.remote_note.is_empty() {
                    self.note.clone()
                } else {
                    self.remote_note.clone()
                })
                .size(11)
                .style(
                    move |theme: &iced_material::Theme| iced::widget::text::Style {
                        color: Some(iced::Color {
                            a: status_alpha,
                            ..theme.colors().surface.text_variant
                        }),
                    }
                ),
            ]
            .spacing(12),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill);

        let Some(dialog) = self.remove_dialog() else {
            return scene.into();
        };
        stack![
            scene,
            // The scrim is the "no": a press anywhere outside the card cancels, which is the way back the
            // inline confirm button never had.
            button(space())
                .style(|_theme: &iced_material::Theme, _status| {
                    iced::widget::button::Style {
                        background: Some(
                            iced::Color {
                                a: 0.55,
                                ..iced::Color::BLACK
                            }
                            .into(),
                        ),
                        ..Default::default()
                    }
                })
                .width(Length::Fill)
                .height(Length::Fill)
                .on_press(Message::CancelRemove),
            container(dialog)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(iced::Alignment::Center)
                .align_y(iced::Alignment::Center),
        ]
        .into()
    }

    /// A warning that takes the window, the way UAC does: the action deletes a directory, so it gets a yes
    /// and a no side by side instead of a button that changes its own meaning and offers no way back.
    fn remove_dialog(&self) -> Option<El<'_>> {
        let id = self.pending_remove?;
        let title = self
            .meta
            .get(&id)
            .map(|meta| meta.title.clone())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| id.to_string());
        Some(
            container(
                column![
                    text(self.i18n.tr("Remove local copy")).size(18),
                    text(format!("{title} · {id}")).size(12),
                    text(self.i18n.tr("This deletes the copy under the SteamCMD root. It does not unsubscribe in Steam, and your own Steam library is never touched.")).size(11),
                    row![
                        button(text(self.i18n.tr("Remove local copy")))
                            .style(danger)
                            .on_press(Message::Remove(id)),
                        button(text(self.i18n.tr("Cancel")))
                            .style(secondary)
                            .on_press(Message::CancelRemove),
                    ]
                    .spacing(8),
                ]
                .spacing(10),
            )
            .style(iced_material::container::surface_container_high)
            .padding(16)
            .width(Length::Fixed(430.0))
            .into(),
        )
    }

    /// The tab strip, on its own row under the header rather than inside it: the header already
    /// carries the title, the language picker, refresh and quit, and four more labels would crowd it
    /// at the minimum window size. This is also the shape Steam uses, which is where the idea came
    /// from.
    fn tab_strip(&self) -> El<'_> {
        let tabs = [
            (Tab::Workshop, "Workshop"),
            (Tab::Overlays, "Overlay"),
            (Tab::Tools, "Tools"),
            (Tab::Settings, "Settings"),
            (Tab::Diagnostics, "Diagnostics"),
        ];
        let mut strip = row![].spacing(6);
        for (tab, key) in tabs {
            let tab_button = button(text(self.i18n.tr(key)))
                .style(if self.tab == tab {
                    selected_fill
                } else {
                    text_button
                })
                .padding(6);
            // The active tab has no press handler: pressing it would do nothing anyway, and this is
            // the same "no affordance means disabled" idiom the pager uses.
            strip = strip.push(if self.tab == tab {
                tab_button
            } else {
                tab_button.on_press(Message::Tab(tab))
            });
        }
        strip.into()
    }

    /// The Steam key and the motion switch, writing `studio.json` immediately. The stored key is never
    /// rendered — only whether one is stored — so a screenshot of this tab cannot leak it.
    fn tab_settings(&self) -> El<'_> {
        use anima_engine_rs::settings::{ConfigFile, StudioSettings};

        let stored = StudioSettings::load(&anima_engine_rs::workshop_web::studio_settings_path());
        let key_state = if stored.steam_api_key.trim().is_empty() {
            self.i18n.tr("not set")
        } else {
            self.i18n.tr("stored")
        };
        column![
            text(self.i18n.tr("Settings")).size(20),
            text(format!("{} · {}", self.i18n.tr("Steam API key"), key_state)).size(12),
            text_input(
                &self.i18n.tr("paste the key and press Enter"),
                &self.key_draft
            )
            .on_input(Message::KeyDraft)
            .on_submit(Message::SaveKey)
            .style(iced_material::text_input::default)
            .width(Length::Fill),
            row![
                button(text(self.i18n.tr("Save")))
                    .style(primary)
                    .on_press(Message::SaveKey),
                text(self.i18n.tr("get one at steamcommunity.com/dev/apikey")).size(11),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            divider(Length::Fill, Length::Fixed(1.0)),
            row![
                text(self.i18n.tr("Reduce motion")).size(14),
                toggle(
                    self,
                    self.motion,
                    &self.i18n.tr("turn motion off"),
                    &self.i18n.tr("turn motion on"),
                    Message::ToggleReduceMotion,
                ),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            text(
                self.i18n
                    .tr("Stored in studio.json; ANIMA_NO_MOTION overrides it.")
            )
            .size(11),
            divider(Length::Fill, Length::Fixed(1.0)),
            text(format!(
                "Palette: {}",
                palette_path()
                    .map(|path| shorten(&path))
                    .unwrap_or_else(|| "-".to_string())
            ))
            .size(11),
            text(
                self.i18n
                    .tr("A wallpaper change needs this window restarted.")
            )
            .size(11),
        ]
        .spacing(10)
        .into()
    }

    /// What the status line knows, laid out so it can be pasted into a bug report.
    /// Decode the typed path's first frame, at preview size, and key it once.
    fn key_load(&mut self) {
        let path = std::path::PathBuf::from(self.key_path.trim());
        match anima_engine_rs::anim::first_frame(&path) {
            Ok((width, height, rgba)) => {
                let shrunk = shrink_preview(width, height, rgba);
                self.key_source = Some(shrunk);
                self.key_note = String::new();
                self.key_redraw();
            }
            Err(error) => {
                self.key_source = None;
                self.key_before = None;
                self.key_after = None;
                self.key_note =
                    format!("{}: {error}", self.i18n.tr("could not read a first frame"));
            }
        }
    }

    fn tab_diagnostics(&self) -> El<'_> {
        let mut lines = column![text(self.i18n.tr("Diagnostics")).size(20)].spacing(6);
        for line in self.diagnostics_lines() {
            lines = lines.push(text(line).size(12));
        }
        lines = lines.push(
            button(text(self.i18n.tr("Copy all")))
                .style(secondary)
                .on_press(Message::CopyDiagnostics),
        );
        scrollable(lines.spacing(10))
            .style(iced_material::scrollable::default)
            .into()
    }

    /// One `key: value` per line — plain text on purpose, for pasting into an issue.
    fn diagnostics_lines(&self) -> Vec<String> {
        let watcher = WorkshopWatcher::discover();
        let roots = watcher.roots();
        let catalog = self
            .i18n
            .catalog_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".to_string());
        vec![
            format!("version:    {}", env!("CARGO_PKG_VERSION")),
            format!(
                "roots:      {} · {}",
                roots.len(),
                roots
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            format!("installed:  {} item(s)", self.snapshot.len()),
            format!("catalog:    {catalog} · {} entries", self.i18n.entries()),
            format!(
                "shepherd:   {}",
                if anima_engine_rs::shepherd::is_up() {
                    "running"
                } else {
                    "not running"
                }
            ),
            // The name is spelled out rather than poked out of `dbus_intent`, which only exists when
            // this crate is built with the `dbus` feature — the shepherd is what serves it.
            "dbus:       dev.AnimaEngine.Shepherd (served by the shepherd)".to_string(),
            format!("thumbnails: {}", thumb_dir().display()),
            format!(
                "palette:    {}",
                palette_path()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string())
            ),
        ]
    }

    /// The list, where it is read: installed items, or the Steam results, in the left column of the
    /// workshop tab. It used to float over the window in a popup; the results belong where the search
    /// is, not behind an extra layer to dismiss.
    fn result_panel(&self) -> El<'_> {
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
                self.i18n.tr("Installed"),
                self.snapshot.len()
            )
        };
        let mut header = row![text(title).size(14), space().width(Length::Fill)]
            .spacing(8)
            .align_y(iced::Alignment::Center);
        if self.remote.is_some() {
            let pages = page_count(self.remote_total, REMOTE_PER_PAGE);
            let page = if self.remote_loading {
                format!("{}/{} …", self.remote_page, pages)
            } else {
                format!("{}/{}", self.remote_page, pages)
            };
            header = header.push(text(page).size(11));
            // No press handler is iced's disabled state, which is how the pager says "no more".
            let back = button(text("‹")).style(secondary);
            header = header.push(if self.remote_page > 1 {
                back.on_press(Message::RemotePage(self.remote_page - 1))
            } else {
                back
            });
            let forward = button(text("›")).style(secondary);
            header = header.push(if self.remote_next.is_some() {
                forward.on_press(Message::RemotePage(self.remote_page + 1))
            } else {
                forward
            });
            header = header.push(
                button(text(self.i18n.tr("Installed")))
                    .style(secondary)
                    .on_press(Message::ShowInstalled),
            );
        }
        column![
            header,
            divider(Length::Fill, Length::Fixed(1.0)),
            scrollable(self.result_rows(&installed))
                .style(iced_material::scrollable::default)
                .height(Length::Fill),
        ]
        .spacing(8)
        .into()
    }

    /// Every instance as a row with its own controls. The narrow panel could only drive the selected
    /// one; that was the reason to move this out of the workshop tab at all.
    fn tab_overlays(&self) -> El<'_> {
        let heading = row![
            text(self.i18n.tr("Overlay")).size(20),
            space().width(Length::Fill),
            // Following is a property of the whole list, not of one instance, so it sits in the heading.
            button(text(format!(
                "{} {}",
                self.i18n.tr("follow"),
                if self.follow { "●" } else { "○" }
            )))
            .style(if self.follow { primary } else { secondary })
            .on_press(Message::ToggleFollow),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        let mut rows = column![heading].spacing(10);
        if self.overlay.is_empty() {
            return rows
                .push(text(self.i18n.tr("no control file yet — start an overlay first")).size(11))
                .into();
        }
        // Running first: those are the rows with something to change, and the rest are read-only.
        let mut instances: Vec<&OverlayInstance> = self.overlay.iter().collect();
        instances.sort_by_key(|instance| !instance.running);
        for instance in instances {
            rows = rows.push(self.instance_row(instance));
        }
        scrollable(rows)
            .style(iced_material::scrollable::default)
            .into()
    }

    /// One instance: what it is, what state it is in, and its own parameters. The scale gets a slider
    /// with 1% steps — the ±10% buttons stay beside it for coarse jumps.
    fn instance_row(&self, instance: &OverlayInstance) -> El<'_> {
        use anima_engine_rs::control::{MAX_SCALE_PERCENT, MIN_SCALE_PERCENT};

        let control = instance.path.clone();
        let state = &instance.state;
        let mode = match state.input_mode {
            InputMode::Full => "full",
            InputMode::Opaque => "opaque",
            InputMode::PassThrough => "pass-through",
        };
        let summary = format!(
            "{} {}% · {}° · {mode}",
            if instance.running { "●" } else { "○" },
            state.scale_percent,
            state.rotation
        );
        let mut details = column![
            row![
                text(instance_label(instance, true)).size(14),
                space().width(Length::Fill),
                text(summary).size(11),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
        ]
        .spacing(6);

        if instance.running {
            // The slider is the fine control; the entry beside it is for typing an exact number. The
            // coarse ±10% buttons are not repeated here — they are gone, since the slider and the entry
            // cover both jobs.
            let draft = self
                .scale_draft
                .as_ref()
                .filter(|(path, _)| path == &control)
                .map(|(_, text)| text.clone())
                .unwrap_or_else(|| state.scale_percent.to_string());
            details = details.push(
                row![
                    slider(
                        MIN_SCALE_PERCENT..=MAX_SCALE_PERCENT,
                        state.scale_percent,
                        {
                            let control = control.clone();
                            move |percent| Message::SetScale(control.clone(), percent)
                        },
                    )
                    .step(1u32)
                    .width(Length::Fill),
                    text_input("100", &draft)
                        .on_input({
                            let control = control.clone();
                            move |text| Message::ScaleDraft(control.clone(), text)
                        })
                        .on_submit(Message::CommitScale(control.clone()))
                        .style(iced_material::text_input::default)
                        .width(Length::Fixed(56.0)),
                    text("%").size(11),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            );
            // One row, with Quit on the end of it: the row is wide, the buttons are short, and a button
            // per line was four lines of height spent on empty space.
            details = details.push(
                row![
                    self.overlay_params(
                        Some(state),
                        |action| Message::OverlayOn(control.clone(), action),
                        false,
                    ),
                    button(text(self.i18n.tr("Quit overlay")))
                        .style(danger)
                        .on_press(Message::OverlayOn(control.clone(), OverlayAction::Quit)),
                ]
                .spacing(6)
                .wrap(),
            );
        }
        container(details)
            .style(iced_material::container::surface_container_low)
            .padding(10)
            .width(Length::Fill)
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
            let selected = self.selected_item == Some(*id);
            list = list.push(
                button(
                    column![
                        text(name).size(14).style(if selected {
                            iced_material::text::secondary_container
                        } else {
                            iced_material::text::surface
                        }),
                        text(format!(
                            "{id} · {} {}",
                            item.assets.len(),
                            self.i18n.tr("assets")
                        ))
                        .size(11),
                    ]
                    .spacing(2),
                )
                .style(if selected { selected_row } else { secondary })
                .on_press(Message::Select(*id))
                .width(Length::Fill),
            );
        }
        if let Some(rows) = &self.remote {
            for row in rows {
                let selected = self.selected_item == Some(row.id);
                list = list.push(
                    button(
                        column![
                            text(format!("☁ {}", row.title))
                                .size(14)
                                .style(if selected {
                                    iced_material::text::secondary_container
                                } else {
                                    iced_material::text::surface
                                }),
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
            self.note = format!("{} · {}", file_name(&asset), self.i18n.tr("not running"));
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
            Err(error) => format!(
                "{}: {error}",
                self.i18n.tr("could not stage the parameters")
            ),
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
        tag: impl Fn(OverlayAction) -> Message,
        scale: bool,
    ) -> El<'_> {
        let hidden = state.is_some_and(|state| state.hidden);
        let locked = state.is_some_and(|state| state.locked);
        // One wrapping row rather than three: the panel is wide and the buttons are short, so stacking
        // them cost four lines of height for empty space. `.wrap()` still moves them down when narrow.
        let mut buttons = row![].spacing(6);
        buttons = buttons
            .push(toggle(
                self,
                hidden,
                "Show",
                "Hide",
                tag(OverlayAction::ToggleHidden),
            ))
            .push(toggle(
                self,
                locked,
                "Unlock",
                "Lock",
                tag(OverlayAction::ToggleLocked),
            ))
            .push(
                button(text(self.i18n.tr("Click-through")))
                    .style(secondary)
                    .on_press(tag(OverlayAction::ToggleClickThrough)),
            );
        // The scale buttons live beside a slider in the overlays tab, so they are left out there and
        // kept here, where they are the only scale control.
        if scale {
            buttons = buttons
                .push(
                    button(text("−10%"))
                        .style(secondary)
                        .on_press(tag(OverlayAction::ScaleDown)),
                )
                .push(
                    button(text("+10%"))
                        .style(secondary)
                        .on_press(tag(OverlayAction::ScaleUp)),
                );
        }
        buttons = buttons
            .push(
                button(text("⟳ 90°"))
                    .style(secondary)
                    .on_press(tag(OverlayAction::Rotate)),
            )
            .push(
                button(text(self.i18n.tr("Flip H")))
                    .style(secondary)
                    .on_press(tag(OverlayAction::FlipH)),
            )
            .push(
                button(text(self.i18n.tr("Flip V")))
                    .style(secondary)
                    .on_press(tag(OverlayAction::FlipV)),
            )
            .push(
                button(text(self.i18n.tr("Reset")))
                    .style(secondary)
                    .on_press(tag(OverlayAction::Reset)),
            );
        buttons.wrap().into()
    }

    /// The tuning area: the asset that was clicked, its first frame, and the parameters it
    /// will start with. Empty until an asset is picked, so nothing here acts on a guess.
    fn asset_preview_area(&self) -> El<'_> {
        let Some(asset) = self.previewed_asset.as_ref() else {
            return placeholder(
                "▦",
                self.i18n.tr("Click an asset to preview it").to_string(),
                Length::Fixed(PREVIEW_HEIGHT),
            )
            .into();
        };
        let mut area = column![text(file_name(asset)).size(14)].spacing(8);
        match self.asset_previews.get(asset) {
            Some(Thumb::Ready(handle)) => {
                area = area.push(tooltip(
                    container(
                        image(handle.clone())
                            .content_fit(iced::ContentFit::Contain)
                            .opacity(self.fade(&self.asset_fade)),
                    )
                    .width(Length::Fill)
                    .height(Length::Fixed(PREVIEW_HEIGHT)),
                    path_tip(asset),
                    tooltip::Position::Bottom,
                ));
            }
            Some(Thumb::Loading) => {
                area = area.push(placeholder(
                    self.spinner(),
                    "…".to_string(),
                    Length::Fixed(PREVIEW_HEIGHT),
                ));
            }
            Some(Thumb::Failed(error)) => {
                area = area.push(placeholder(
                    "⚠",
                    format!("{} · {error}", self.i18n.tr("No preview")),
                    Length::Fixed(PREVIEW_HEIGHT),
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
                // No slider here, so this is the only scale control on the tuning area.
                true,
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
                Err(error) => format!(
                    "{}: {error}",
                    self.i18n.tr("could not write the control file")
                ),
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
                    Ok(()) => format!(
                        "{} {}",
                        self.i18n.tr("handed to the shepherd"),
                        file_name(asset)
                    ),
                    Err(error) => format!(
                        "{}: {error}",
                        self.i18n.tr("could not update the shepherd registry")
                    ),
                }
            } else {
                format!("{} is already wanted by the shepherd", file_name(asset))
            };
            return;
        }
        let Some(binary) = find_overlay_binary() else {
            self.note = self
                .i18n
                .tr("anima-overlay not found — build or install it first (--features overlay)");
            return;
        };
        self.note = match std::process::Command::new(&binary).arg(asset).spawn() {
            Ok(child) => {
                // Keep the handle so `poll_overlay` can wait for it: a killed overlay that
                // nobody reaps stays a zombie in `/proc`, where it still looks alive.
                self.children.push(child);
                format!(
                    "{} {}",
                    self.i18n.tr("started an overlay for"),
                    file_name(asset)
                )
            }
            Err(error) => format!(
                "{} {}: {error}",
                self.i18n.tr("could not start"),
                binary.display()
            ),
        };
    }

    /// Whether anything is moving: a spinner needs frames, and so does an unfinished fade.
    /// Nothing else does, so an idle window asks for no frames at all — and with motion off,
    /// nothing ever does.
    fn animating(&self) -> bool {
        if !self.motion {
            return false;
        }
        self.item_fade.is_animating(self.now)
            || self.asset_fade.is_animating(self.now)
            || self.note_fade.is_animating(self.now)
            || self
                .thumbs
                .values()
                .any(|thumb| matches!(thumb, Thumb::Loading))
            || self
                .asset_previews
                .values()
                .any(|thumb| matches!(thumb, Thumb::Loading))
    }

    /// How opaque a fade is right now, clamped to [0, 1]: an overshooting curve is right for a
    /// position and wrong for an opacity.
    fn fade(&self, animation: &Animation<f32>) -> f32 {
        self.eased(animation).clamp(0.0, 1.0)
    }

    /// A curve's value right now, unclamped so an overshoot can be seen (and used as travel).
    fn eased(&self, animation: &Animation<f32>) -> f32 {
        animation.interpolate_with(|value| value, self.now)
    }

    /// The spinner's current glyph: one frame of the cycle, or a still one when motion is off.
    fn spinner(&self) -> &'static str {
        if self.motion {
            spinner_frame(self.spin_ms)
        } else {
            "▦"
        }
    }

    /// The panel follows the control files on its own; the Refresh buttons are for
    /// impatience rather than correctness.
    fn subscription(&self) -> Subscription<Message> {
        // Keyboard shortcuts are not here yet: iced 0.14 removed `keyboard::on_key_press`, so
        // they have to go through the event-listening API. Noted in docs/next-steps.md.
        let mut subscriptions = vec![
            iced::time::every(std::time::Duration::from_millis(OVERLAY_POLL_MS))
                .map(|_| Message::PollOverlay),
            // The fast path: an event from the watcher, which is what makes a change look immediate.
            control_events(),
            // Steam downloads on its own schedule and tells nobody, so the row has to notice by itself.
            // Outside the `animating` branch on purpose: a download is not an animation.
            iced::time::every(std::time::Duration::from_millis(WORKSHOP_POLL_MS))
                .map(|_| Message::PollWorkshop),
            // The floor. Everything above can fail — an event the watcher never got, an `.acf` Steam has
            // not written yet — and this one still puts the window back in sync two seconds later.
            iced::time::every(std::time::Duration::from_millis(WORKSHOP_FALLBACK_MS))
                .map(|_| Message::Refresh),
        ];
        if self.animating() {
            subscriptions.push(
                iced::time::every(std::time::Duration::from_millis(FRAME_MS)).map(Message::Frame),
            );
        }
        // The AI batch writes its progress into atomics, so a repaint is all the bar needs. Only while it
        // runs: an empty tick list is cheaper than an idle timer.
        if self.ai_running {
            subscriptions.push(
                iced::time::every(std::time::Duration::from_millis(400)).map(|_| Message::AiTick),
            );
        }
        Subscription::batch(subscriptions)
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
    iced::application(
        || {
            let studio = Studio::boot();
            // Titles for the installed list, in one keyless batch, before anyone has to click an
            // item to make its name appear. Boot returns the task rather than spawning it here, so
            // it runs on the runtime that owns the tasks.
            let task = installed_meta_task(&studio).unwrap_or_else(Task::none);
            (studio, task)
        },
        Studio::update,
        Studio::view,
    )
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

    #[test]
    fn a_warm_cache_reaches_the_rows_without_a_request() {
        let cached = |id: u64, fetched: u64| CachedMeta {
            fetched,
            details: Meta {
                title: format!("title {id}"),
                ..Meta::default()
            },
        };
        let now = 1_000_000;
        let read = |id: u64| match id {
            1 => Some(cached(1, now - 10)),
            // Stale: its title must not be shown as if it were current.
            2 => Some(cached(2, now - META_TTL_SECS - 1)),
            _ => None,
        };

        let hydrated = hydrate_from([1_u64, 2, 3], now, read);
        assert_eq!(hydrated.len(), 1);
        assert_eq!(hydrated[&1].title, "title 1");
        assert!(!hydrated.contains_key(&2));
        assert!(!hydrated.contains_key(&3));
    }

    #[test]
    fn only_stale_metadata_is_requested() {
        let ids = [1, 2, 3, 4];
        // Everything fresh: nothing to ask for, which is the offline second run.
        assert!(meta_batches(&ids, |_| true).is_empty());
        // Nothing fresh: one batch, in order.
        assert_eq!(meta_batches(&ids, |_| false), vec![vec![1, 2, 3, 4]]);
        // Mixed: only what is missing.
        assert_eq!(meta_batches(&ids, |id| id == 2), vec![vec![1, 3, 4]]);
        // The batches respect the endpoint's cap.
        let many: Vec<u64> = (0..250).collect();
        let batches = meta_batches(&many, |_| false);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 100);
        assert_eq!(batches[2].len(), 50);
    }

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
        // The key can come from the environment or from `studio.json`, so ask the same question the
        // code asks rather than checking the environment alone.
        if WorkshopWeb::new()
            .map(|web| web.has_api_key())
            .unwrap_or(false)
        {
            return; // this machine has a key; nothing to assert
        }
        let error = fetch_remote(None, None).expect_err("no key means an error, not an empty list");
        assert!(error.contains("STEAM_WEB_API_KEY"), "{error}");
        // This is the technical fallback; the window's own message (in `goto_remote_page`) is the one
        // that names the settings file and the URL, in the user's language.
        assert!(error.contains("studio.json"), "{error}");
        eprintln!("[studio] no key: {error}");
    }

    #[test]
    fn pages_never_say_zero() {
        assert_eq!(page_count(0, 50), 1);
        assert_eq!(page_count(1, 50), 1);
        assert_eq!(page_count(50, 50), 1);
        assert_eq!(page_count(51, 50), 2);
        assert_eq!(page_count(1823, 50), 37);
        // A zero page size must not divide by zero, and a huge total must not overflow to a wrap.
        assert_eq!(page_count(10, 0), 10);
        let _ = page_count(u64::MAX, 1);
    }

    /// The real thing, end to end: search the Workshop, then walk to the second page with the
    /// cursor the first page handed over. Ignored by default — it needs a key and the network.
    #[test]
    #[ignore = "network: Steam Web API, needs STEAM_WEB_API_KEY"]
    fn searches_the_workshop_and_pages_with_the_cursor() {
        let query = Some("teto".to_string());
        let first = match fetch_remote(query.clone(), None) {
            Ok(page) => page,
            Err(error) => {
                eprintln!("[test] skipped: {error}");
                return;
            }
        };
        eprintln!(
            "[test] page 1: {} row(s), total {}, more: {}",
            first.rows.len(),
            first.total,
            first.next_cursor.is_some()
        );
        assert!(!first.rows.is_empty(), "a search for teto found nothing");
        assert!(
            first.total as usize >= first.rows.len(),
            "the total is smaller than the page it describes"
        );
        // The pager's arithmetic is what the window shows, so it has to be sane for this total.
        assert!(page_count(first.total, REMOTE_PER_PAGE) >= 1);

        let Some(cursor) = first.next_cursor.clone() else {
            eprintln!("[test] only one page of results; nothing to walk");
            return;
        };
        let second = fetch_remote(query, Some(cursor)).expect("page 2");
        eprintln!(
            "[test] page 2: {} row(s), total {}",
            second.rows.len(),
            second.total
        );
        // What a cursor actually guarantees: the window moved on. It does *not* guarantee the pages
        // tile the result set — Steam's list is live and ranked, so it can shift between two
        // requests and let a page repeat or skip an item. `50 + 20 = 70` looked like a property
        // until the ranking moved and page 2 came back with 26 rows instead of 20.
        assert!(!second.rows.is_empty(), "page 2 was empty");
        assert!(
            second.rows != first.rows,
            "page 2 is page 1 again — the cursor was ignored"
        );
        assert!(second.total >= second.rows.len() as u64);
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
    fn the_installed_path_always_shows_installed() {
        let mut studio = Studio::boot();
        // Pretend a Steam search is on screen.
        studio.remote = Some(vec![RemoteRow {
            id: 1,
            title: "from steam".to_string(),
            subscriptions: 0,
        }]);
        studio.remote_query = Some("teto".to_string());
        studio.remote_page = 3;
        studio.remote_total = 70;
        studio.remote_next = Some("cursor".to_string());

        // Back to the installed list, from the Steam results. This used to mean "whatever was on
        // screen last", which with Steam results up was Steam results.
        let _ = studio.update(Message::ShowInstalled);
        assert!(
            studio.remote.is_none(),
            "the installed path kept showing Steam results"
        );
        assert_eq!(studio.remote_page, 1);
        assert!(studio.remote_next.is_none());
        assert!(studio.remote_query.is_none());
    }

    #[test]
    fn a_reply_from_a_search_we_left_is_dropped() {
        let mut studio = Studio::boot();
        studio.remote_request = 7;
        let stale = RemotePage {
            total: 70,
            next_cursor: None,
            rows: vec![RemoteRow {
                id: 2,
                title: "stale".to_string(),
                subscriptions: 0,
            }],
        };

        let _ = studio.update(Message::RemoteResults(6, 1, Ok(stale)));
        assert!(
            studio.remote.is_none(),
            "a reply to an abandoned search hijacked the list"
        );
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
    fn the_spinner_turns_through_four_quadrants() {
        assert_eq!(spinner_frame(0), "◐");
        assert_eq!(spinner_frame(SPINNER_STEP_MS), "◓");
        assert_eq!(spinner_frame(2 * SPINNER_STEP_MS), "◑");
        assert_eq!(spinner_frame(3 * SPINNER_STEP_MS), "◒");
        assert_eq!(spinner_frame(4 * SPINNER_STEP_MS), "◐");
        // Long uptimes must not panic or divide by zero.
        let _ = spinner_frame(u64::MAX);
    }

    #[test]
    fn the_motion_precedence_is_the_keys() {
        assert!(resolve_reduce_motion(Some("1"), false));
        assert!(resolve_reduce_motion(Some("yes"), false));
        assert!(!resolve_reduce_motion(Some("0"), true));
        assert!(resolve_reduce_motion(None, true));
        assert!(!resolve_reduce_motion(None, false));
        // Blank is "not set", so it must not override a stored preference.
        assert!(resolve_reduce_motion(Some("  "), true));
    }

    #[test]
    fn the_status_line_fades_only_when_it_changes() {
        let mut studio = Studio::boot();
        let settle = std::time::Duration::from_millis(FADE_MS * 2);

        // Booting said something; let it finish.
        studio.now += settle;
        assert_eq!(studio.eased(&studio.note_fade), 1.0);

        // The same thing again is not something new, so the line does not restart.
        let _ = studio.update(Message::Refresh);
        studio.now += settle;
        assert_eq!(studio.eased(&studio.note_fade), 1.0);

        // A different line is, and it starts from the bottom.
        let _ = studio.update(Message::ArmRemove(3799603054));
        assert!(
            studio.eased(&studio.note_fade) < 1.0,
            "expected a fresh fade"
        );
    }

    #[test]
    fn no_motion_settles_everything_at_once() {
        let at = Instant::now();
        let still = fade_in(at, false);
        assert_eq!(still.interpolate_with(|value| value, at), 1.0);
        assert!(!still.is_animating(at));

        // With motion on, the same calls start at the bottom and are still running.
        assert_eq!(fade_in(at, true).interpolate_with(|value| value, at), 0.0);
    }

    #[test]
    fn a_fade_starts_transparent_and_ends_opaque() {
        let started = Instant::now();
        let fade = fade_in(started, true);

        let at_start: f32 = fade.interpolate_with(|value| value, started);
        assert_eq!(at_start, 0.0);
        assert!(fade.is_animating(started));

        let quarter: f32 = fade.interpolate_with(
            |value| value,
            started + std::time::Duration::from_millis(FADE_MS / 4),
        );
        assert!(quarter > 0.0 && quarter < 1.0, "quarter was {quarter}");

        let after: f32 = fade.interpolate_with(
            |value| value,
            started + std::time::Duration::from_millis(FADE_MS * 2),
        );
        assert_eq!(after, 1.0);
        assert!(!fade.is_animating(started + std::time::Duration::from_millis(FADE_MS * 2)));
    }

    #[test]
    fn long_asset_names_keep_their_head_and_tail() {
        assert_eq!(truncate_middle("short.apng", 28), "short.apng");

        let long = "ssstik.io_1790982665008_0_20261003_024542.apng";
        let cut = truncate_middle(long, INSTANCE_LABEL_MAX);
        assert_eq!(cut.chars().count(), INSTANCE_LABEL_MAX);
        assert!(cut.starts_with("ssstik.io"));
        assert!(cut.ends_with(".apng"));
        assert!(cut.contains('…'));

        // A budget too small for both ends is not worth a mark, and must not panic.
        assert_eq!(truncate_middle("abcdef", 3), "abcdef");
        // Counted in characters, so CJK names do not lose bytes mid-glyph.
        assert_eq!(
            truncate_middle("一二三四五六七八九十", 5).chars().count(),
            5
        );
    }

    #[test]
    fn instance_labels_are_short_unless_the_name_collides() {
        let instance = |label: &str| OverlayInstance {
            label: label.to_string(),
            path: PathBuf::from("/tmp/x.json"),
            state: ControlState::default(),
            running: false,
        };

        // The common case: just the file name, not half a Steam path.
        assert_eq!(
            instance_label(&instance("/w/3828587753/TETO12.apng"), false),
            "○ TETO12.apng"
        );
        // The same file name in another item: the workshop id says which one this is.
        assert_eq!(
            instance_label(&instance("/w/3828587753/TETO12.apng"), true),
            "○ 3828587753/TETO12.apng"
        );
        // Not a path at all: left alone.
        assert_eq!(instance_label(&instance("TETO12"), false), "○ TETO12");
        assert_eq!(instance_label(&instance("TETO12"), true), "○ TETO12");
    }

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
