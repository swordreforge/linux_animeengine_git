//! The Tools tab: three cards, one file each.
//!
//! Split again after it was split out: the single `tab_tools` was 345 lines, and a function that long is
//! where a careful edit goes wrong. Each card now builds itself and returns, so this file is the page rather
//! than the page and everything on it.

use crate::*;
use iced::widget::{column, scrollable, text};

pub(crate) mod ai;
pub(crate) mod key;
pub(crate) mod mp4;

impl Studio {
    /// The Tools tab. Three pipelines, each one building itself: the colour key, an animation through
    /// ffmpeg, and background removal by a model. All three run here rather than being described.
    pub(crate) fn tab_tools(&self) -> El<'_> {
        let mut page = column![
            text(self.i18n.tr("Tools")).size(20),
            text(self.i18n.tr(
                "the conversion and removal tools — the three pipelines below run here, in Rust",
            ))
            .size(12),
        ]
        .spacing(6);

        // Built by tools/ai.rs.
        page = page.push(self.ai_card());

        // Built by tools/mp4.rs.
        page = page.push(self.mp4_card());

        // Built by tools/key.rs.
        page = page.push(self.key_card());

        page = page.push(text(self.i18n.tr("checked once at startup")).size(10));
        scrollable(page.spacing(10))
            .style(iced_material::scrollable::default)
            .into()
    }
}
