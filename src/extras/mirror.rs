//! The Mirror tab: scrcpy sessions for screen, camera, microphone and audio.

use eframe::egui::{self, Color32, RichText};

use super::{apply, DeviceCtx, Extras};
use crate::scrcpy::{self, Facing, Mode};

impl Extras {
    pub fn ui_mirror(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        self.probe_scrcpy();

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Screen, camera & audio");
            self.ui_scrcpy_status(ui);
            ui.add_space(6.0);

            self.ui_mode_picker(ui);
            ui.add_space(4.0);
            self.ui_mode_options(ui, dev);
            ui.add_space(4.0);
            self.ui_quality_and_recording(ui);
            ui.add_space(8.0);
            self.ui_start_button(ui, dev);

            ui.add_space(8.0);
            ui.separator();
            self.ui_sessions(ui);

            ui.add_space(8.0);
            ui.separator();
            ui.weak(
                "Using the phone as a speaker for this computer's audio is not supported over adb; \
                 only phone → computer audio (microphone and playback) is supported.",
            );
        });
    }

    fn probe_scrcpy(&mut self) {
        if self.scrcpy_probe_started {
            return;
        }
        self.scrcpy_probe_started = true;
        self.jobs.spawn(|| {
            let result = scrcpy::detect_version().map_err(|err| err.to_string());
            apply(move |extras: &mut Self| extras.scrcpy_version = Some(result))
        });
    }

    fn ui_scrcpy_status(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match &self.scrcpy_version {
                None => {
                    ui.weak("Looking for scrcpy…");
                }
                Some(Ok(version)) => {
                    ui.label(format!("scrcpy {version} found."));
                }
                Some(Err(message)) => {
                    ui.colored_label(ui.visuals().error_fg_color, message);
                }
            }
            if ui.small_button("Check again").clicked() {
                self.scrcpy_version = None;
                self.scrcpy_probe_started = false;
            }
        });
    }

    fn ui_mode_picker(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Mode:");
            egui::ComboBox::from_id_salt("scrcpy_mode")
                .selected_text(self.scrcpy.mode.label())
                .width(260.0)
                .show_ui(ui, |ui| {
                    for mode in Mode::ALL {
                        let label = if mode.available_here() {
                            mode.label().to_owned()
                        } else {
                            format!("{} (not on this OS)", mode.label())
                        };
                        ui.add_enabled_ui(mode.available_here(), |ui| {
                            ui.selectable_value(&mut self.scrcpy.mode, mode, label);
                        });
                    }
                });
        });
        ui.weak(self.scrcpy.mode.hint());
        if let Some(Ok(version)) = &self.scrcpy_version {
            let needed = self.scrcpy.mode.min_version();
            if *version < needed {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("This mode needs scrcpy {needed} or newer."),
                );
            }
        }
    }

    fn ui_mode_options(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        let mode = self.scrcpy.mode;
        if matches!(mode, Mode::Camera | Mode::Webcam) {
            self.ui_camera_options(ui, dev);
        }
        match mode {
            Mode::Mirror => {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.scrcpy.control, "Mouse & keyboard control");
                    ui.checkbox(&mut self.scrcpy.audio, "Forward audio");
                    ui.checkbox(&mut self.scrcpy.turn_screen_off, "Turn phone screen off");
                    ui.checkbox(&mut self.scrcpy.stay_awake, "Stay awake");
                    ui.checkbox(&mut self.scrcpy.show_touches, "Show touches");
                    ui.checkbox(&mut self.scrcpy.always_on_top, "Always on top");
                });
            }
            Mode::Camera => {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.scrcpy.audio, "Forward audio");
                    ui.checkbox(&mut self.scrcpy.always_on_top, "Always on top");
                });
            }
            Mode::DeviceAudio => {
                ui.checkbox(
                    &mut self.scrcpy.audio_duplicate,
                    "Keep playing on the phone too (Android 13+)",
                );
            }
            Mode::Webcam => {
                let dev_exists = std::path::Path::new(&self.scrcpy.v4l2_device).exists();
                ui.horizontal(|ui| {
                    ui.label("V4L2 device:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.scrcpy.v4l2_device)
                            .desired_width(160.0),
                    );
                    if dev_exists {
                        ui.colored_label(Color32::LIGHT_GREEN, "✓ Device ready");
                    } else {
                        ui.colored_label(ui.visuals().error_fg_color, "✗ Node not found");
                    }
                });

                if !dev_exists {
                    ui.horizontal(|ui| {
                        ui.weak("Run in terminal:");
                        let cmd_str = format!(
                            "sudo modprobe v4l2loopback video_nr={} card_label=PhoneWebcam exclusive_caps=1",
                            self.scrcpy.v4l2_device.trim_start_matches("/dev/video")
                        );
                        if ui.small_button("📋 Copy modprobe command").clicked() {
                            ui.ctx().copy_text(cmd_str);
                            self.say("Copied modprobe command to clipboard.", false);
                        }
                    });
                }
            }
            Mode::Microphone => {}
        }
    }

    fn ui_camera_options(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Lens:");
            egui::ComboBox::from_id_salt("scrcpy_facing")
                .selected_text(self.scrcpy.facing.label())
                .show_ui(ui, |ui| {
                    for facing in Facing::ALL {
                        ui.selectable_value(&mut self.scrcpy.facing, facing, facing.label());
                    }
                });

            if ui
                .add_enabled(dev.ready, egui::Button::new("Detect cameras"))
                .on_hover_text("List every camera of the phone (ultrawide, telephoto…)")
                .clicked()
            {
                let serial = dev.serial.map(str::to_owned);
                self.jobs.spawn(move || {
                    let cameras = scrcpy::list_cameras(serial.as_deref())?;
                    apply(move |extras: &mut Self| {
                        extras.say(format!("Found {} camera(s).", cameras.len()), false);
                        extras.cameras = cameras;
                    })
                });
            }

            let selected = self
                .cameras
                .iter()
                .find(|c| c.id == self.scrcpy.camera_id)
                .map_or_else(|| "By lens".to_owned(), scrcpy::CameraInfo::label);
            egui::ComboBox::from_id_salt("scrcpy_camera")
                .selected_text(selected)
                .width(300.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.scrcpy.camera_id, String::new(), "By lens");
                    for camera in &self.cameras {
                        ui.selectable_value(
                            &mut self.scrcpy.camera_id,
                            camera.id.clone(),
                            camera.label(),
                        );
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("Size:");
            ui.add(
                egui::TextEdit::singleline(&mut self.scrcpy.camera_size)
                    .hint_text("1920x1080")
                    .desired_width(100.0),
            );
            ui.label("FPS:");
            ui.add(egui::DragValue::new(&mut self.scrcpy.camera_fps).range(0..=240));
            ui.weak("(0 = default)");
        });
    }

    fn ui_quality_and_recording(&mut self, ui: &mut egui::Ui) {
        if !matches!(self.scrcpy.mode, Mode::Microphone | Mode::DeviceAudio) {
            ui.horizontal(|ui| {
                ui.label("Max size:");
                ui.add(
                    egui::DragValue::new(&mut self.scrcpy.max_size)
                        .range(0..=8192)
                        .speed(16),
                );
                ui.label("Bit rate (Mbit/s):");
                ui.add(egui::DragValue::new(&mut self.scrcpy.bit_rate_mbps).range(0..=200));
                ui.label("Max FPS:");
                ui.add(egui::DragValue::new(&mut self.scrcpy.max_fps).range(0..=240));
                ui.weak("(0 = default)");
            });
        }
        if self.scrcpy.mode != Mode::Webcam {
            ui.horizontal(|ui| {
                ui.label("Record to:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.scrcpy.record_path)
                        .hint_text("optional: /path/to/recording.mp4")
                        .desired_width(320.0),
                );
                if ui.button("Choose…").clicked() {
                    let suggestion = match self.scrcpy.mode {
                        Mode::Microphone | Mode::DeviceAudio => "recording.opus",
                        _ => "recording.mp4",
                    };
                    if let Some(path) = rfd::FileDialog::new().set_file_name(suggestion).save_file()
                    {
                        self.scrcpy.record_path = path.display().to_string();
                    }
                }
                ui.checkbox(&mut self.scrcpy.record_only, "No window");
            });
        }
    }

    fn ui_start_button(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        let mode = self.scrcpy.mode;
        let version_ok = match &self.scrcpy_version {
            Some(Ok(v)) => *v >= mode.min_version(),
            _ => false,
        };
        let can_start = dev.ready && mode.available_here() && version_ok;
        let button = egui::Button::new(RichText::new("▶ Start").strong());
        let response = ui.add_enabled(can_start, button);
        if !dev.ready {
            response.on_disabled_hover_text("Select an authorised device first");
        } else if !version_ok {
            response.on_disabled_hover_text("scrcpy is missing or too old for this mode");
        } else if response.clicked() {
            match scrcpy::launch(&self.scrcpy, dev.serial) {
                Ok(session) => {
                    let label = session.label().to_owned();
                    self.sessions.push(session);
                    self.say(format!("Started: {label}."), false);
                }
                Err(err) => self.say(err.to_string(), true),
            }
        }
    }

    fn ui_sessions(&mut self, ui: &mut egui::Ui) {
        ui.strong("Running sessions");
        if self.sessions.is_empty() {
            ui.weak("None.");
            return;
        }
        let mut stop: Option<usize> = None;
        for (index, session) in self.sessions.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(Color32::LIGHT_GREEN));
                ui.label(session.label());
                if ui.button("Stop").clicked() {
                    stop = Some(index);
                }
            });
        }
        if let Some(index) = stop {
            let mut session = self.sessions.remove(index);
            session.stop();
            self.say(format!("Stopped {}.", session.label()), false);
        }
    }
}
