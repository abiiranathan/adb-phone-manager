//! Modal dialogs and management overlays.
#![allow(clippy::too_many_lines)]
#![allow(clippy::cast_precision_loss)]
use eframe::egui::{self, RichText};

use super::{apply, done, DeviceCtx, Extras};

impl Extras {
    pub fn ui_windows(&mut self, ctx: &egui::Context, dev: &DeviceCtx) {
        self.ui_app_details_dialog(ctx, dev);
        self.ui_power_dialog(ctx, dev);
        self.ui_new_folder_dialog(ctx, dev);
        self.ui_delete_remote_dialog(ctx, dev);
    }

    fn ui_app_details_dialog(&mut self, ctx: &egui::Context, dev: &DeviceCtx) {
        let Some(managed) = self.managed.clone() else {
            return;
        };
        let mut close = false;
        let mut clear_data = false;
        let mut extract = false;
        let mut enable = false;
        let mut disable = false;

        egui::Modal::new(egui::Id::new("app_details_modal")).show(ctx, |ui| {
            ui.set_max_width(500.0);
            ui.heading(format!("App: {}", managed.package));
            ui.separator();

            if managed.loading {
                ui.weak("Loading application details…");
            } else if let Some(d) = &managed.details {
                egui::Grid::new("details_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.strong("Version:");
                        ui.label(format!("{} ({})", d.version_name, d.version_code));
                        ui.end_row();

                        ui.strong("Target SDK:");
                        ui.label(format!("{} (min {})", d.target_sdk, d.min_sdk));
                        ui.end_row();

                        ui.strong("APK Size:");
                        ui.label(format!("{:.2} MB", d.apk_size as f64 / 1_048_576.0));
                        ui.end_row();

                        ui.strong("Path:");
                        ui.add(egui::Label::new(RichText::new(&d.apk_path).monospace()).truncate());
                        ui.end_row();

                        ui.strong("Data Dir:");
                        ui.add(egui::Label::new(RichText::new(&d.data_dir).monospace()).truncate());
                        ui.end_row();

                        ui.strong("Type:");
                        ui.label(if d.is_system {
                            "System App"
                        } else {
                            "User App"
                        });
                        ui.end_row();
                    });

                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Extract APK").clicked() {
                        extract = true;
                    }
                    if ui.button("Clear App Data").clicked() {
                        clear_data = true;
                    }
                    if ui.button("Disable").clicked() {
                        disable = true;
                    }
                    if ui.button("Enable").clicked() {
                        enable = true;
                    }
                });

                if !d.permissions.is_empty() {
                    ui.collapsing("Permissions", |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(120.0)
                            .show(ui, |ui| {
                                for p in &d.permissions {
                                    ui.weak(p);
                                }
                            });
                    });
                }
            }

            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });

        if close {
            self.managed = None;
        }
        if extract {
            let pkg = managed.package.clone();
            self.run(dev, move |adb| {
                let p = crate::package::extract_apk(&adb, &pkg)?;
                done(format!("Extracted to {}", p.display()))
            });
        }
        if clear_data {
            let pkg = managed.package.clone();
            self.run(dev, move |adb| {
                crate::package::clear_data(&adb, &pkg)?;
                done(format!("Cleared data for {pkg}"))
            });
        }
        if disable {
            let pkg = managed.package.clone();
            self.run(dev, move |adb| {
                crate::package::disable_app(&adb, &pkg)?;
                done(format!("Disabled {pkg}"))
            });
        }
        if enable {
            let pkg = managed.package;
            self.run(dev, move |adb| {
                crate::package::enable_app(&adb, &pkg)?;
                done(format!("Enabled {pkg}"))
            });
        }
    }

    fn ui_power_dialog(&mut self, ctx: &egui::Context, dev: &DeviceCtx) {
        let Some(action) = self.pending_power else {
            return;
        };
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Modal::new(egui::Id::new("power_confirm_modal")).show(ctx, |ui| {
            ui.set_max_width(340.0);
            ui.heading("Confirm Power Action");
            ui.add_space(6.0);
            ui.label(format!("Are you sure you want to: {}?", action.label()));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancelled = true;
                }
                if ui
                    .button(RichText::new("Confirm").color(ui.visuals().error_fg_color))
                    .clicked()
                {
                    confirmed = true;
                }
            });
        });

        if confirmed {
            self.pending_power = None;
            self.run(dev, move |adb| {
                action.execute(&adb)?;
                done("Power command executed.")
            });
        } else if cancelled {
            self.pending_power = None;
        }
    }

    fn ui_new_folder_dialog(&mut self, ctx: &egui::Context, dev: &DeviceCtx) {
        let Some((target_dir, mut name)) = self.new_folder.clone() else {
            return;
        };
        let mut create = false;
        let mut cancel = false;

        egui::Modal::new(egui::Id::new("new_folder_modal")).show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.heading("New Folder");
            ui.label(format!("Create in: {target_dir}"));
            ui.add(egui::TextEdit::singleline(&mut name).hint_text("Folder name"));
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui.button("Create").clicked() {
                    create = true;
                }
            });
        });

        if create {
            self.new_folder = None;
            self.run(dev, move |adb| {
                let path = crate::transfer::create_remote_dir(&adb, &target_dir, &name)?;
                apply(move |extras: &mut Self| {
                    extras.files_dirty = true;
                    extras.say(format!("Created {path}"), false);
                })
            });
        } else if cancel {
            self.new_folder = None;
        } else {
            self.new_folder = Some((target_dir, name));
        }
    }

    fn ui_delete_remote_dialog(&mut self, ctx: &egui::Context, dev: &DeviceCtx) {
        let Some((path, is_dir)) = self.pending_delete.clone() else {
            return;
        };
        let mut confirmed = false;
        let mut cancel = false;

        egui::Modal::new(egui::Id::new("delete_remote_modal")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.heading("Delete Confirmation");
            ui.label(format!("Permanently delete {path}?"));
            if is_dir {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    "Warning: all contents inside this directory will be removed!",
                );
            }
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
                if ui
                    .button(RichText::new("Delete").color(ui.visuals().error_fg_color))
                    .clicked()
                {
                    confirmed = true;
                }
            });
        });

        if confirmed {
            self.pending_delete = None;
            self.run(dev, move |adb| {
                crate::transfer::delete_remote(&adb, &path, is_dir)?;
                apply(move |extras: &mut Self| {
                    extras.files_dirty = true;
                    extras.say(format!("Deleted {path}"), false);
                })
            });
        } else if cancel {
            self.pending_delete = None;
        }
    }
}
