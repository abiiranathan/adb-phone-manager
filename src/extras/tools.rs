//! The Tools tab: wireless adb, APK install, file upload, data export and shell.

use eframe::egui::{self};

use super::{apply, done, DeviceCtx, ExportData, Extras};
use crate::export;
use crate::wireless;

impl Extras {
    pub fn ui_tools(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx, export: &ExportData) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.ui_wireless(ui, dev);
                ui.add_space(8.0);
                self.ui_install(ui, dev);
                ui.add_space(8.0);
                self.ui_upload(ui, dev);
                ui.add_space(8.0);
                self.ui_export(ui, export);
                ui.add_space(8.0);
                self.ui_shell(ui, dev);
            });
    }

    fn ui_wireless(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("Wireless ADB & Pairing", |ui| {
            ui.horizontal(|ui| {
                ui.label("Switch USB device to TCP/IP port:");
                ui.add(egui::DragValue::new(&mut self.tcpip_port));
                if ui
                    .add_enabled(dev.ready, egui::Button::new("Enable Wireless Mode"))
                    .clicked()
                {
                    let port = self.tcpip_port;
                    self.run(dev, move |adb| {
                        let msg = wireless::enable_tcpip(&adb, port)?;
                        done(msg)
                    });
                }
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Pair code & host:port:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.pair_address)
                        .hint_text("192.168.1.50:37123")
                        .desired_width(160.0),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.pair_code)
                        .hint_text("123456")
                        .desired_width(100.0),
                );
                if ui.button("Pair").clicked() {
                    let addr = self.pair_address.trim().to_owned();
                    let code = self.pair_code.trim().to_owned();
                    self.jobs.spawn(move || {
                        let msg = wireless::pair(&addr, &code)?;
                        done(msg)
                    });
                }
                if ui.button("Scan mDNS").clicked() {
                    self.jobs.spawn(|| {
                        let svcs = wireless::discover_mdns()?;
                        apply(move |extras: &mut Self| {
                            extras.say(format!("Found {} service(s).", svcs.len()), false);
                            extras.services = svcs;
                        })
                    });
                }
            });

            if !self.services.is_empty() {
                ui.label("Discovered wireless services:");
                for svc in &self.services {
                    ui.horizontal(|ui| {
                        ui.weak(&svc.name);
                        ui.label(format!("{}:{}", svc.host, svc.port));
                        if svc.is_pairing {
                            ui.colored_label(ui.visuals().error_fg_color, "(Pairing)");
                        }
                    });
                }
            }
        });
    }

    fn ui_install(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("APK Installer", |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.install_options.reinstall, "Reinstall (-r)");
                ui.checkbox(&mut self.install_options.allow_downgrade, "Downgrade (-d)");
                ui.checkbox(
                    &mut self.install_options.grant_permissions,
                    "Grant permissions (-g)",
                );
            });
            ui.horizontal(|ui| {
                if ui.button("Choose APK file(s)…").clicked() {
                    if let Some(files) = rfd::FileDialog::new()
                        .add_filter("APK package", &["apk"])
                        .pick_files()
                    {
                        self.install(dev, files);
                    }
                }
                ui.weak("or drag and drop an .apk file into this window");
            });
        });
    }

    fn ui_upload(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("Upload Files to Phone", |ui| {
            ui.horizontal(|ui| {
                ui.label("Target folder on phone:");
                ui.add(egui::TextEdit::singleline(&mut self.upload_dir).desired_width(300.0));
            });
            ui.horizontal(|ui| {
                if ui.button("Upload files…").clicked() {
                    if let Some(files) = rfd::FileDialog::new().pick_files() {
                        for f in files {
                            self.upload(dev, f);
                        }
                    }
                }
                ui.weak("or drag and drop files into this window");
            });
        });
    }

    fn ui_export(&mut self, ui: &mut egui::Ui, data: &ExportData) {
        ui.collapsing("Data Export", |ui| {
            ui.horizontal(|ui| {
                ui.label("Contacts:");
                if ui.button("Export vCard (.vcf)").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_file_name("contacts.vcf")
                        .save_file()
                    {
                        if let Err(e) = export::export_contacts_vcf(&data.contacts, &p) {
                            self.say(format!("Export failed: {e}"), true);
                        } else {
                            self.say(format!("Saved {}", p.display()), false);
                        }
                    }
                }
                if ui.button("Export CSV").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_file_name("contacts.csv")
                        .save_file()
                    {
                        if let Err(e) = export::export_contacts_csv(&data.contacts, &p) {
                            self.say(format!("Export failed: {e}"), true);
                        } else {
                            self.say(format!("Saved {}", p.display()), false);
                        }
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.label("Messages:");
                if ui.button("Export CSV").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_file_name("messages.csv")
                        .save_file()
                    {
                        if let Err(e) = export::export_messages_csv(&data.messages, &p) {
                            self.say(format!("Export failed: {e}"), true);
                        } else {
                            self.say(format!("Saved {}", p.display()), false);
                        }
                    }
                }
            });

            ui.horizontal(|ui| {
                ui.label("Call history:");
                if ui.button("Export CSV").clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .set_file_name("calls.csv")
                        .save_file()
                    {
                        if let Err(e) = export::export_calls_csv(&data.calls, &p) {
                            self.say(format!("Export failed: {e}"), true);
                        } else {
                            self.say(format!("Saved {}", p.display()), false);
                        }
                    }
                }
            });
        });
    }

    fn ui_shell(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        ui.collapsing("Interactive Shell", |ui| {
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.shell_input)
                        .hint_text("getprop, ls, pm list...")
                        .desired_width(400.0),
                );
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Execute").clicked() || enter) && !self.shell_input.trim().is_empty()
                {
                    let cmd = self.shell_input.trim().to_owned();
                    self.shell_history.push(format!("$ {cmd}"));
                    self.shell_input.clear();
                    self.run(dev, move |adb| {
                        let out = adb.shell(&cmd)?;
                        apply(move |extras: &mut Self| {
                            extras.shell_history.push(out.stdout);
                            if !out.stderr.is_empty() {
                                extras.shell_history.push(out.stderr);
                            }
                        })
                    });
                }
                if ui.button("Clear Output").clicked() {
                    self.shell_history.clear();
                }
            });

            let full_out = self.shell_history.join("\n");
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut full_out.as_str())
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                });
        });
    }
}
