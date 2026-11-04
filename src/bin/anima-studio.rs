//! `anima-studio` — the main window (Iced).
//!
//! First slice: the four primary languages, the installed Workshop library (the same
//! discovery `anima-workshop` uses) and per-item details. Requires the `studio`
//! feature: `cargo run --features studio --bin anima-studio`.
//!
//! Note: this pulls a second `wgpu` (iced 0.14 is on 27.x, the overlay is on 29.x).
//! They never meet — the overlay is a separate process and keeps owning its own GPU
//! stack — so the cost is build time and binary size, not correctness.

use anima_engine_rs::i18n::{I18n, PRIMARY_LANGUAGES, STEAM_LANGUAGES, normalize_language};
use anima_engine_rs::workshop::{WorkshopSnapshot, WorkshopWatcher};
use iced::widget::{button, column, container, pick_list, row, scrollable, space, text};
use iced::{Element, Length, Task};

const APP_TITLE: &str = "AnimaEngine";

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

#[derive(Debug, Clone)]
enum Message {
    Language(String),
    Refresh,
    Select(usize),
    Quit,
}

struct Studio {
    i18n: I18n,
    snapshot: WorkshopSnapshot,
    selected: Option<usize>,
    note: String,
}

impl Studio {
    fn boot() -> Self {
        let mut studio = Self {
            i18n: I18n::discover(),
            snapshot: WorkshopSnapshot::default(),
            selected: None,
            note: String::new(),
        };
        studio.refresh();
        studio
    }

    fn refresh(&mut self) {
        let mut watcher = WorkshopWatcher::discover();
        let _ = watcher.poll();
        self.snapshot = watcher.snapshot().clone();
        self.selected = None;
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

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Language(code) => {
                if let Err(error) = self.i18n.set_language(&code) {
                    self.note = format!("language not saved: {error}");
                }
            }
            Message::Refresh => self.refresh(),
            Message::Select(index) => self.selected = Some(index),
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
        let mut selected_item = None;
        for (index, (id, item)) in self.snapshot.items.iter().enumerate() {
            let name = item
                .assets
                .first()
                .and_then(|asset| asset.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.i18n.tr("Workshop"));
            if self.selected == Some(index) {
                selected_item = Some(item);
            }
            list = list.push(
                button(text(format!(
                    "{id} — {name} ({} asset(s))",
                    item.assets.len()
                )))
                .on_press(Message::Select(index))
                .width(Length::Fill),
            );
        }

        let details: Element<'_, Message> = match selected_item {
            Some(item) => {
                let mut column = column![text(item.path.display().to_string()).size(12)].spacing(4);
                for asset in &item.assets {
                    column = column.push(text(asset.display().to_string()).size(12));
                }
                scrollable(column).into()
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
}
