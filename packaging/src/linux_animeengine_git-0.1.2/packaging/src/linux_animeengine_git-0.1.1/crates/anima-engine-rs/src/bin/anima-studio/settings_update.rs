//! The messages this window handles on its own, moved out of `handle` so that it dispatches
//! rather than works. Each was an arm of that match, verbatim, with the types the enum declares.

use crate::*;

impl Studio {
    pub(crate) fn settings_frame(&mut self, at: Instant) -> Task<Message> {
        self.spin_ms = self
            .spin_ms
            .saturating_add(at.duration_since(self.last_frame).as_millis() as u64);
        self.last_frame = at;
        self.now = at;
        Task::none()
    }

    pub(crate) fn settings_toggle_follow(&mut self) -> Task<Message> {
        self.follow = !self.follow;
        self.note = format!(
            "{}: {}",
            self.i18n.tr("follow"),
            if self.follow { "●" } else { "○" }
        );
        Task::none()
    }

    pub(crate) fn settings_save_key(&mut self) -> Task<Message> {
        use anima_engine_rs::settings::{ConfigFile, StudioSettings};

        let path = anima_engine_rs::workshop_web::studio_settings_path();
        let mut stored = StudioSettings::load(&path);
        stored.steam_api_key = self.key_draft.trim().to_string();
        self.note = match stored.save(&path) {
            Ok(()) => {
                // Never leave the key sitting in a text field.
                self.key_draft.clear();
                self.i18n.tr("Steam API key saved")
            }
            Err(error) => format!("{}: {error}", self.i18n.tr("could not save the key")),
        };
        Task::none()
    }

    pub(crate) fn settings_toggle_reduce_motion(&mut self) -> Task<Message> {
        use anima_engine_rs::settings::{ConfigFile, StudioSettings};

        let path = anima_engine_rs::workshop_web::studio_settings_path();
        let mut stored = StudioSettings::load(&path);
        stored.reduce_motion = !stored.reduce_motion;
        self.note = match stored.save(&path) {
            Ok(()) => self.i18n.tr("motion preference saved"),
            Err(error) => format!(
                "{}: {error}",
                self.i18n.tr("could not save the motion preference")
            ),
        };
        // Re-resolve instead of flipping in memory: the environment may override the file, and
        // the UI must not claim a change that will not happen.
        self.motion = motion_enabled();
        if std::env::var_os("ANIMA_NO_MOTION").is_some() {
            self.note.push_str(&format!(
                " — {}",
                self.i18n.tr("ANIMA_NO_MOTION is set and overrides it")
            ));
        }
        Task::none()
    }

    pub(crate) fn settings_copy_diagnostics(&mut self) -> Task<Message> {
        iced::clipboard::write(self.diagnostics_lines().join("\n"))
    }

    pub(crate) fn settings_language(&mut self, code: String) -> Task<Message> {
        if let Err(error) = self.i18n.set_language(&code) {
            self.note = format!("{}: {error}", self.i18n.tr("language not saved"));
        }
        Task::none()
    }

    pub(crate) fn settings_scale_draft(&mut self, control: PathBuf, text: String) -> Task<Message> {
        let cleaned: String = text.chars().filter(char::is_ascii_digit).take(3).collect();
        self.scale_draft = Some((control, cleaned));
        Task::none()
    }

    pub(crate) fn settings_commit_scale(&mut self, control: PathBuf) -> Task<Message> {
        let percent = self
            .scale_draft
            .as_ref()
            .filter(|(path, _)| path == &control)
            .and_then(|(_, text)| text.parse::<u32>().ok());
        self.scale_draft = None;
        if let Some(percent) = percent {
            self.set_scale_on(control, percent);
        }
        Task::none()
    }

    pub(crate) fn settings_asset_preview(
        &mut self,
        asset: PathBuf,
        loaded: Result<Still, String>,
    ) -> Task<Message> {
        if loaded.is_ok() {
            self.asset_fade = fade_in(self.now, self.motion);
        }
        self.asset_previews.insert(
            asset,
            match loaded {
                Ok((width, height, rgba)) => {
                    Thumb::Ready(image::Handle::from_rgba(width, height, rgba))
                }
                Err(error) => Thumb::Failed(error),
            },
        );
        Task::none()
    }
}
