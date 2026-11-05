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
