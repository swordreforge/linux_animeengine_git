//! The colour-key card, with the two helpers only it uses.

use crate::*;
use iced::widget::{
    button, column, container, image, row, slider, space, text, text_input, tooltip,
};

impl Studio {
    pub(super) fn key_card(&self) -> El<'_> {
        let mut key_card = column![
            text(self.i18n.tr("Background removal by colour")).size(15),
            text(self.i18n.tr(
                "pick a background colour and a tolerance, applied to every frame of an animation",
            ))
            .size(11),
            row![
                text_input(&self.i18n.tr("paste an animation path"), &self.key_path)
                    .on_input(Message::KeyPath)
                    .on_submit(Message::KeyPickFile)
                    .width(Length::Fill),
                button(text(self.i18n.tr("Choose file")))
                    .style(secondary)
                    .on_press(Message::KeyPickFile),
                button(text(self.i18n.tr("Load")))
                    .style(secondary)
                    .on_press(Message::KeyLoad),
            ]
            .spacing(8),
            self.key_swatches(),
            row![
                text(self.i18n.tr("tolerance")).size(11),
                slider(0..=100u8, self.key_tolerance, Message::KeyTolerance).width(Length::Fill),
                text(format!("{}%", self.key_tolerance)).size(11),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
            row![
                text(self.i18n.tr("feather")).size(11),
                slider(0..=100u8, self.key_feather, Message::KeyFeather).width(Length::Fill),
                text(format!("{}%", self.key_feather)).size(11),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(6);

        if !self.key_note.is_empty() {
            key_card = key_card.push(text(self.key_note.clone()).size(11));
        }
        if let (Some(before), Some(after)) = (self.key_before.clone(), self.key_after.clone()) {
            key_card = key_card.push(
                row![
                    column![
                        text(self.i18n.tr("original")).size(11),
                        image(before).height(Length::Fixed(150.0)),
                    ]
                    .spacing(4),
                    column![
                        text(self.i18n.tr("result")).size(11),
                        // No transparency backdrop exists in iced, so a keyed frame can land on the
                        // window's own surface and look like nothing happened. This container is what
                        // makes the transparent parts read as transparent.
                        container(image(after).height(Length::Fixed(150.0)))
                            .style(iced_material::container::surface_container_highest)
                            .padding(2),
                    ]
                    .spacing(4),
                ]
                .spacing(12),
            );
        }
        let formats: Vec<String> = anima_engine_rs::ffmpeg::PRESETS
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect();
        key_card = key_card.push(
            row![
                text(self.i18n.tr("format")).size(11),
                pick_list(formats, Some(self.key_format.clone()), Message::KeyFormat),
                text(self.i18n.tr("fps")).size(11),
                slider(1..=60u8, self.key_fps, Message::KeyFps).width(Length::Fixed(140.0)),
                text(format!("{}", self.key_fps)).size(11),
                text(self.i18n.tr("quality")).size(11),
                slider(0..=100u8, self.key_quality, Message::KeyQuality)
                    .width(Length::Fixed(140.0)),
                text(format!("{}", self.key_quality)).size(11),
                toggle(
                    self,
                    self.key_lossless,
                    "lossless",
                    "lossy",
                    Message::KeyLossless(!self.key_lossless),
                ),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .wrap(),
        );
        key_card = key_card.push(
            row![
                text(self.i18n.tr("ffmpeg command, editable")).size(11),
                text_input("ffmpeg …", &self.key_command)
                    .on_input(Message::KeyCommand)
                    .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        key_card = key_card.push(
            row![
                text(self.i18n.tr("output")).size(11),
                text_input("…", &self.key_output)
                    .on_input(Message::KeyOutput)
                    .width(Length::Fill),
                button(text(self.i18n.tr("Choose file")))
                    .style(secondary)
                    .on_press(Message::KeyPickOutput),
                if self.key_writing {
                    button(text(self.i18n.tr("writing…"))).style(secondary)
                } else {
                    button(text(self.i18n.tr("Write")))
                        .style(primary)
                        .on_press(Message::KeyWrite)
                },
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        // The template's vocabulary is not a secret: a field you cannot fill in is not editable.
        key_card = key_card.push(
            text(
                anima_engine_rs::ffmpeg::VARIABLES
                    .iter()
                    .map(|(name, what)| format!("{name} {what}"))
                    .collect::<Vec<_>>()
                    .join("   ·   "),
            )
            .size(10),
        );
        container(key_card)
            .style(iced_material::container::surface_container)
            .padding(12)
            .width(Length::Fill)
            .into()
    }
}

// Two helpers on Studio, not free functions: they read and write this window's own state.
impl Studio {
    /// frame, so calling it from every slider tick is fine; calling it from `view` would not be.
    pub(crate) fn key_redraw(&mut self) {
        let Some((width, height, rgba)) = self.key_source.clone() else {
            self.key_before = None;
            self.key_after = None;
            return;
        };
        self.key_before = Some(iced::widget::image::Handle::from_rgba(
            width,
            height,
            rgba.clone(),
        ));
        let mut keyed = rgba;
        anima_engine_rs::keying::key_out(
            &mut keyed,
            self.key_background,
            self.key_tolerance,
            self.key_feather,
        );
        self.key_after = Some(iced::widget::image::Handle::from_rgba(width, height, keyed));
    }

    /// The background choices offered as swatches. The corner button beside them is the one that matters
    /// on a real file; these are for the cases where the corner is not the background.
    pub(super) fn key_swatches(&self) -> El<'_> {
        let mut row_of_swatches = row![].spacing(6);
        for (label, rgb) in [
            ("white", [255u8, 255, 255]),
            ("black", [0, 0, 0]),
            ("chroma green", [0, 255, 0]),
            ("chroma blue", [0, 0, 255]),
        ] {
            let chosen = self.key_background == rgb;
            // A colour needs no caption: the swatch is its own explanation, so the word moves to the
            // tooltip where it costs nothing, and the selected one is ringed rather than described.
            row_of_swatches = row_of_swatches.push(tooltip(
                button(space())
                    .style(move |theme: &iced_material::Theme, status| {
                        let mut style = iced_material::button::outlined(theme, status);
                        style.background =
                            Some(iced::Color::from_rgb8(rgb[0], rgb[1], rgb[2]).into());
                        style.border = iced::Border {
                            radius: 9999.0.into(),
                            width: if chosen { 2.0 } else { 1.0 },
                            color: if chosen {
                                theme.colors().primary.color
                            } else {
                                theme.colors().surface.text_variant
                            },
                        };
                        style
                    })
                    .width(Length::Fixed(26.0))
                    .height(Length::Fixed(26.0))
                    .padding(0)
                    .on_press(Message::KeyBackground(rgb)),
                text(self.i18n.tr(label)).size(11),
                tooltip::Position::Top,
            ));
        }
        row_of_swatches
            .push(
                button(text(self.i18n.tr("use the corner pixel")).size(11))
                    .style(secondary)
                    .on_press(Message::KeyCorner),
            )
            .into()
    }
}

// The messages this card handles. Each one was an arm of the window's single `handle` match; they
// live here now because this is the state they read and write.
impl Studio {
    pub(crate) fn key_pick_file(&mut self) -> Task<Message> {
        // Blocking is what a native modal does; the portal call is wrapped in a task so the
        // answer arrives as a message like every other result in this window.
        Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .add_filter("animations", &["gif", "apng", "png", "webp", "mdaa"])
                    .pick_file()
                    .await
                    .map(|file| file.path().display().to_string())
            },
            |picked| match picked {
                Some(path) => Message::KeyChosen(path),
                None => Message::KeyChosen(String::new()),
            },
        )
    }

    pub(crate) fn key_format(&mut self, chosen: String) -> Task<Message> {
        if let Some((_, template)) = anima_engine_rs::ffmpeg::PRESETS
            .iter()
            .find(|(name, _)| *name == chosen)
        {
            self.key_command = (*template).to_string();
        }
        self.key_format = chosen;
        Task::none()
    }

    pub(crate) fn key_pick_output(&mut self) -> Task<Message> {
        let suggested = if self.key_output.is_empty() {
            self.key_path.clone()
        } else {
            self.key_output.clone()
        };
        Task::perform(
            async move {
                rfd::AsyncFileDialog::new()
                    .set_file_name(suggested)
                    .save_file()
                    .await
                    .map(|file| file.path().display().to_string())
            },
            |picked| match picked {
                Some(path) => Message::KeyOutput(path),
                None => Message::KeyOutput(String::new()),
            },
        )
    }

    pub(crate) fn key_write(&mut self) -> Task<Message> {
        // Checked here rather than in the worker, because this is where the catalog is: they are
        // sentences for a person, and the worker's own failures are ffmpeg's and the filesystem's.
        if self.key_source.is_none() {
            self.key_note = self.i18n.tr("nothing to write — load an animation first");
            self.key_writing = false;
        } else if self.key_output.trim().is_empty() {
            self.key_note = self.i18n.tr("choose where to write first");
            self.key_writing = false;
        } else {
            self.key_writing = true;
            self.key_note = String::new();
            let request = KeyWriteRequest {
                source: self.key_path.clone(),
                background: self.key_background,
                tolerance: self.key_tolerance,
                feather: self.key_feather,
                command: self.key_command.clone(),
                output: self.key_output.clone(),
                fps: self.key_fps,
                quality: self.key_quality,
                lossless: self.key_lossless,
            };
            return Task::perform(async move { key_write(request) }, Message::KeyWrote);
        }
        Task::none()
    }

    pub(crate) fn key_wrote(&mut self, result: Result<String, String>) -> Task<Message> {
        self.key_writing = false;
        self.key_note = match result {
            Ok(detail) => detail,
            Err(error) => error,
        };
        Task::none()
    }

    pub(crate) fn key_chosen(&mut self, path: String) -> Task<Message> {
        // A chosen path is finished, unlike a typed one, so it loads without a second press.
        self.key_path = path;
        if !self.key_path.is_empty() {
            self.key_load();
        }
        Task::none()
    }

    pub(crate) fn key_background(&mut self, rgb: [u8; 3]) -> Task<Message> {
        self.key_background = rgb;
        self.key_redraw();
        Task::none()
    }

    pub(crate) fn key_corner(&mut self) -> Task<Message> {
        if let Some((_, _, rgba)) = &self.key_source
            && rgba.len() >= 4
        {
            self.key_background = [rgba[0], rgba[1], rgba[2]];
        }
        self.key_redraw();
        Task::none()
    }

    pub(crate) fn key_tolerance(&mut self, value: u8) -> Task<Message> {
        self.key_tolerance = value;
        self.key_redraw();
        Task::none()
    }

    pub(crate) fn key_feather(&mut self, value: u8) -> Task<Message> {
        self.key_feather = value;
        self.key_redraw();
        Task::none()
    }
}
