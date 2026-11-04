//! ADB Phone Manager: a front end for browsing and controlling an Android phone over adb.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adb;
mod capture;
mod controls;
mod device_info;
mod export;
mod extras;
mod icons;
mod jobs;
mod logcat;
mod package;
mod raster;
mod scrcpy;
mod transfer;
mod wireless;
mod worker;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};

use crate::adb::{
    join_remote, parent_dir, parse_text_segments, path_segments, CallRecord, CallState, CallType,
    Contact, Device, EntryKind, RemoteEntry, Sms, SmsKind, StorageRoot, TextSegment,
    UninstallOutcome,
};
use crate::extras::{DeviceCtx, ExportData, Extras};
use crate::worker::{Request, Response, Worker};

const WINDOW_SIZE: [f32; 2] = [1120.0, 780.0];
const CALL_POLL_INTERVAL: Duration = Duration::from_secs(3);
const DEFAULT_MESSAGE_LIMIT: usize = 200;
const DEFAULT_CALL_LIMIT: usize = 100;
const LIMIT_RANGE: std::ops::RangeInclusive<usize> = 10..=5000;
const DEVICE_READY_STATE: &str = "device";
const INPUT_WIDTH: f32 = 220.0;
const BYTE_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
const UNIT_STEP: f64 = 1024.0;
const ROW_HEIGHT: f32 = 32.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Calls,
    Messages,
    Contacts,
    Apps,
    Files,
    Mirror,
    Control,
    Info,
    Logcat,
    Tools,
}

impl Tab {
    const ALL: [Self; 10] = [
        Self::Calls,
        Self::Messages,
        Self::Contacts,
        Self::Apps,
        Self::Files,
        Self::Mirror,
        Self::Control,
        Self::Info,
        Self::Logcat,
        Self::Tools,
    ];

    const fn title(self) -> &'static str {
        match self {
            Self::Calls => "Calls",
            Self::Messages => "Messages",
            Self::Contacts => "Contacts",
            Self::Apps => "Apps",
            Self::Files => "Files",
            Self::Mirror => "Mirror",
            Self::Control => "Control",
            Self::Info => "Info",
            Self::Logcat => "Logcat",
            Self::Tools => "Tools",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Error,
}

enum AppAction {
    Launch(String),
    Stop(String),
    Uninstall(String),
    Details(String),
}

enum FileAction {
    Browse(String),
    Reload,
    Open(String),
    Download { path: String, is_dir: bool },
    CopyPath(String),
    NewFolder,
    Upload,
    Delete { path: String, is_dir: bool },
}

fn app_display_name(pkg: &str) -> String {
    let parts: Vec<&str> = pkg.split('.').collect();
    if parts.is_empty() {
        return pkg.to_owned();
    }
    let meaningful: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|&p| {
            !matches!(
                p,
                "com" | "org" | "net" | "io" | "android" | "google" | "app" | "apps"
            )
        })
        .collect();

    let chosen = if meaningful.is_empty() {
        parts.last().copied().unwrap_or(pkg)
    } else if meaningful.len() >= 2 {
        return meaningful
            .iter()
            .map(|s| capitalize(s))
            .collect::<Vec<_>>()
            .join(" ");
    } else {
        meaningful[0]
    };

    capitalize(chosen)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |first| {
        let mut res = first.to_uppercase().collect::<String>();
        res.extend(chars);
        res
    })
}

fn format_duration(secs: u64) -> String {
    if secs == 0 {
        return "0s".to_owned();
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64 / UNIT_STEP;
    let mut unit = 1;
    while value >= UNIT_STEP && unit < BYTE_UNITS.len() - 1 {
        value /= UNIT_STEP;
        unit += 1;
    }
    format!("{value:.1} {}", BYTE_UNITS[unit])
}

fn format_timestamp(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis).map_or_else(
        || "unknown time".to_owned(),
        |utc| {
            utc.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        },
    )
}

fn configure_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (
                egui::TextStyle::Small,
                egui::FontId::new(11.5, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Body,
                egui::FontId::new(14.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Monospace,
                egui::FontId::new(13.5, egui::FontFamily::Monospace),
            ),
            (
                egui::TextStyle::Button,
                egui::FontId::new(14.0, egui::FontFamily::Proportional),
            ),
            (
                egui::TextStyle::Heading,
                egui::FontId::new(20.0, egui::FontFamily::Proportional),
            ),
        ]
        .into();

        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size = egui::vec2(44.0, 28.0);
        style.spacing.combo_width = 190.0;
        style.spacing.window_margin = egui::Margin::same(8);
        style.spacing.menu_margin = egui::Margin::same(8);

        let visuals = &mut style.visuals;
        for widget in [
            &mut visuals.widgets.noninteractive,
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
            &mut visuals.widgets.open,
        ] {
            widget.corner_radius = egui::CornerRadius::same(4);
        }

        if visuals.dark_mode {
            let text = Color32::from_gray(235);
            visuals.widgets.noninteractive.fg_stroke.color = text;
            visuals.widgets.inactive.fg_stroke.color = text;
            visuals.widgets.hovered.fg_stroke.color = Color32::WHITE;
            visuals.widgets.active.fg_stroke.color = Color32::WHITE;
            visuals.widgets.open.fg_stroke.color = Color32::WHITE;
            visuals.widgets.noninteractive.bg_stroke.color = Color32::from_gray(78);

            visuals.panel_fill = Color32::from_gray(24);
            visuals.window_fill = Color32::from_gray(30);
            visuals.extreme_bg_color = Color32::from_gray(14);
            visuals.code_bg_color = Color32::from_gray(52);
            visuals.faint_bg_color = Color32::from_gray(38);

            visuals.hyperlink_color = Color32::from_rgb(110, 190, 255);
            visuals.error_fg_color = Color32::from_rgb(255, 130, 130);
            visuals.selection.bg_fill = Color32::from_rgb(0, 105, 152);
            visuals.selection.stroke = egui::Stroke::new(1.0_f32, Color32::WHITE);
        } else {
            visuals.widgets.noninteractive.fg_stroke.color = Color32::BLACK;
            visuals.widgets.inactive.fg_stroke.color = Color32::BLACK;
            visuals.widgets.hovered.fg_stroke.color = Color32::BLACK;
            visuals.widgets.active.fg_stroke.color = Color32::BLACK;
            visuals.widgets.open.fg_stroke.color = Color32::BLACK;
            visuals.widgets.noninteractive.bg_stroke.color = Color32::from_gray(175);

            visuals.panel_fill = Color32::from_gray(250);
            visuals.window_fill = Color32::from_gray(248);
            visuals.extreme_bg_color = Color32::WHITE;
            visuals.code_bg_color = Color32::from_gray(232);
            visuals.faint_bg_color = Color32::from_gray(238);

            visuals.hyperlink_color = Color32::from_rgb(0, 90, 200);
            visuals.error_fg_color = Color32::from_rgb(190, 20, 20);
            visuals.selection.bg_fill = Color32::from_rgb(160, 210, 250);
            visuals.selection.stroke = egui::Stroke::new(1.0_f32, Color32::BLACK);
        }
    });
}

fn row_button(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::new(label).min_size(egui::vec2(0.0, 22.0)))
}

fn caller_label(ui: &egui::Ui, name: &str, number: &str) -> egui::text::LayoutJob {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::default();
    job.append(
        name,
        0.0,
        egui::TextFormat {
            font_id: font_id.clone(),
            color: ui.visuals().strong_text_color(),
            ..Default::default()
        },
    );
    job.append(
        &format!(" ({number})"),
        0.0,
        egui::TextFormat {
            font_id,
            color: ui.visuals().weak_text_color(),
            ..Default::default()
        },
    );
    job
}

fn message_preview(body: &str) -> String {
    const MAX_CHARS: usize = 180;

    let mut preview = String::new();
    for (count, ch) in body.chars().enumerate() {
        if count >= MAX_CHARS {
            preview.push('…');
            break;
        }
        if ch.is_whitespace() {
            if !preview.is_empty() && !preview.ends_with(' ') {
                preview.push(' ');
            }
        } else {
            preview.push(ch);
        }
    }
    let preview = preview.trim();
    if preview.is_empty() {
        "—".to_owned()
    } else {
        preview.to_owned()
    }
}

fn ui_message_body(ui: &mut egui::Ui, body: &str) {
    for line in body.split('\n') {
        if line.is_empty() {
            ui.add_space(4.0);
            continue;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for segment in parse_text_segments(line) {
                match segment {
                    TextSegment::Text(text) => {
                        ui.add(egui::Label::new(text).selectable(true));
                    }
                    TextSegment::Url(url) => {
                        ui.hyperlink(url);
                    }
                }
            }
        });
    }
}

struct PhoneApp {
    worker: Worker,
    extras: Extras,
    app_icon: Option<egui::TextureHandle>,
    devices: Vec<Device>,
    selected: Option<String>,
    tab: Tab,
    contacts: Vec<Contact>,
    messages: Vec<Sms>,
    message_selection: HashSet<i64>,
    pending_message_delete: Option<Vec<i64>>,
    view_message: Option<Sms>,
    call_logs: Vec<CallRecord>,
    apps: Vec<String>,
    storage_roots: Vec<StorageRoot>,
    files_path: Option<String>,
    files_requested: Option<String>,
    entries: Vec<RemoteEntry>,
    call_state: CallState,
    contact_filter: String,
    message_filter: String,
    call_filter: String,
    app_filter: String,
    file_filter: String,
    connect_input: String,
    dial_input: String,
    message_limit: usize,
    call_limit: usize,
    third_party_only: bool,
    pending_uninstall: Option<String>,
    show_hidden: bool,
    status: String,
    status_kind: StatusKind,
    pending: usize,
    poll_inflight: bool,
    last_poll: Instant,
}

impl PhoneApp {
    fn new(ctx: &egui::Context) -> Self {
        configure_style(ctx);
        let app_icon = app_icon().map(|icon| {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            ctx.load_texture("app_icon", image, egui::TextureOptions::LINEAR)
        });
        Self {
            worker: Worker::new(ctx.clone()),
            extras: Extras::new(ctx),
            app_icon,
            devices: Vec::new(),
            selected: None,
            tab: Tab::Calls,
            contacts: Vec::new(),
            messages: Vec::new(),
            message_selection: HashSet::new(),
            pending_message_delete: None,
            view_message: None,
            call_logs: Vec::new(),
            apps: Vec::new(),
            storage_roots: Vec::new(),
            files_path: None,
            files_requested: None,
            entries: Vec::new(),
            call_state: CallState::Unknown,
            contact_filter: String::new(),
            message_filter: String::new(),
            call_filter: String::new(),
            app_filter: String::new(),
            file_filter: String::new(),
            connect_input: String::new(),
            dial_input: String::new(),
            message_limit: DEFAULT_MESSAGE_LIMIT,
            call_limit: DEFAULT_CALL_LIMIT,
            third_party_only: true,
            pending_uninstall: None,
            show_hidden: false,
            status: "Looking for devices...".to_owned(),
            status_kind: StatusKind::Info,
            pending: 0,
            poll_inflight: false,
            last_poll: Instant::now(),
        }
    }

    fn ready(&self) -> bool {
        self.selected.as_ref().is_some_and(|serial| {
            self.devices
                .iter()
                .any(|d| &d.serial == serial && d.state == DEVICE_READY_STATE)
        })
    }

    fn export_data(&self) -> ExportData {
        ExportData {
            contacts: self.contacts.clone(),
            messages: self.messages.clone(),
            calls: self.call_logs.clone(),
        }
    }

    fn set_status(&mut self, text: impl Into<String>, is_error: bool) {
        self.status = text.into();
        self.status_kind = if is_error {
            StatusKind::Error
        } else {
            StatusKind::Info
        };
    }

    fn submit(&mut self, request: Request) {
        if request.needs_device() && !self.ready() {
            self.set_status("No authorised device selected.", true);
            return;
        }
        self.pending += 1;
        self.worker.submit(self.selected.clone(), request);
    }

    fn select(&mut self, serial: Option<String>) {
        if self.selected == serial {
            return;
        }
        self.selected = serial;
        self.poll_inflight = false;
        self.contacts.clear();
        self.messages.clear();
        self.message_selection.clear();
        self.pending_message_delete = None;
        self.view_message = None;
        self.call_logs.clear();
        self.apps.clear();
        self.storage_roots.clear();
        self.entries.clear();
        self.files_path = None;
        self.files_requested = None;
        self.pending_uninstall = None;
        self.call_state = CallState::Unknown;
    }

    fn apply_devices(&mut self, devices: Vec<Device>) {
        self.extras.on_devices_changed(&devices);

        let still_present = self
            .selected
            .as_ref()
            .is_some_and(|serial| devices.iter().any(|d| &d.serial == serial));
        if !still_present {
            let first_ready = devices
                .iter()
                .find(|d| d.state == DEVICE_READY_STATE)
                .map(|d| d.serial.clone());
            self.select(first_ready);
        }
        if devices.is_empty() {
            self.set_status(
                "No devices found. Enable USB debugging, plug the phone in and accept the RSA prompt.",
                true,
            );
        } else {
            let ready = devices
                .iter()
                .filter(|d| d.state == DEVICE_READY_STATE)
                .count();
            if ready == 0 {
                self.set_status(
                    format!(
                        "{} device(s) found, but none is authorised. Accept the USB debugging prompt on the phone.",
                        devices.len()
                    ),
                    true,
                );
            } else {
                self.set_status(format!("{ready} ready device(s)."), false);
            }
        }
        self.devices = devices;
        if self.ready() {
            match self.tab {
                Tab::Calls if self.call_logs.is_empty() => self.load_call_logs(),
                Tab::Messages if self.messages.is_empty() => self.load_messages(),
                Tab::Contacts if self.contacts.is_empty() => {
                    self.submit(Request::LoadContacts);
                }
                Tab::Apps if self.apps.is_empty() => self.load_apps(),
                Tab::Files if self.storage_roots.is_empty() => {
                    self.submit(Request::LoadStorage);
                }
                _ => {}
            }
        }
    }

    fn drain_responses(&mut self) {
        while let Some(response) = self.worker.try_recv() {
            if !response.is_background() {
                self.pending = self.pending.saturating_sub(1);
            }
            match response {
                Response::Devices(devices) | Response::DevicesChanged(devices) => {
                    self.apply_devices(devices);
                }
                Response::Connected(message) => {
                    self.set_status(message, false);
                    self.submit(Request::RefreshDevices);
                }
                Response::Contacts(contacts) => {
                    self.set_status(format!("Loaded {} contacts.", contacts.len()), false);
                    self.contacts = contacts;
                }
                Response::Messages(messages) => {
                    self.set_status(format!("Loaded {} messages.", messages.len()), false);
                    self.messages = messages;
                    self.message_selection.clear();
                }
                Response::DeletedMessages { ids } => {
                    let count = ids.len();
                    self.messages.retain(|message| !ids.contains(&message.id));
                    self.message_selection.retain(|id| !ids.contains(id));
                    if self
                        .view_message
                        .as_ref()
                        .is_some_and(|message| ids.contains(&message.id))
                    {
                        self.view_message = None;
                    }
                    self.set_status(format!("Deleted {count} message(s) from the phone."), false);
                }
                Response::CallLogs(logs) => {
                    self.set_status(format!("Loaded {} call log(s).", logs.len()), false);
                    self.call_logs = logs;
                }
                Response::Apps(apps) => {
                    self.set_status(format!("Loaded {} apps.", apps.len()), false);
                    self.apps = apps;
                }
                Response::Uninstalled { package, outcome } => {
                    let note = match outcome {
                        UninstallOutcome::Removed => String::new(),
                        UninstallOutcome::RemovedForUser => {
                            " for the primary user (system app)".to_owned()
                        }
                    };
                    self.set_status(format!("Uninstalled {package}{note}."), false);
                    self.apps.retain(|installed| installed != &package);
                }
                Response::Storage(roots) => {
                    self.set_status(format!("Found {} storage volume(s).", roots.len()), false);
                    let first = roots.first().map(|root| root.path.clone());
                    self.storage_roots = roots;
                    if self.files_path.is_none() && self.files_requested.is_none() {
                        if let Some(path) = first {
                            self.browse(&path);
                        }
                    }
                }
                Response::Dir { path, entries } => {
                    if self.files_requested.as_deref() == Some(path.as_str()) {
                        self.set_status(format!("{} item(s) in {path}.", entries.len()), false);
                        self.files_requested = None;
                        self.files_path = Some(path);
                        self.entries = entries;
                    }
                }
                Response::Done(message) => self.set_status(message, false),
                Response::Failed(message) => self.set_status(format!("Error: {message}"), true),
                Response::CallState(state) => {
                    self.call_state = state;
                    self.poll_inflight = false;
                }
            }
        }
    }

    fn poll_call_state(&mut self, ctx: &egui::Context) {
        if self.tab != Tab::Calls || !self.ready() || self.poll_inflight {
            return;
        }

        let elapsed = self.last_poll.elapsed();
        if elapsed >= CALL_POLL_INTERVAL {
            self.poll_inflight = true;
            self.last_poll = Instant::now();
            self.worker
                .submit(self.selected.clone(), Request::PollCallState);
        } else {
            ctx.request_repaint_after(CALL_POLL_INTERVAL.saturating_sub(elapsed));
        }
    }

    fn load_messages(&mut self) {
        self.submit(Request::LoadMessages {
            limit: self.message_limit,
        });
    }

    fn load_call_logs(&mut self) {
        self.submit(Request::LoadCallLogs {
            limit: self.call_limit,
        });
    }

    fn load_apps(&mut self) {
        self.submit(Request::LoadApps {
            third_party_only: self.third_party_only,
        });
    }

    fn browse(&mut self, path: &str) {
        self.files_requested = Some(path.to_owned());
        self.submit(Request::ListDir {
            path: path.to_owned(),
        });
    }

    fn root_for(&self, path: &str) -> Option<&StorageRoot> {
        self.storage_roots
            .iter()
            .filter(|root| {
                let base = root.path.trim_end_matches('/');
                path == base
                    || path
                        .strip_prefix(base)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|root| root.path.len())
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        if !self.ready() {
            return;
        }
        match tab {
            Tab::Calls if self.call_logs.is_empty() => self.load_call_logs(),
            Tab::Messages if self.messages.is_empty() => self.load_messages(),
            Tab::Contacts if self.contacts.is_empty() => self.submit(Request::LoadContacts),
            Tab::Apps if self.apps.is_empty() => self.load_apps(),
            Tab::Files if self.storage_roots.is_empty() => self.submit(Request::LoadStorage),
            _ => {}
        }
    }

    fn selected_label(&self) -> String {
        self.selected
            .as_ref()
            .and_then(|serial| self.devices.iter().find(|d| &d.serial == serial))
            .map_or_else(|| "No device".to_owned(), Device::label)
    }

    fn ui_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(icon) = &self.app_icon {
                ui.add(
                    egui::Image::new(icon)
                        .fit_to_exact_size(egui::vec2(24.0, 24.0))
                        .corner_radius(6.0),
                );
                ui.separator();
            }
            ui.label("Device:");
            let mut chosen: Option<String> = None;
            egui::ComboBox::from_id_salt("device_picker")
                .selected_text(self.selected_label())
                .show_ui(ui, |ui| {
                    for device in &self.devices {
                        let is_selected = self.selected.as_deref() == Some(device.serial.as_str());
                        if ui.selectable_label(is_selected, device.label()).clicked() {
                            chosen = Some(device.serial.clone());
                        }
                    }
                });
            if let Some(serial) = chosen {
                self.select(Some(serial));
            }
            if ui.button("Refresh").clicked() {
                self.submit(Request::RefreshDevices);
            }

            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut self.connect_input)
                    .hint_text("192.168.1.20:5555")
                    .desired_width(INPUT_WIDTH),
            );
            if ui.button("Connect").clicked() {
                let address = self.connect_input.trim().to_owned();
                if address.is_empty() {
                    self.set_status("Enter a host:port to connect to.", true);
                } else {
                    self.submit(Request::Connect(address));
                }
            }
            if self.pending > 0 || self.extras.busy() {
                ui.weak("Working…");
            }
        });
    }

    #[allow(clippy::too_many_lines)]
    fn ui_calls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Telephony Control");
            let (text, color) = match self.call_state {
                CallState::Ringing => ("Incoming call ringing", Color32::LIGHT_GREEN),
                CallState::Offhook => ("Call in progress", Color32::YELLOW),
                CallState::Idle => ("Idle", Color32::GRAY),
                CallState::Unknown => ("State unknown", Color32::GRAY),
            };
            ui.label(RichText::new(format!("• {text}")).color(color).strong());
            if self.call_state == CallState::Ringing && ui.button("Answer").clicked() {
                self.submit(Request::AnswerCall);
            }
            if (self.call_state == CallState::Ringing || self.call_state == CallState::Offhook)
                && ui.button("Reject / Hang up").clicked()
            {
                self.submit(Request::EndCall);
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.dial_input)
                    .hint_text("+256700123456")
                    .desired_width(INPUT_WIDTH),
            );
            let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("📞 Dial").clicked() || enter {
                let number = self.dial_input.trim().to_owned();
                self.submit(Request::Dial(number));
            }
        });

        ui.add_space(8.0);
        ui.separator();

        ui.horizontal(|ui| {
            ui.heading("Call History");
            if ui.button("Reload").clicked() {
                self.load_call_logs();
            }
            ui.label("Limit:");
            if ui
                .add(
                    egui::DragValue::new(&mut self.call_limit)
                        .range(LIMIT_RANGE)
                        .speed(10),
                )
                .changed()
            {
                self.load_call_logs();
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.call_filter)
                    .hint_text("Filter by name or number")
                    .desired_width(INPUT_WIDTH),
            );
            ui.label(format!("{} record(s)", self.call_logs.len()));
        });
        ui.separator();

        let needle = self.call_filter.to_lowercase();
        let visible: Vec<&CallRecord> = self
            .call_logs
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.name.to_lowercase().contains(&needle)
                    || c.number.contains(&needle)
            })
            .collect();

        let mut dial_number: Option<String> = None;
        let mut copy_text: Option<String> = None;

        TableBuilder::new(ui)
            .id_salt("calls_table")
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(Column::initial(100.0).at_least(85.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::initial(150.0).at_least(130.0))
            .column(Column::initial(90.0).at_least(75.0))
            .column(Column::initial(190.0).at_least(170.0))
            .header(26.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Type");
                });
                header.col(|ui| {
                    ui.strong("Caller / Number");
                });
                header.col(|ui| {
                    ui.strong("Date");
                });
                header.col(|ui| {
                    ui.strong("Duration");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, visible.len(), |mut row| {
                    let call = visible[row.index()];
                    row.col(|ui| {
                        let (type_color, type_label) = match call.call_type {
                            CallType::Incoming => (Color32::LIGHT_GREEN, call.call_type.label()),
                            CallType::Outgoing => {
                                (Color32::from_rgb(100, 180, 255), call.call_type.label())
                            }
                            CallType::Missed | CallType::Rejected | CallType::Blocked => {
                                (Color32::LIGHT_RED, call.call_type.label())
                            }
                            CallType::Voicemail | CallType::Other => {
                                (Color32::GRAY, call.call_type.label())
                            }
                        };
                        ui.label(RichText::new(type_label).color(type_color).strong());
                    });
                    row.col(|ui| {
                        if call.name.is_empty() {
                            ui.add(
                                egui::Label::new(RichText::new(&call.number).monospace())
                                    .selectable(true)
                                    .truncate(),
                            );
                        } else {
                            ui.add(
                                egui::Label::new(caller_label(ui, &call.name, &call.number))
                                    .selectable(true)
                                    .truncate(),
                            );
                        }
                    });
                    row.col(|ui| {
                        ui.label(format_timestamp(call.date_ms));
                    });
                    row.col(|ui| {
                        ui.label(format_duration(call.duration_secs));
                    });
                    row.col(|ui| {
                        if !call.number.is_empty() {
                            if row_button(ui, "📞 Call")
                                .on_hover_text("Call this number")
                                .clicked()
                            {
                                dial_number = Some(call.number.clone());
                            }
                            if row_button(ui, "📋 Copy")
                                .on_hover_text("Copy number")
                                .clicked()
                            {
                                copy_text = Some(call.number.clone());
                            }
                        }
                    });
                });
            });

        if let Some(number) = dial_number {
            self.dial_input.clone_from(&number);
            self.submit(Request::Dial(number));
        }
        if let Some(text) = copy_text {
            ui.ctx().copy_text(text.clone());
            self.set_status(format!("Copied {text} to clipboard."), false);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn ui_messages(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Reload").clicked() {
                self.load_messages();
            }
            ui.label("Limit:");
            if ui
                .add(
                    egui::DragValue::new(&mut self.message_limit)
                        .range(LIMIT_RANGE)
                        .speed(10),
                )
                .changed()
            {
                self.load_messages();
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.message_filter)
                    .hint_text("Filter by number or text")
                    .desired_width(INPUT_WIDTH),
            );
            ui.label(format!("{} message(s)", self.messages.len()));
        });
        ui.separator();

        let needle = self.message_filter.to_lowercase();
        let visible: Vec<&Sms> = self
            .messages
            .iter()
            .filter(|m| {
                needle.is_empty()
                    || m.address.to_lowercase().contains(&needle)
                    || m.body.to_lowercase().contains(&needle)
            })
            .collect();

        let selected_count = self.message_selection.len();
        let mut select_all_shown = false;
        let mut clear_selection = false;
        let mut delete_selected = false;
        ui.horizontal(|ui| {
            if ui.button("Select all shown").clicked() {
                select_all_shown = true;
            }
            if ui
                .add_enabled(selected_count > 0, egui::Button::new("Clear selection"))
                .clicked()
            {
                clear_selection = true;
            }
            if ui
                .add_enabled(
                    selected_count > 0,
                    egui::Button::new(
                        RichText::new(format!("🗑 Delete selected ({selected_count})"))
                            .color(ui.visuals().error_fg_color),
                    ),
                )
                .on_hover_text("Delete the checked messages from the phone")
                .clicked()
            {
                delete_selected = true;
            }
            if selected_count > 0 {
                ui.weak(format!("{selected_count} selected"));
            }
        });
        if select_all_shown {
            self.message_selection
                .extend(visible.iter().map(|message| message.id));
        }
        if clear_selection {
            self.message_selection.clear();
        }
        if delete_selected {
            self.pending_message_delete = Some(self.message_selection.iter().copied().collect());
        }
        ui.separator();

        let selected = &self.message_selection;
        let mut selection_changes: Vec<(i64, bool)> = Vec::new();
        let mut dial_number: Option<String> = None;
        let mut copy_number: Option<String> = None;
        let mut view_sms: Option<Sms> = None;

        TableBuilder::new(ui)
            .id_salt("messages_table")
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(Column::initial(28.0).at_least(24.0))
            .column(Column::initial(55.0).at_least(50.0))
            .column(Column::initial(140.0).at_least(110.0))
            .column(Column::initial(140.0).at_least(120.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::initial(250.0).at_least(230.0))
            .header(26.0, |mut header| {
                header.col(|ui| {
                    ui.strong("");
                });
                header.col(|ui| {
                    ui.strong("Dir");
                });
                header.col(|ui| {
                    ui.strong("Number");
                });
                header.col(|ui| {
                    ui.strong("Date");
                });
                header.col(|ui| {
                    ui.strong("Message");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, visible.len(), |mut row| {
                    let sms = visible[row.index()];
                    row.col(|ui| {
                        let mut checked = selected.contains(&sms.id);
                        if ui.checkbox(&mut checked, "").changed() {
                            selection_changes.push((sms.id, checked));
                        }
                    });
                    row.col(|ui| {
                        let (kind_color, kind_text) = match sms.kind {
                            SmsKind::Received => (Color32::LIGHT_GREEN, "IN"),
                            SmsKind::Sent => (Color32::from_rgb(100, 180, 255), "OUT"),
                            SmsKind::Other => (Color32::GRAY, "---"),
                        };
                        ui.label(
                            RichText::new(kind_text)
                                .color(kind_color)
                                .monospace()
                                .strong(),
                        );
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&sms.address).monospace())
                                .selectable(true)
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.label(format_timestamp(sms.date_ms));
                    });
                    row.col(|ui| {
                        if ui
                            .add(
                                egui::Label::new(message_preview(&sms.body))
                                    .truncate()
                                    .sense(egui::Sense::click()),
                            )
                            .on_hover_text("Open the full message")
                            .clicked()
                        {
                            view_sms = Some(sms.clone());
                        }
                    });
                    row.col(|ui| {
                        if row_button(ui, "📞 Call")
                            .on_hover_text("Call this number")
                            .clicked()
                        {
                            dial_number = Some(sms.address.clone());
                        }
                        if row_button(ui, "📋 Copy")
                            .on_hover_text("Copy this number")
                            .clicked()
                        {
                            copy_number = Some(sms.address.clone());
                        }
                        if row_button(ui, "🔍 View")
                            .on_hover_text("Open the full message")
                            .clicked()
                        {
                            view_sms = Some(sms.clone());
                        }
                    });
                });
            });

        for (id, checked) in selection_changes {
            if checked {
                self.message_selection.insert(id);
            } else {
                self.message_selection.remove(&id);
            }
        }

        if let Some(number) = dial_number {
            self.dial_input.clone_from(&number);
            self.submit(Request::Dial(number));
        }
        if let Some(number) = copy_number {
            ui.ctx().copy_text(number.clone());
            self.set_status(format!("Copied {number} to clipboard."), false);
        }
        if let Some(sms) = view_sms {
            self.view_message = Some(sms);
        }
    }

    fn ui_message_dialog(&mut self, ctx: &egui::Context) {
        let Some(sms) = self.view_message.clone() else {
            return;
        };

        let mut close = false;
        let mut copy_text = false;
        let mut copy_number = false;
        let mut dial_number: Option<String> = None;

        egui::Modal::new(egui::Id::new("message-dialog")).show(ctx, |ui| {
            ui.set_max_width(620.0);
            ui.heading("Message");
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let (kind_color, kind_text) = match sms.kind {
                    SmsKind::Received => (Color32::LIGHT_GREEN, "Received"),
                    SmsKind::Sent => (Color32::from_rgb(100, 180, 255), "Sent"),
                    SmsKind::Other => (Color32::GRAY, "Other"),
                };
                ui.colored_label(kind_color, kind_text);
                ui.add(
                    egui::Label::new(RichText::new(&sms.address).monospace().strong())
                        .selectable(true),
                );
                ui.weak(format_timestamp(sms.date_ms));
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(320.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui_message_body(ui, &sms.body);
                });
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("📞 Call").clicked() {
                    dial_number = Some(sms.address.clone());
                }
                if ui.button("📋 Copy number").clicked() {
                    copy_number = true;
                }
                if ui.button("📋 Copy text").clicked() {
                    copy_text = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        });

        if close {
            self.view_message = None;
        }
        if copy_text {
            ctx.copy_text(sms.body.clone());
            self.set_status("Copied message text to clipboard.", false);
        }
        if copy_number {
            ctx.copy_text(sms.address.clone());
            self.set_status(format!("Copied {} to clipboard.", sms.address), false);
        }
        if let Some(number) = dial_number {
            self.dial_input.clone_from(&number);
            self.submit(Request::Dial(number));
        }
    }

    fn ui_message_delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(ids) = self.pending_message_delete.clone() else {
            return;
        };

        let mut confirmed = false;
        let mut cancelled = false;

        let modal = egui::Modal::new(egui::Id::new("message-delete-dialog")).show(ctx, |ui| {
            ui.set_max_width(430.0);
            ui.heading("Delete messages?");
            ui.add_space(6.0);
            ui.label(format!(
                "Delete {} selected message(s) from the phone?",
                ids.len()
            ));
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().error_fg_color, "This cannot be undone.");
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancelled = true;
                }
                if ui
                    .add(egui::Button::new(
                        RichText::new("🗑 Delete").color(ui.visuals().error_fg_color),
                    ))
                    .clicked()
                {
                    confirmed = true;
                }
            });
        });

        if confirmed {
            self.pending_message_delete = None;
            self.set_status(format!("Deleting {} message(s)…", ids.len()), false);
            self.submit(Request::DeleteMessages(ids));
        } else if cancelled || modal.should_close() {
            self.pending_message_delete = None;
        }
    }

    fn ui_contacts(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Reload").clicked() {
                self.submit(Request::LoadContacts);
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.contact_filter)
                    .hint_text("Filter by name or number")
                    .desired_width(INPUT_WIDTH),
            );
            ui.label(format!("{} contact(s)", self.contacts.len()));
        });
        ui.separator();

        let needle = self.contact_filter.to_lowercase();
        let visible: Vec<&Contact> = self
            .contacts
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.name.to_lowercase().contains(&needle)
                    || c.number.contains(&needle)
            })
            .collect();

        let mut dial_number: Option<String> = None;
        let mut copy_text: Option<String> = None;

        TableBuilder::new(ui)
            .id_salt("contacts_table")
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(Column::remainder().at_least(180.0).clip(true))
            .column(Column::initial(190.0).at_least(160.0))
            .column(Column::initial(190.0).at_least(170.0))
            .header(26.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Name");
                });
                header.col(|ui| {
                    ui.strong("Phone Number");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, visible.len(), |mut row| {
                    let contact = visible[row.index()];
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&contact.name).strong())
                                .selectable(true)
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&contact.number).monospace())
                                .selectable(true)
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        if row_button(ui, "📞 Call")
                            .on_hover_text("Dial this number")
                            .clicked()
                        {
                            dial_number = Some(contact.number.clone());
                        }
                        if row_button(ui, "📋 Copy")
                            .on_hover_text("Copy phone number")
                            .clicked()
                        {
                            copy_text = Some(contact.number.clone());
                        }
                    });
                });
            });

        if let Some(number) = dial_number {
            self.dial_input.clone_from(&number);
            self.submit(Request::Dial(number));
        }
        if let Some(text) = copy_text {
            ui.ctx().copy_text(text.clone());
            self.set_status(format!("Copied {text} to clipboard."), false);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn ui_apps(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Reload").clicked() {
                self.load_apps();
            }
            if ui
                .checkbox(&mut self.third_party_only, "Third-party only")
                .on_hover_text(
                    "Uncheck to list system apps. They can be uninstalled for the primary user.",
                )
                .changed()
            {
                self.load_apps();
            }
            ui.checkbox(&mut self.extras.load_icons, "Icons");
            ui.add(
                egui::TextEdit::singleline(&mut self.app_filter)
                    .hint_text("Filter apps")
                    .desired_width(INPUT_WIDTH),
            );
            ui.label(format!("{} app(s)", self.apps.len()));
        });
        ui.separator();

        let needle = self.app_filter.to_lowercase();
        let visible: Vec<(&String, String)> = self
            .apps
            .iter()
            .map(|p| (p, app_display_name(p)))
            .filter(|(pkg, name)| {
                needle.is_empty()
                    || pkg.to_lowercase().contains(&needle)
                    || name.to_lowercase().contains(&needle)
            })
            .collect();

        let mut action: Option<AppAction> = None;
        let mut copy_pkg: Option<String> = None;
        let selected = self.selected.clone();
        let ready = self.ready();
        let dev = DeviceCtx {
            serial: selected.as_deref(),
            ready,
        };

        TableBuilder::new(ui)
            .id_salt("apps_table")
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(Column::initial(220.0).at_least(180.0).clip(true))
            .column(Column::remainder().at_least(220.0).clip(true))
            .column(Column::initial(430.0).at_least(400.0))
            .header(26.0, |mut header| {
                header.col(|ui| {
                    ui.strong("App Name");
                });
                header.col(|ui| {
                    ui.strong("Package ID");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, visible.len(), |mut row| {
                    let (package, name) = &visible[row.index()];
                    row.col(|ui| {
                        if let Some(tex) = self.extras.icon_for(package, &dev) {
                            ui.image((tex.id(), egui::vec2(20.0, 20.0)));
                        }
                        ui.add(
                            egui::Label::new(RichText::new(name).strong())
                                .selectable(true)
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(package.as_str()).monospace())
                                .selectable(true)
                                .truncate(),
                        );
                    });
                    row.col(|ui| {
                        if row_button(ui, "▶ Launch")
                            .on_hover_text("Launch app")
                            .clicked()
                        {
                            action = Some(AppAction::Launch((*package).clone()));
                        }
                        if row_button(ui, "⏹ Stop")
                            .on_hover_text("Force stop app")
                            .clicked()
                        {
                            action = Some(AppAction::Stop((*package).clone()));
                        }
                        if row_button(ui, "ℹ Details")
                            .on_hover_text("View app details and permissions")
                            .clicked()
                        {
                            action = Some(AppAction::Details((*package).clone()));
                        }
                        if row_button(
                            ui,
                            RichText::new("🗑 Uninstall").color(ui.visuals().error_fg_color),
                        )
                        .on_hover_text(
                            "Uninstall this app (system apps are removed for the primary user)",
                        )
                        .clicked()
                        {
                            action = Some(AppAction::Uninstall((*package).clone()));
                        }
                        if row_button(ui, "📋 Copy")
                            .on_hover_text("Copy package name")
                            .clicked()
                        {
                            copy_pkg = Some((*package).clone());
                        }
                    });
                });
            });

        match action {
            Some(AppAction::Launch(package)) => self.submit(Request::Launch(package)),
            Some(AppAction::Stop(package)) => self.submit(Request::ForceStop(package)),
            Some(AppAction::Details(package)) => {
                let selected = self.selected.clone();
                let ready = self.ready();
                let dev = DeviceCtx {
                    serial: selected.as_deref(),
                    ready,
                };
                self.extras.open_app_details(package, &dev);
            }
            Some(AppAction::Uninstall(package)) => self.pending_uninstall = Some(package),
            None => {}
        }
        if let Some(pkg) = copy_pkg {
            ui.ctx().copy_text(pkg.clone());
            self.set_status(format!("Copied {pkg} to clipboard."), false);
        }
    }

    fn ui_uninstall_dialog(&mut self, ctx: &egui::Context) {
        let Some(package) = self.pending_uninstall.clone() else {
            return;
        };

        let mut confirmed = false;
        let mut cancelled = false;

        let modal = egui::Modal::new(egui::Id::new("uninstall-dialog")).show(ctx, |ui| {
            ui.set_max_width(430.0);
            ui.heading("Uninstall app?");
            ui.add_space(6.0);
            ui.label(RichText::new(app_display_name(&package)).strong());
            ui.add(egui::Label::new(RichText::new(&package).monospace()).selectable(true));
            ui.add_space(10.0);
            ui.label(
                "User apps are removed completely. System apps are removed for the primary user \
                 only: the system image keeps them and a factory reset brings them back.",
            );
            ui.add_space(6.0);
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Removing essential system apps can stop the phone from working. Only uninstall \
                 apps you recognise.",
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    cancelled = true;
                }
                if ui
                    .add(egui::Button::new(
                        RichText::new("🗑 Uninstall").color(ui.visuals().error_fg_color),
                    ))
                    .clicked()
                {
                    confirmed = true;
                }
            });
        });

        if confirmed {
            self.pending_uninstall = None;
            self.set_status(format!("Uninstalling {package}..."), false);
            self.submit(Request::Uninstall(package));
        } else if cancelled || modal.should_close() {
            self.pending_uninstall = None;
        }
    }

    #[allow(clippy::too_many_lines)]
    fn ui_files(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<FileAction> = None;

        let current_dir = self.files_path.clone();
        let root = current_dir
            .as_deref()
            .and_then(|dir| self.root_for(dir))
            .cloned();
        let at_root = match (&current_dir, &root) {
            (Some(dir), Some(root)) => dir.trim_end_matches('/') == root.path.trim_end_matches('/'),
            _ => false,
        };

        ui.horizontal(|ui| {
            ui.label("Storage:");
            let selected_text = root.as_ref().map_or("Select storage", |r| r.label.as_str());
            egui::ComboBox::from_id_salt("storage_picker")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for volume in &self.storage_roots {
                        let is_selected = root.as_ref().is_some_and(|r| r.path == volume.path);
                        if ui
                            .selectable_label(is_selected, volume.label.as_str())
                            .clicked()
                        {
                            action = Some(FileAction::Browse(volume.path.clone()));
                        }
                    }
                });

            let can_go_up = current_dir.is_some() && !at_root;
            if ui
                .add_enabled(can_go_up, egui::Button::new("⬆ Up"))
                .clicked()
            {
                if let Some(parent) = current_dir.as_deref().and_then(parent_dir) {
                    action = Some(FileAction::Browse(parent.to_owned()));
                }
            }
            if ui.button("Reload").clicked() {
                action = Some(FileAction::Reload);
            }
            if ui.button("📁+ New Folder").clicked() {
                action = Some(FileAction::NewFolder);
            }
            if ui.button("⬆ Upload…").clicked() {
                action = Some(FileAction::Upload);
            }
            ui.checkbox(&mut self.show_hidden, "Show hidden");
            ui.add(
                egui::TextEdit::singleline(&mut self.file_filter)
                    .hint_text("Filter by name")
                    .desired_width(INPUT_WIDTH),
            );
        });

        let needle = self.file_filter.to_lowercase();
        let visible: Vec<&RemoteEntry> = self
            .entries
            .iter()
            .filter(|entry| {
                (self.show_hidden || !entry.name.starts_with('.'))
                    && (needle.is_empty() || entry.name.to_lowercase().contains(&needle))
            })
            .collect();
        let visible_count = visible.len();

        ui.horizontal_wrapped(|ui| match (&root, &current_dir) {
            (Some(root), Some(dir)) => {
                let segments = path_segments(&root.path, dir);
                if segments.is_empty() {
                    ui.strong(root.label.as_str());
                } else if ui.link(root.label.as_str()).clicked() {
                    action = Some(FileAction::Browse(root.path.clone()));
                }
                let last = segments.len().saturating_sub(1);
                for (i, (name, full)) in segments.iter().enumerate() {
                    ui.weak("›");
                    if i == last {
                        ui.strong(name.as_str());
                    } else if ui.link(name.as_str()).clicked() {
                        action = Some(FileAction::Browse(full.clone()));
                    }
                }
                ui.weak(format!(
                    "— {visible_count} of {} item(s)",
                    self.entries.len()
                ));
            }
            _ => {
                ui.weak("Select a storage volume to browse.");
            }
        });
        ui.separator();

        let dir_for_rows = current_dir.clone().unwrap_or_default();

        TableBuilder::new(ui)
            .id_salt("files_table")
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(Column::remainder().at_least(200.0).clip(true))
            .column(Column::initial(100.0).at_least(85.0))
            .column(Column::initial(160.0).at_least(140.0))
            .column(Column::initial(340.0).at_least(320.0))
            .header(26.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Name");
                });
                header.col(|ui| {
                    ui.strong("Size");
                });
                header.col(|ui| {
                    ui.strong("Modified");
                });
                header.col(|ui| {
                    ui.strong("Actions");
                });
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, visible.len(), |mut row| {
                    let entry = visible[row.index()];
                    let path = join_remote(&dir_for_rows, &entry.name);
                    row.col(|ui| match entry.kind {
                        EntryKind::Directory => {
                            let response = ui
                                .add(
                                    egui::Label::new(
                                        RichText::new(format!("📁 {}", entry.name)).strong(),
                                    )
                                    .truncate()
                                    .sense(egui::Sense::click()),
                                )
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if response.clicked() {
                                action = Some(FileAction::Browse(path.clone()));
                            }
                        }
                        EntryKind::File => {
                            let response = ui
                                .add(
                                    egui::Label::new(format!("📄 {}", entry.name))
                                        .truncate()
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_text("Double-click to open");
                            if response.double_clicked() {
                                action = Some(FileAction::Open(path.clone()));
                            }
                        }
                        EntryKind::Other => {
                            ui.add(
                                egui::Label::new(RichText::new(format!("⚙ {}", entry.name)).weak())
                                    .truncate(),
                            );
                        }
                    });
                    row.col(|ui| {
                        if entry.kind == EntryKind::File {
                            ui.label(format_size(entry.size));
                        } else {
                            ui.weak("—");
                        }
                    });
                    row.col(|ui| {
                        if entry.modified_ms == 0 {
                            ui.weak("—");
                        } else {
                            ui.label(format_timestamp(entry.modified_ms));
                        }
                    });
                    row.col(|ui| {
                        if entry.kind == EntryKind::File
                            && row_button(ui, "↗ Open")
                                .on_hover_text(
                                    "Copy to a temporary folder and open with the default app",
                                )
                                .clicked()
                        {
                            action = Some(FileAction::Open(path.clone()));
                        }
                        if entry.kind != EntryKind::Other
                            && row_button(ui, "⬇ Save")
                                .on_hover_text("Download to Downloads/ADB Phone Manager")
                                .clicked()
                        {
                            action = Some(FileAction::Download {
                                path: path.clone(),
                                is_dir: entry.kind == EntryKind::Directory,
                            });
                        }
                        if row_button(ui, "📋 Path")
                            .on_hover_text("Copy device path")
                            .clicked()
                        {
                            action = Some(FileAction::CopyPath(path.clone()));
                        }
                        if row_button(ui, RichText::new("🗑").color(ui.visuals().error_fg_color))
                            .on_hover_text("Delete from phone")
                            .clicked()
                        {
                            action = Some(FileAction::Delete {
                                path: path.clone(),
                                is_dir: entry.kind == EntryKind::Directory,
                            });
                        }
                    });
                });
            });

        match action {
            Some(FileAction::Browse(path)) => self.browse(&path),
            Some(FileAction::Reload) => match self.files_path.clone() {
                Some(path) => self.browse(&path),
                None => self.submit(Request::LoadStorage),
            },
            Some(FileAction::NewFolder) => {
                if let Some(dir) = &self.files_path {
                    self.extras.prompt_new_folder(dir.clone());
                }
            }
            Some(FileAction::Upload) => {
                if let Some(dir) = &self.files_path {
                    let selected = self.selected.clone();
                    let ready = self.ready();
                    let dev = DeviceCtx {
                        serial: selected.as_deref(),
                        ready,
                    };
                    self.extras.upload_dialog(&dev, dir);
                }
            }
            Some(FileAction::Delete { path, is_dir }) => {
                self.extras.prompt_delete(path, is_dir);
            }
            Some(FileAction::Open(path)) => {
                self.set_status(format!("Opening {path}..."), false);
                self.submit(Request::OpenRemote { path });
            }
            Some(FileAction::Download { path, is_dir }) => {
                self.set_status(format!("Downloading {path}..."), false);
                self.submit(Request::Download { path, is_dir });
            }
            Some(FileAction::CopyPath(path)) => {
                ui.ctx().copy_text(path.clone());
                self.set_status(format!("Copied {path} to clipboard."), false);
            }
            None => {}
        }
    }
}

impl Drop for PhoneApp {
    fn drop(&mut self) {
        adb::cleanup_open_cache();
    }
}

impl eframe::App for PhoneApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_responses();
        self.poll_call_state(ctx);

        let selected = self.selected.clone();
        let ready = self.ready();
        let dev = DeviceCtx {
            serial: selected.as_deref(),
            ready,
        };
        self.extras.update(ctx, &dev);

        if let Some((msg, is_err)) = self.extras.take_status() {
            self.set_status(msg, is_err);
        }
        if self.extras.take_files_dirty() {
            if let Some(path) = self.files_path.clone() {
                self.browse(&path);
            }
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.ui_toolbar(ui));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let text = RichText::new(&self.status);
            ui.label(match self.status_kind {
                StatusKind::Error => text.color(ui.visuals().error_fg_color),
                StatusKind::Info => text,
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    if ui.selectable_label(self.tab == tab, tab.title()).clicked() {
                        self.switch_tab(tab);
                    }
                }
            });
            ui.separator();

            let export_data = self.export_data();

            match self.tab {
                Tab::Calls => self.ui_calls(ui),
                Tab::Messages => self.ui_messages(ui),
                Tab::Contacts => self.ui_contacts(ui),
                Tab::Apps => self.ui_apps(ui),
                Tab::Files => self.ui_files(ui),
                Tab::Mirror => {
                    let selected = self.selected.clone();
                    let ready = self.ready();
                    let dev_ctx = DeviceCtx {
                        serial: selected.as_deref(),
                        ready,
                    };
                    self.extras.ui_mirror(ui, &dev_ctx);
                }
                Tab::Control => {
                    let selected = self.selected.clone();
                    let ready = self.ready();
                    let dev_ctx = DeviceCtx {
                        serial: selected.as_deref(),
                        ready,
                    };
                    self.extras.ui_control(ui, &dev_ctx);
                }
                Tab::Info => {
                    let selected = self.selected.clone();
                    let ready = self.ready();
                    let dev_ctx = DeviceCtx {
                        serial: selected.as_deref(),
                        ready,
                    };
                    self.extras.ui_info(ui, &dev_ctx);
                }
                Tab::Logcat => {
                    let selected = self.selected.clone();
                    let ready = self.ready();
                    let dev_ctx = DeviceCtx {
                        serial: selected.as_deref(),
                        ready,
                    };
                    self.extras.ui_logcat(ui, &dev_ctx);
                }
                Tab::Tools => {
                    let selected = self.selected.clone();
                    let ready = self.ready();
                    let dev_ctx = DeviceCtx {
                        serial: selected.as_deref(),
                        ready,
                    };
                    self.extras.ui_tools(ui, &dev_ctx, &export_data);
                }
            }
        });

        self.ui_uninstall_dialog(ctx);
        self.ui_message_dialog(ctx);
        self.ui_message_delete_dialog(ctx);
        let selected = self.selected.clone();
        let ready = self.ready();
        let dev_ctx = DeviceCtx {
            serial: selected.as_deref(),
            ready,
        };
        self.extras.ui_windows(ctx, &dev_ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.extras.shutdown();
    }
}

fn app_icon() -> Option<egui::IconData> {
    eframe::icon_data::from_png_bytes(include_bytes!(
        "../assets/icons/hicolor/256x256/apps/adb-phone-manager.png"
    ))
    .ok()
}

/// Whether winit will use the Wayland backend for this session.
fn wayland_session() -> bool {
    ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .into_iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.is_empty()))
}

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_app_id("adb-phone-manager")
        .with_inner_size(WINDOW_SIZE)
        .with_title("ADB Phone Manager");
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        // On Wayland the compositor stops sending frame callbacks while the
        // window is hidden or occluded. Waiting for vsync then blocks inside
        // eglSwapBuffers, and the compositor marks the app as "not responding"
        // until it becomes visible again. Rendering without waiting fixes that;
        // keep vsync on X11 and the other platforms.
        vsync: !wayland_session(),
        ..Default::default()
    };
    eframe::run_native(
        "ADB Phone Manager",
        options,
        Box::new(|cc| Ok(Box::new(PhoneApp::new(&cc.egui_ctx)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes_with_binary_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
