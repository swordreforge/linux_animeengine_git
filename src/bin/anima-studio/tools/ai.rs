//! The AI card: an animation in, a folder of cut-out frames out.

use crate::*;
use iced::widget::{button, column, container, progress_bar, row, text, text_input};

impl Studio {
    pub(super) fn ai_card(&self) -> El<'_> {
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
        container(ai_card)
            .style(iced_material::container::surface_container)
            .padding(12)
            .width(Length::Fill)
            .into()
    }
}

// The messages this card handles, moved out of the window's single `handle` match.
impl Studio {
    pub(crate) fn ai_pick_file(&mut self) -> Task<Message> {
        Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .add_filter("animation", &["mp4", "gif", "webm", "mov", "mkv"])
                    .pick_file()
                    .await
                    .map(|file| file.path().display().to_string())
            },
            |picked| Message::AiChosen(picked.unwrap_or_default()),
        )
    }

    pub(crate) fn ai_chosen(&mut self, path: String) -> Task<Message> {
        self.ai_path = path;
        if !self.ai_path.is_empty() {
            // ffprobe reads the frame count here, where it is fast, so the bar the worker draws
            // can be a real fraction instead of a spinner.
            let frames = anima_engine_rs::ai::probe_frames(&self.ai_path);
            self.ai_state.reset(frames);
            self.ai_note = match frames {
                Some(count) => format!("{} {count}", self.i18n.tr("frames")),
                None => self.i18n.tr("frame count unknown").to_string(),
            };
            if self.ai_output.is_empty() {
                let stem = std::path::Path::new(&self.ai_path)
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.ai_output = format!("{stem} (no background)");
            }
        }
        Task::none()
    }

    pub(crate) fn ai_pick_output(&mut self) -> Task<Message> {
        let suggestion = self.ai_output.clone();
        Task::perform(
            async move {
                rfd::AsyncFileDialog::new()
                    .set_file_name(suggestion)
                    .pick_folder()
                    .await
                    .map(|folder| folder.path().display().to_string())
            },
            |picked| Message::AiOutput(picked.unwrap_or_default()),
        )
    }

    pub(crate) fn ai_start(&mut self) -> Task<Message> {
        if self.ai_path.trim().is_empty() {
            self.ai_note = self.i18n.tr("choose an animation first").to_string();
        } else if self.ai_output.trim().is_empty() {
            self.ai_note = self.i18n.tr("choose where to write first").to_string();
        } else if !anima_engine_rs::ai::model_ready() {
            self.ai_note = self.i18n.tr("the model is not downloaded yet").to_string();
        } else {
            self.ai_running = true;
            self.ai_note = self.i18n.tr("working…").to_string();
            self.ai_state
                .reset(anima_engine_rs::ai::probe_frames(&self.ai_path));
            self.ai_state
                .stopped
                .store(false, std::sync::atomic::Ordering::Relaxed);
            let request = AiRequest {
                video: self.ai_path.clone(),
                output: std::path::PathBuf::from(self.ai_output.trim()),
                running: self.ai_state.clone(),
            };
            return Task::perform(async move { ai_cut(request) }, Message::AiDone);
        }
        Task::none()
    }

    pub(crate) fn ai_cancel(&mut self) -> Task<Message> {
        // The worker reads this once per frame, so cancelling costs at most one frame's work.
        self.ai_state
            .stopped
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.ai_note = self.i18n.tr("stopping…").to_string();
        Task::none()
    }

    pub(crate) fn ai_done(&mut self, result: Result<String, String>) -> Task<Message> {
        self.ai_running = false;
        self.ai_note = match result {
            Ok(detail) => detail,
            Err(detail) => detail,
        };
        Task::none()
    }
}
