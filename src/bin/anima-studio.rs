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

use anima_engine_rs::i18n::{I18n, PRIMARY_LANGUAGES, STEAM_LANGUAGES, normalize_language};
use anima_engine_rs::paths::{ensure_dir, user_data_dir};
use anima_engine_rs::steamcmd::SteamCmd;
use anima_engine_rs::workshop::{WorkshopItem, WorkshopSnapshot, WorkshopWatcher};
use anima_engine_rs::workshop_web::WorkshopWeb;
use iced::widget::{button, column, container, image, pick_list, row, scrollable, space, text};
use iced::{Element, Length, Task};

const APP_TITLE: &str = "AnimaEngine";
const THUMB_WIDTH: f32 = 320.0;
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

/// Newtype so `pick_list` shows the native name while carrying the catalog code.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Language(String);

impl std::fmt::Display for Language {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&native_name(&self.0))
    }
}

/// Steam metadata for one item (keyless `GetPublishedFileDetails`).
#[derive(Debug, Clone, Default)]
struct Meta {
    title: String,
    subscriptions: u64,
    views: u64,
    preview_url: String,
}

/// A decoded preview: ready to hand to the renderer.
#[derive(Debug, Clone)]
enum Thumb {
    Loading,
    Ready {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Failed(String),
}

/// Everything the worker thread produces for one item.
struct Loaded {
    id: u64,
    meta: Option<Meta>,
    thumb: Result<(u32, u32, Vec<u8>), String>,
}

#[derive(Debug, Clone)]
enum Message {
    Language(String),
    Refresh,
    Select(usize),
    Loaded(u64, Option<Meta>, Result<(u32, u32, Vec<u8>), String>),
    Download(u64),
    OpenPage(u64),
    ArmRemove(u64),
    Remove(u64),
    Quit,
}

fn thumb_dir() -> PathBuf {
    user_data_dir().join("thumbnails")
}

/// Keyless metadata fetch.
fn fetch_meta(id: u64) -> Option<Meta> {
    let web = WorkshopWeb::new().ok()?;
    let details = web.fetch_published_file_details(&[id]).ok()?;
    let first = details.into_iter().next()?;
    Some(Meta {
        title: first.title,
        subscriptions: first.subscriptions,
        views: first.views,
        preview_url: first.preview_url,
    })
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
    let meta = fetch_meta(id);
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
        };
        studio.refresh();
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
                        Ok((width, height, rgba)) => Thumb::Ready {
                            width,
                            height,
                            rgba,
                        },
                        Err(error) => Thumb::Failed(error),
                    },
                );
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
            button(text(self.i18n.tr("Search"))).on_press(Message::Refresh),
            button(text(self.i18n.tr("Quit"))).on_press(Message::Quit),
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
            list = list.push(
                button(text(format!(
                    "{id} — {name} ({} asset(s))",
                    item.assets.len()
                )))
                .on_press(Message::Select(index))
                .width(Length::Fill),
            );
        }

        let details: Element<'_, Message> = match self.selected.and_then(|index| self.item(index)) {
            Some(item) => {
                let id = item.id;
                let mut pane = column![text(item.path.display().to_string()).size(12)].spacing(8);

                match self.thumbs.get(&id) {
                    Some(Thumb::Ready {
                        width,
                        height,
                        rgba,
                    }) => {
                        pane = pane.push(
                            image(image::Handle::from_rgba(*width, *height, rgba.clone()))
                                .width(Length::Fixed(THUMB_WIDTH))
                                .content_fit(iced::ContentFit::Contain),
                        )
                    }
                    Some(Thumb::Loading) => {
                        pane = pane.push(text("…").size(12));
                    }
                    Some(Thumb::Failed(error)) => {
                        pane = pane.push(text(format!("no preview: {error}")).size(12));
                    }
                    None => {}
                }

                if let Some(meta) = self.meta.get(&id) {
                    pane = pane.push(text(meta.title.clone()).size(16));
                    pane = pane.push(
                        text(format!(
                            "{} subscriptions · {} views · {}",
                            meta.subscriptions, meta.views, meta.preview_url
                        ))
                        .size(11),
                    );
                }

                let mut actions = row![
                    button(text(format!("{} (SteamCMD)", self.i18n.tr("Search"))))
                        .on_press(Message::Download(id)),
                    button(text("Subscribe… (Steam)")).on_press(Message::OpenPage(id)),
                    button(text("Unsubscribe… (Steam)")).on_press(Message::OpenPage(id)),
                ]
                .spacing(8);
                actions = if self.pending_remove == Some(id) {
                    actions.push(button(text("Confirm remove")).on_press(Message::Remove(id)))
                } else {
                    actions.push(button(text("Remove local copy")).on_press(Message::ArmRemove(id)))
                };
                pane = pane.push(actions);

                for asset in &item.assets {
                    pane = pane.push(text(asset.display().to_string()).size(11));
                }
                scrollable(pane).into()
            }
            None => text(self.note.clone()).into(),
        };

        let bodies = row![
            scrollable(list)
                .height(Length::Fill)
                .width(Length::FillPortion(2)),
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
        .window_size((1100.0, 720.0))
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

        // Second load must not need the network.
        let second = load_item(id, dir);
        assert!(second.thumb.is_ok());
    }
}
