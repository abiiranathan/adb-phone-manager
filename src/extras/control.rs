//! The Control tab: navigation, media, typing, quick settings, screenshots,
//! screen recording, remote camera and power.

use eframe::egui;

use super::{apply, done, quiet, DeviceCtx, Extras, ScreenshotView};
use crate::capture::{self, CameraMode, Shutter};
use crate::controls::{self, Key, Panel, PowerAction, Stream, Toggle};

const MAX_VOLUME_INDEX: u32 = 25;
const PREVIEW_MAX_HEIGHT: f32 = 380.0;

impl Extras {
    pub fn ui_control(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.add_enabled_ui(dev.ready, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.ui_navigation(ui, dev);
                    self.ui_media(ui, dev);
                    self.ui_input(ui, dev);
                    self.ui_toggles(ui, dev);
                    self.ui_capture(ui, dev);
                    self.ui_camera(ui, dev);
                    self.ui_power(ui);
                });
        });
        if !dev.ready {
            ui.weak("Select an authorised device to use these controls.");
        }
    }

    fn key_buttons(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx, keys: &[Key]) {
        for &key in keys {
            if ui.button(key.label()).clicked() {
                self.press(dev, key);
            }
        }
    }

    fn ui_navigation(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("Navigation & screen", |ui| {
            ui.horizontal_wrapped(|ui| {
                self.key_buttons(ui, dev, &[Key::Back, Key::Home, Key::Recents, Key::Menu]);
                ui.separator();
                self.key_buttons(ui, dev, &[Key::Power, Key::Wake, Key::Sleep]);
            });
            ui.horizontal_wrapped(|ui| {
                for (label, panel) in [
                    ("Notifications", Panel::Notifications),
                    ("Quick settings", Panel::QuickSettings),
                    ("Collapse", Panel::Collapse),
                ] {
                    if ui.button(label).clicked() {
                        self.run(dev, move |adb| {
                            controls::panel(&adb, panel)?;
                            quiet()
                        });
                    }
                }
            });
            ui.horizontal(|ui| {
                self.key_buttons(
                    ui,
                    dev,
                    &[
                        Key::DpadLeft,
                        Key::DpadUp,
                        Key::DpadDown,
                        Key::DpadRight,
                        Key::DpadCenter,
                    ],
                );
            });
            ui.horizontal(|ui| {
                ui.label("Brightness:");
                let slider = ui.add(egui::Slider::new(&mut self.brightness, 0..=255));
                if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                    let level = self.brightness;
                    self.run(dev, move |adb| {
                        controls::set_brightness(&adb, level)?;
                        quiet()
                    });
                }
            });
        });
    }

    fn ui_media(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("Media & volume", |ui| {
            ui.horizontal_wrapped(|ui| {
                self.key_buttons(
                    ui,
                    dev,
                    &[
                        Key::Previous,
                        Key::PlayPause,
                        Key::Next,
                        Key::VolumeDown,
                        Key::VolumeUp,
                        Key::Mute,
                    ],
                );
            });
            for (label, stream) in [
                ("Media volume:", Stream::Media),
                ("Ringer volume:", Stream::Ring),
            ] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    let value = match stream {
                        Stream::Media => &mut self.media_volume,
                        Stream::Ring => &mut self.ring_volume,
                    };
                    let slider = ui.add(egui::Slider::new(value, 0..=MAX_VOLUME_INDEX));
                    if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                        let level = *value;
                        self.run(dev, move |adb| {
                            controls::set_volume(&adb, stream, level)?;
                            quiet()
                        });
                    }
                });
            }
        });
    }

    fn ui_input(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("Type & open", |ui| {
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.type_input)
                        .hint_text("Text to type into the focused field (ASCII only)")
                        .desired_width(340.0),
                );
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Type").clicked() || enter {
                    let text = self.type_input.clone();
                    self.run(dev, move |adb| {
                        controls::type_text(&adb, &text)?;
                        quiet()
                    });
                }
                self.key_buttons(ui, dev, &[Key::Enter, Key::Delete, Key::Tab, Key::Escape]);
            });
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.url_input)
                        .hint_text("https://example.com")
                        .desired_width(340.0),
                );
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Open in phone browser").clicked() || enter {
                    let url = self.url_input.clone();
                    self.run(dev, move |adb| {
                        controls::open_url(&adb, &url)?;
                        done("Opened the link on the phone.")
                    });
                }
            });
        });
    }

    fn ui_toggles(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("Quick settings", |ui| {
            egui::Grid::new("toggle_grid").num_columns(3).show(ui, |ui| {
                for toggle in Toggle::ALL {
                    ui.label(toggle.label());
                    for (text, on) in [("On", true), ("Off", false)] {
                        if ui.button(text).clicked() {
                            self.run(dev, move |adb| {
                                controls::set_toggle(&adb, toggle, on)?;
                                done(format!("{}: {}.", toggle.label(), if on { "on" } else { "off" }))
                            });
                        }
                    }
                    ui.end_row();
                }
            });
            ui.weak("Some phone makers block individual switches (Bluetooth and mobile data most often).");
        });
    }

    fn ui_capture(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("Screenshot & screen recording", |ui| {
            ui.horizontal(|ui| {
                if ui.button("📸 Take screenshot").clicked() {
                    let ctx = self.ctx.clone();
                    self.run(dev, move |adb| {
                        let shot = capture::screenshot(&adb)?;
                        let preview = crate::raster::decode(&shot.png).map_err(crate::adb::AdbError::Local)?;
                        apply(move |extras: &mut Self| {
                            let texture = ctx.load_texture(
                                "screenshot",
                                preview.to_color_image(),
                                egui::TextureOptions::LINEAR,
                            );
                            extras.say(format!("Saved {}", shot.path.display()), false);
                            extras.screenshot = Some(ScreenshotView { path: shot.path, texture });
                        })
                    });
                }
                ui.separator();
                ui.label("Record");
                ui.add(egui::DragValue::new(&mut self.record_secs).range(1..=capture::MAX_RECORD_SECS).suffix(" s"));
                ui.add(egui::DragValue::new(&mut self.record_mbps).range(1..=50).suffix(" Mbit/s"));
                if ui.button("⏺ Record screen").clicked() {
                    let (secs, mbps) = (self.record_secs, self.record_mbps);
                    self.say(format!("Recording for {secs} s…"), false);
                    self.run(dev, move |adb| {
                        let path = capture::record_screen(&adb, secs, mbps)?;
                        done(format!("Saved {}", path.display()))
                    });
                }
            });
            ui.weak("Recording has no audio and is capped at 180 s. Use Mirror → Record for longer clips with sound.");
            if let Some(view) = &self.screenshot {
                ui.add(egui::Image::new(&view.texture).max_height(PREVIEW_MAX_HEIGHT));
                ui.horizontal(|ui| {
                    ui.weak(view.path.display().to_string());
                    if ui.small_button("Copy path").clicked() {
                        ui.ctx().copy_text(view.path.display().to_string());
                    }
                });
            }
        });
    }

    fn ui_camera(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("Remote camera", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (label, mode) in [("Open camera (photo)", CameraMode::Photo), ("Open camera (video)", CameraMode::Video)] {
                    if ui.button(label).clicked() {
                        self.run(dev, move |adb| {
                            capture::open_camera(&adb, mode)?;
                            quiet()
                        });
                    }
                }
                ui.label("Shutter key:");
                egui::ComboBox::from_id_salt("shutter_key")
                    .selected_text(self.shutter.label())
                    .show_ui(ui, |ui| {
                        for shutter in Shutter::ALL {
                            ui.selectable_value(&mut self.shutter, shutter, shutter.label());
                        }
                    });
                if ui.button("Shutter").clicked() {
                    let shutter = self.shutter;
                    self.run(dev, move |adb| {
                        capture::press_shutter(&adb, shutter)?;
                        quiet()
                    });
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button("📷 Take photo & download").clicked() {
                    let shutter = self.shutter;
                    self.say("Taking a photo…", false);
                    self.run(dev, move |adb| {
                        let path = capture::take_photo(&adb, shutter)?;
                        done(format!("Photo saved to {}", path.display()))
                    });
                }
                if ui.button("Download newest photo/video").clicked() {
                    self.run(dev, move |adb| {
                        let path = capture::fetch_newest_media(&adb)?;
                        done(format!("Saved {}", path.display()))
                    });
                }
            });
            ui.weak("This drives the phone's own camera app. For a live preview use Mirror → Camera preview.");
        });
    }

    fn ui_power(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Power", |ui| {
            ui.horizontal(|ui| {
                for action in [
                    PowerAction::Reboot,
                    PowerAction::Recovery,
                    PowerAction::Bootloader,
                    PowerAction::PowerOff,
                ] {
                    if ui.button(action.label()).clicked() {
                        self.pending_power = Some(action);
                    }
                }
            });
        });
    }
}
