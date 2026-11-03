//! The Info tab: device dashboard.

use std::time::{Duration, Instant};

use super::{apply, DeviceCtx, Extras};
use crate::device_info;
use eframe::egui;
use std::fmt::Write;

const REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const KIB_STEP: f64 = 1024.0;

fn format_kib(kib: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let mib = kib as f64 / KIB_STEP;
    if mib >= KIB_STEP {
        format!("{:.1} GB", mib / KIB_STEP)
    } else {
        format!("{mib:.0} MB")
    }
}

fn format_uptime(secs: u64) -> String {
    let (days, hours, minutes) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else {
        format!("{hours}h {minutes}m")
    }
}

#[allow(clippy::cast_possible_truncation)]
fn usage_bar(ui: &mut egui::Ui, used: u64, total: u64) {
    if total == 0 {
        return;
    }
    #[allow(clippy::cast_precision_loss)]
    let fraction = (used as f64 / total as f64).clamp(0.0, 1.0) as f32;
    ui.add(
        egui::ProgressBar::new(fraction)
            .desired_width(260.0)
            .text(format!("{} of {}", format_kib(used), format_kib(total))),
    );
}

impl Extras {
    pub fn ui_info(&mut self, ui: &mut egui::Ui, dev: &DeviceCtx) {
        let due = self
            .info_loaded_at
            .is_none_or(|at| self.info_auto && at.elapsed() >= REFRESH_INTERVAL);
        if dev.ready && !self.info_loading && due {
            self.refresh_info(dev);
        }
        if self.info_auto && dev.ready {
            ui.ctx().request_repaint_after(REFRESH_INTERVAL);
        }

        ui.horizontal(|ui| {
            ui.heading("Device");
            if ui
                .add_enabled(dev.ready, egui::Button::new("Refresh"))
                .clicked()
            {
                self.refresh_info(dev);
            }
            ui.checkbox(&mut self.info_auto, "Auto-refresh");
        });
        ui.separator();

        let Some(info) = &self.info else {
            ui.weak(if dev.ready {
                "Reading device information…"
            } else {
                "Select an authorised device."
            });
            return;
        };

        egui::Grid::new("info_grid")
            .num_columns(2)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                let mut row = |label: &str, value: String| {
                    ui.strong(label);
                    ui.add(egui::Label::new(value).selectable(true));
                    ui.end_row();
                };
                row("Model", format!("{} {}", info.manufacturer, info.model));
                row(
                    "Android",
                    format!(
                        "{} (API {}), patch {}",
                        info.android, info.sdk, info.security_patch
                    ),
                );
                row("CPU ABI", info.abi.clone());
                row(
                    "Screen",
                    format!("{} @ {} dpi", info.screen_size, info.screen_density),
                );
                if let Some(ip) = &info.wifi_ip {
                    row("Wi-Fi address", ip.clone());
                }
                if let Some(secs) = info.uptime_secs {
                    row("Uptime", format_uptime(secs));
                }
                let battery = &info.battery;
                let mut text = battery
                    .level
                    .map_or_else(|| "unknown".to_owned(), |l| format!("{l}%"));
                let _ = write!(text, ", {}", battery.status);

                if !battery.plugged.is_empty() {
                    let _ = write!(text, " ({})", battery.plugged);
                }
                if let Some(t) = battery.temperature_c {
                    let _ = write!(text, ", {t:.1} °C");
                }
                if let Some(mv) = battery.voltage_mv {
                    let _ = write!(text, ", {:.2} V", f64::from(mv) / 1000.0);
                }
                let _ = write!(text, ", health: {}", battery.health);
                row("Battery", text);
            });

        ui.add_space(8.0);
        if let Some(level) = info.battery.level {
            ui.add(
                egui::ProgressBar::new(f32::from(level) / 100.0)
                    .desired_width(260.0)
                    .text(format!("Battery {level}%")),
            );
        }
        if let (Some(total), Some(available)) = (info.mem_total_kb, info.mem_available_kb) {
            ui.label("Memory in use");
            usage_bar(ui, total.saturating_sub(available), total);
        }
        if let (Some(total), Some(free)) = (info.storage_total_kb, info.storage_free_kb) {
            ui.label("Storage in use");
            usage_bar(ui, total.saturating_sub(free), total);
        }
    }

    fn refresh_info(&mut self, dev: &DeviceCtx) {
        self.info_loading = true;
        self.info_loaded_at = Some(Instant::now());
        self.run(dev, |adb| {
            let info = device_info::collect(&adb);
            apply(move |extras: &mut Self| {
                extras.info = Some(info);
                extras.info_loading = false;
            })
        });
    }
}
