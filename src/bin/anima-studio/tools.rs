//! The Tools tab: the three pipelines, and the helpers only they use.
//!
//! Moved out of the main window file verbatim. It was 425 lines in a 5000-line file, which is where every
//! editing mistake of this session happened — a bad cut is much harder to make in a file this size, and much
//! cheaper to notice.

// Explicit rather than left to the glob: `column!` and `row!` come from both a glob import and this
// module's own scope, and the compiler is right to refuse to guess which one was meant.
use super::*;
use iced::widget::{
    button, column, container, image, pick_list, progress_bar, row, scrollable, slider, space,
    text, text_input, tooltip,
};

impl Studio {
    /// Rebuild both previews from the decoded frame and the current controls. One pass over a scaled
    /// frame, so calling it from every slider tick is fine; calling it from `view` would not be.
    pub(super) fn key_redraw(&mut self) {
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

    /// Which video preset the current command came from, for the pick list's label. Matched by template
    /// text, because that is what choosing an entry actually sets.
    pub(super) fn video_format_name(&self) -> String {
        anima_engine_rs::ffmpeg::VIDEO_PRESETS
            .iter()
            .find(|(_, template)| *template == self.video_command)
            .map(|(name, _)| (*name).to_string())
            .unwrap_or_else(|| "gif".to_string())
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

    /// The Tools tab. Three pipelines the Python app offers, described accurately and **not wired yet** —
    /// no buttons, because a button that does nothing is worse than a sentence that says so. What *is*
    /// real here is the environment: whether the commands each pipeline needs exist on this machine.
    pub(super) fn tab_tools(&self) -> El<'_> {
        let mut page = column![
            text(self.i18n.tr("Tools")).size(20),
            text(self.i18n.tr(
                "the conversion and removal tools — the three pipelines below run here, in Rust",
            ))
            .size(12),
        ]
        .spacing(6);

        // The AI card: an animation in, a folder of cut-out frames out. ffmpeg does the extraction, so
        // nothing here had to learn a container format, and the model runs on the CPU at a few seconds a
        // frame — which is why this one gets a real progress bar and a stop button, not just a spinner.
        let mut ai_card = column![
            text(self.i18n.tr("Background removal by AI")).size(15),
            text(self.i18n.tr(
                "an animation in, a folder of png frames out — isnet-anime through ONNX Runtime",
            ))
            .size(11),
            row![
                text_input(&self.i18n.tr("paste an mp4 or gif path"), &self.ai_path)
                    .on_input(Message::AiPath)
                    .width(Length::Fill),
                button(text(self.i18n.tr("Choose file")))
                    .style(secondary)
                    .on_press(Message::AiPickFile),
            ]
            .spacing(8),
        ]
        .spacing(6);
        if anima_engine_rs::ai::model_ready() {
            ai_card = ai_card.push(
                text(format!(
                    "{} {}",
                    self.i18n.tr("model ready"),
                    anima_engine_rs::ai::MODEL_FILE
                ))
                .size(10),
            );
        } else {
            // Not a button that downloads nothing: it says what is missing and where it comes from.
            ai_card = ai_card.push(
                text(format!(
                    "{} {}",
                    self.i18n.tr("no model yet — fetch isnet-anime.onnx from"),
                    anima_engine_rs::ai::MODEL_URL
                ))
                .size(10),
            );
        }
        ai_card = ai_card.push(
            row![
                text(self.i18n.tr("output folder")).size(11),
                text_input("…", &self.ai_output)
                    .on_input(Message::AiOutput)
                    .width(Length::Fill),
                button(text(self.i18n.tr("Choose folder")))
                    .style(secondary)
                    .on_press(Message::AiPickOutput),
                if self.ai_running {
                    button(text(self.i18n.tr("Stop")))
                        .style(danger)
                        .on_press(Message::AiCancel)
                } else {
                    button(text(self.i18n.tr("Start")))
                        .style(primary)
                        .on_press(Message::AiStart)
                },
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        if self.ai_running {
            let done = self
                .ai_state
                .done
                .load(std::sync::atomic::Ordering::Relaxed);
            let total = self
                .ai_state
                .total
                .load(std::sync::atomic::Ordering::Relaxed);
            let fraction = if total == 0 {
                0.0
            } else {
                (done as f32 / total as f32).clamp(0.0, 1.0)
            };
            ai_card = ai_card.push(
                row![
                    progress_bar(0.0..=1.0, fraction).girth(6.0),
                    text(format!("{done} / {total}")).size(10),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
        }
        if !self.ai_note.is_empty() {
            ai_card = ai_card.push(text(self.ai_note.clone()).size(11));
        }
        page = page.push(
            container(ai_card)
                .style(iced_material::container::surface_container)
                .padding(12)
                .width(Length::Fill),
        );

        // The MP4 card. The same shape as the keying panel below it, and one ffmpeg invocation: ffmpeg
        // reads the video itself, so nothing here decodes anything and there is no frame directory.
        let mut video_card = column![
            text(self.i18n.tr("MP4 to animation")).size(15),
            text(self.i18n.tr(
                "mp4 — one ffmpeg call, reading the video directly; the frame rate is a field, not a literal",
            ))
            .size(11),
            row![
                text_input(&self.i18n.tr("paste a video path"), &self.video_path)
                    .on_input(Message::VideoPath)
                    .width(Length::Fill),
                button(text(self.i18n.tr("Choose file")))
                    .style(secondary)
                    .on_press(Message::VideoPickFile),
            ]
            .spacing(8),
        ]
        .spacing(6);
        if let Some(probe) = &self.video_probe {
            video_card = video_card.push(text(format!("ffprobe: {probe}")).size(11));
        }
        let video_formats: Vec<String> = anima_engine_rs::ffmpeg::VIDEO_PRESETS
            .iter()
            .map(|(name, _)| (*name).to_string())
            .collect();
        video_card = video_card.push(
            row![
                text(self.i18n.tr("format")).size(11),
                pick_list(
                    video_formats,
                    Some(self.video_format_name()),
                    Message::VideoFormat
                ),
                text(self.i18n.tr("fps")).size(11),
                slider(1..=60u8, self.video_fps, Message::VideoFps).width(Length::Fixed(120.0)),
                text(format!("{}", self.video_fps)).size(11),
                text(self.i18n.tr("quality")).size(11),
                slider(0..=100u8, self.video_quality, Message::VideoQuality)
                    .width(Length::Fixed(120.0)),
                text(format!("{}", self.video_quality)).size(11),
                toggle(
                    self,
                    self.video_lossless,
                    "lossless",
                    "lossy",
                    Message::VideoLossless(!self.video_lossless),
                ),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .wrap(),
        );
        video_card = video_card.push(
            row![
                text(self.i18n.tr("ffmpeg command, editable")).size(11),
                text_input("ffmpeg …", &self.video_command)
                    .on_input(Message::VideoCommand)
                    .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        video_card = video_card.push(
            row![
                text(self.i18n.tr("output")).size(11),
                text_input("…", &self.video_output)
                    .on_input(Message::VideoOutput)
                    .width(Length::Fill),
                button(text(self.i18n.tr("Choose file")))
                    .style(secondary)
                    .on_press(Message::VideoPickOutput),
                if self.video_writing {
                    button(text(self.i18n.tr("writing…"))).style(secondary)
                } else {
                    button(text(self.i18n.tr("Write")))
                        .style(primary)
                        .on_press(Message::VideoWrite)
                },
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        if !self.video_note.is_empty() {
            video_card = video_card.push(text(self.video_note.clone()).size(11));
        }
        // No progress bar, deliberately: a real one needs a fraction, and the only source of one is
        // parsing ffmpeg's own output. It says "writing…" while it works, which is true.
        page = page.push(
            container(video_card)
                .style(iced_material::container::surface_container)
                .padding(12)
                .width(Length::Fill),
        );

        // The colour-key panel is the first thing on this page that actually does something, so it is
        // built here rather than in the placeholder loop above. What it does not do is write a file:
        // encoding gif/apng/webp is the next step, and the line at the bottom says so.
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
        page = page.push(
            container(key_card)
                .style(iced_material::container::surface_container)
                .padding(12)
                .width(Length::Fill),
        );

        page = page.push(text(self.i18n.tr("checked once at startup")).size(10));
        scrollable(page.spacing(10))
            .style(iced_material::scrollable::default)
            .into()
    }
}
