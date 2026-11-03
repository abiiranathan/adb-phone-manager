//! The Logcat tab.

use eframe::egui::{self, Color32, RichText};

use super::{apply, DeviceCtx, Extras};
use crate::adb::Adb;
use crate::logcat::{self, Level};

const fn level_color(level: Level, dark: bool) -> Color32 {
    match (level, dark) {
        (Level::Verbose, true) => Color32::GRAY,
        (Level::Verbose, false) => Color32::DARK_GRAY,
        (Level::Debug, true) => Color32::from_rgb(120, 190, 255),
        (Level::Debug, false) => Color32::from_rgb(0, 90, 170),
        (Level::Info, true) => Color32::from_rgb(140, 220, 140),
        (Level::Info, false) => Color32::from_rgb(20, 120, 20),
        (Level::Warn, true) => Color32::from_rgb(240, 200, 80),
        (Level::Warn, false) => Color32::from_rgb(150, 100, 0),
        (Level::Error, true) => Color32::from_rgb(255, 120, 120),
        (Level::Error, false) => Color32::from_rgb(190, 20, 20),
        (Level::Fatal, true) => Color32::from_rgb(255, 80, 200),
        (Level::Fatal, false) => Color32::from_rgb(160, 0, 110),
    }
}

impl Extras {
    pub fn ui_logcat(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.horizontal_wrapped(|ui| {
            if self.log_stream.is_some() {
                if ui.button("⏹ Stop").clicked() {
                    self.log_stream = None;
                }
            } else if ui
                .add_enabled(dev.ready, egui::Button::new("▶ Start"))
                .clicked()
            {
                let adb = Adb::new(dev.serial.map(str::to_owned));
                match logcat::start(&adb, self.ctx.clone()) {
                    Ok(stream) => self.log_stream = Some(stream),
                    Err(err) => self.say(err.to_string(), true),
                }
            }

            if ui
                .button("Clear")
                .on_hover_text("Clears this view and the phone's log buffers")
                .clicked()
            {
                self.log_buffer.clear();
                if dev.ready {
                    self.run(dev, |adb| {
                        logcat::clear_device_log(&adb)?;
                        apply(|_: &mut Self| {})
                    });
                }
            }

            ui.label("Level:");
            egui::ComboBox::from_id_salt("log_level")
                .selected_text(self.log_level.label())
                .show_ui(ui, |ui| {
                    for level in Level::ALL {
                        ui.selectable_value(&mut self.log_level, level, level.label());
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.log_filter)
                    .hint_text("Filter by tag or text")
                    .desired_width(220.0),
            );
            ui.checkbox(&mut self.log_follow, "Follow");

            if ui.button("Save…").clicked() {
                self.save_log();
            }
        });

        let visible = self.log_buffer.filtered(self.log_level, &self.log_filter);
        ui.weak(format!(
            "{} of {} line(s)",
            visible.len(),
            self.log_buffer.len()
        ));
        ui.separator();

        let dark = ui.visuals().dark_mode;
        let row_height =
            ui.text_style_height(&egui::TextStyle::Monospace) + ui.spacing().item_spacing.y;
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .stick_to_bottom(self.log_follow)
            .show_rows(ui, row_height, visible.len(), |ui, range| {
                for line in &visible[range] {
                    let text = RichText::new(line.to_text())
                        .monospace()
                        .color(level_color(line.level, dark));
                    ui.add(egui::Label::new(text).extend().selectable(true));
                }
            });
    }

    fn save_log(&mut self) {
        let text: String = self
            .log_buffer
            .filtered(self.log_level, &self.log_filter)
            .iter()
            .map(|l| l.to_text() + "\n")
            .collect();
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("logcat.txt")
            .save_file()
        else {
            return;
        };
        match std::fs::write(&path, text) {
            Ok(()) => self.say(format!("Saved {}", path.display()), false),
            Err(err) => self.say(format!("cannot write {}: {err}", path.display()), true),
        }
    }
}
