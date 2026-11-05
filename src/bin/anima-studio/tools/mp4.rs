//! The MP4 card: one ffmpeg call, reading the video directly.

use crate::*;
use iced::widget::{button, column, container, pick_list, row, slider, text, text_input};

impl Studio {
    pub(super) fn mp4_card(&self) -> El<'_> {
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
        container(video_card)
            .style(iced_material::container::surface_container)
            .padding(12)
            .width(Length::Fill)
            .into()
    }
}

// The pick list's label: matched by template text, because that is what choosing an entry sets.
impl Studio {
    /// Which video preset the current command came from, for the pick list's label. Matched by template
    /// text, because that is what choosing an entry actually sets.
    pub(super) fn video_format_name(&self) -> String {
        anima_engine_rs::ffmpeg::VIDEO_PRESETS
            .iter()
            .find(|(_, template)| *template == self.video_command)
            .map(|(name, _)| (*name).to_string())
            .unwrap_or_else(|| "gif".to_string())
    }
}

// The messages this card handles, moved out of the window's single `handle` match.
impl Studio {
    pub(crate) fn video_pick_file(&mut self) -> Task<Message> {
        Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .add_filter("video", &["mp4", "mov", "mkv", "webm", "avi"])
                    .pick_file()
                    .await
                    .map(|file| file.path().display().to_string())
            },
            |picked| match picked {
                Some(path) => Message::VideoChosen(path),
                None => Message::VideoChosen(String::new()),
            },
        )
    }

    pub(crate) fn video_chosen(&mut self, path: String) -> Task<Message> {
        self.video_path = path;
        if !self.video_path.is_empty() {
            // A real number about the file instead of a promise that it will work.
            self.video_probe = anima_engine_rs::ffmpeg::probe_duration_seconds(&self.video_path)
                .map(|seconds| format!("{seconds:.1} s"));
            if self.video_output.is_empty() {
                let stem = std::path::Path::new(&self.video_path)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.video_output = format!("{stem}.gif");
            }
            self.video_note = String::new();
        }
        Task::none()
    }

    pub(crate) fn video_format(&mut self, chosen: String) -> Task<Message> {
        if let Some((_, template)) = anima_engine_rs::ffmpeg::VIDEO_PRESETS
            .iter()
            .find(|(name, _)| *name == chosen)
        {
            self.video_command = (*template).to_string();
        }
        Task::none()
    }

    pub(crate) fn video_pick_output(&mut self) -> Task<Message> {
        let suggestion = if self.video_output.is_empty() {
            "animation.gif".to_string()
        } else {
            self.video_output.clone()
        };
        Task::perform(
            async move {
                rfd::AsyncFileDialog::new()
                    .set_file_name(suggestion)
                    .save_file()
                    .await
                    .map(|file| file.path().display().to_string())
            },
            |picked| match picked {
                Some(path) => Message::VideoOutput(path),
                None => Message::VideoOutput(String::new()),
            },
        )
    }

    pub(crate) fn video_write(&mut self) -> Task<Message> {
        // The preconditions live here, where the catalog is; the worker's own failures are
        // ffmpeg's, and those are better left as ffmpeg wrote them.
        if self.video_path.trim().is_empty() {
            self.video_note = self.i18n.tr("choose a video first");
        } else if self.video_output.trim().is_empty() {
            self.video_note = self.i18n.tr("choose where to write first");
        } else {
            self.video_writing = true;
            self.video_note = String::new();
            let request = VideoWriteRequest {
                command: self.video_command.clone(),
                input: self.video_path.clone(),
                output: self.video_output.clone(),
                fps: self.video_fps,
                quality: self.video_quality,
                lossless: self.video_lossless,
            };
            return Task::perform(async move { video_write(request) }, Message::VideoWrote);
        }
        Task::none()
    }

    pub(crate) fn video_wrote(&mut self, result: Result<String, String>) -> Task<Message> {
        self.video_writing = false;
        self.video_note = match result {
            Ok(path) => format!("{} {path}", self.i18n.tr("wrote")),
            Err(detail) => detail,
        };
        Task::none()
    }
}
