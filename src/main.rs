//! ADB Phone Manager: a small egui front end for an Android phone reachable over adb.

mod adb;
mod worker;

use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};
use egui_extras::{Column, TableBuilder};

use crate::adb::{
    join_remote, parent_dir, parse_text_segments, path_segments, CallRecord, CallState, CallType,
    Contact, Device, EntryKind, RemoteEntry, Sms, SmsKind, StorageRoot, TextSegment,
    UninstallOutcome,
};
use crate::worker::{Request, Response, Worker};

/// Initial window size in logical pixels.
const WINDOW_SIZE: [f32; 2] = [1050.0, 720.0];
/// How often the call state is polled while a device is ready.
const CALL_POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Default number of SMS messages loaded.
const DEFAULT_MESSAGE_LIMIT: usize = 200;
/// Default number of call logs loaded.
const DEFAULT_CALL_LIMIT: usize = 100;
/// Allowed range for the limits control.
const LIMIT_RANGE: std::ops::RangeInclusive<usize> = 10..=5000;
/// adb state string for a connected, authorised device.
const DEVICE_READY_STATE: &str = "device";
/// Width of single-line input boxes.
const INPUT_WIDTH: f32 = 220.0;
/// Unit names used when formatting file sizes.
const BYTE_UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
/// Factor between consecutive entries of [`BYTE_UNITS`].
const UNIT_STEP: f64 = 1024.0;
/// Height of one table row, tall enough for padded action buttons.
const ROW_HEIGHT: f32 = 32.0;

/// Top-level tabs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Calls,
    Messages,
    Contacts,
    Apps,
    Files,
}

impl Tab {
    const ALL: [Self; 5] = [
        Self::Calls,
        Self::Messages,
        Self::Contacts,
        Self::Apps,
        Self::Files,
    ];

    const fn title(self) -> &'static str {
        match self {
            Self::Calls => "Calls",
            Self::Messages => "Messages",
            Self::Contacts => "Contacts",
            Self::Apps => "Apps",
            Self::Files => "Files",
        }
    }
}

/// Severity of the message shown in the status bar.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    /// Informational message.
    Info,
    /// Failure report, drawn in red.
    Error,
}

/// Row action chosen in the app list.
enum AppAction {
    Launch(String),
    Stop(String),
    /// Ask for confirmation before uninstalling the package.
    Uninstall(String),
}

/// Action chosen in the file browser during one frame.
enum FileAction {
    /// Show the directory at this device path.
    Browse(String),
    /// Reload the current directory (or the storage list if none is shown yet).
    Reload,
    /// Open the file at this device path with the desktop's default application.
    Open(String),
    /// Save a file or directory to the downloads folder.
    Download { path: String, is_dir: bool },
    /// Copy this device path to the clipboard.
    CopyPath(String),
}

/// Derives a clean, readable name from an Android package identifier.
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

/// Formats duration in seconds into a human-readable string.
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

/// Formats a byte count using binary units, e.g. `1.5 KB`.
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    // Precision loss is irrelevant: the value is only displayed with one decimal.
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64 / UNIT_STEP;
    let mut unit = 1;
    while value >= UNIT_STEP && unit < BYTE_UNITS.len() - 1 {
        value /= UNIT_STEP;
        unit += 1;
    }
    format!("{value:.1} {}", BYTE_UNITS[unit])
}

/// Formats a millisecond Unix timestamp in the local time zone.
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

/// Applies the application-wide typography, spacing and contrast tweaks.
///
/// egui's defaults target compact demos: small controls and secondary text that
/// fades almost into the background. This app is mostly dense tables, so it uses
/// slightly larger type, roomier widgets and higher-contrast text instead.
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
            // Weak text is painted half-way towards the panel background, so the
            // default gray(140) body text fades to nearly invisible. A brighter
            // base keeps timestamps and hints readable while staying secondary.
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

/// Adds a row action button: full button padding, but capped to fit a table row.
fn row_button(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::new(label).min_size(egui::vec2(0.0, 22.0)))
}

/// Builds a `Name (number)` label with the name emphasised and the number muted.
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

/// Application state. Lives on the UI thread only.
struct PhoneApp {
    /// Background job runner.
    worker: Worker,
    /// Devices from the last refresh.
    devices: Vec<Device>,
    /// Serial of the active device.
    selected: Option<String>,
    /// Visible tab.
    tab: Tab,
    /// Loaded address book.
    contacts: Vec<Contact>,
    /// Loaded SMS messages.
    messages: Vec<Sms>,
    /// Loaded call history.
    call_logs: Vec<CallRecord>,
    /// Loaded package names.
    apps: Vec<String>,
    /// Storage volumes of the active device (internal storage, SD card...).
    storage_roots: Vec<StorageRoot>,
    /// Directory currently shown in the file browser.
    files_path: Option<String>,
    /// Directory whose listing is in flight; any other listing that arrives is stale.
    files_requested: Option<String>,
    /// Entries of `files_path`, directories first.
    entries: Vec<RemoteEntry>,
    /// Latest polled telephony state.
    call_state: CallState,
    /// Filter text per tab.
    contact_filter: String,
    message_filter: String,
    call_filter: String,
    app_filter: String,
    file_filter: String,
    /// `host:port` typed into the toolbar.
    connect_input: String,
    /// Number typed into the dialler.
    dial_input: String,
    /// How many SMS messages to load.
    message_limit: usize,
    /// How many call logs to load.
    call_limit: usize,
    /// Hide system apps in the app list.
    third_party_only: bool,
    /// Package waiting for the user to confirm its uninstall.
    pending_uninstall: Option<String>,
    /// Show dot-files in the file browser.
    show_hidden: bool,
    /// Status bar text and its severity.
    status: String,
    status_kind: StatusKind,
    /// Foreground jobs in flight (drives the spinner).
    pending: usize,
    /// Whether a call-state poll is in flight.
    poll_inflight: bool,
    /// When the last poll was started.
    last_poll: Instant,
}

impl PhoneApp {
    fn new(ctx: egui::Context) -> Self {
        configure_style(&ctx);
        // The worker's device watcher performs the first scan and reports it.
        Self {
            worker: Worker::new(ctx),
            devices: Vec::new(),
            selected: None,
            tab: Tab::Calls,
            contacts: Vec::new(),
            messages: Vec::new(),
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

    /// Returns whether the selected device is connected and authorised.
    fn ready(&self) -> bool {
        self.selected.as_ref().is_some_and(|serial| {
            self.devices
                .iter()
                .any(|d| &d.serial == serial && d.state == DEVICE_READY_STATE)
        })
    }

    fn set_status(&mut self, text: impl Into<String>, is_error: bool) {
        self.status = text.into();
        self.status_kind = if is_error {
            StatusKind::Error
        } else {
            StatusKind::Info
        };
    }

    /// Queues a foreground request, refusing device requests while no device is ready.
    fn submit(&mut self, request: Request) {
        if request.needs_device() && !self.ready() {
            self.set_status("No authorised device selected.", true);
            return;
        }
        self.pending += 1;
        self.worker.submit(self.selected.clone(), request);
    }

    /// Switches the active device and drops data that belonged to the previous one.
    fn select(&mut self, serial: Option<String>) {
        if self.selected == serial {
            return;
        }
        self.selected = serial;
        self.contacts.clear();
        self.messages.clear();
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
        if self.ready() && self.tab == Tab::Calls && self.call_logs.is_empty() {
            self.load_call_logs();
        }
        if self.ready() && self.tab == Tab::Files && self.storage_roots.is_empty() {
            self.submit(Request::LoadStorage);
        }
    }

    /// Applies every finished background result.
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
                    // Open the first volume automatically, but never override a navigation
                    // the user already started.
                    if self.files_path.is_none() && self.files_requested.is_none() {
                        if let Some(path) = first {
                            self.browse(&path);
                        }
                    }
                }
                Response::Dir { path, entries } => {
                    // Listings can finish out of order when the user clicks quickly;
                    // only the most recently requested directory is shown.
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

    /// Starts a background call-state poll when one is due.
    fn poll_call_state(&mut self, ctx: &egui::Context) {
        if !self.ready() {
            return;
        }
        if !self.poll_inflight && self.last_poll.elapsed() >= CALL_POLL_INTERVAL {
            self.poll_inflight = true;
            self.last_poll = Instant::now();
            self.worker
                .submit(self.selected.clone(), Request::PollCallState);
        }
        ctx.request_repaint_after(CALL_POLL_INTERVAL);
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

    /// Requests the listing of `path`; the file browser switches to it when it arrives.
    fn browse(&mut self, path: &str) {
        self.files_requested = Some(path.to_owned());
        self.submit(Request::ListDir {
            path: path.to_owned(),
        });
    }

    /// Returns the storage volume containing `path`, preferring the deepest match.
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

    /// Switches tab, loading its data on first visit.
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
            if self.pending > 0 {
                ui.spinner();
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
        let mut dial_number: Option<String> = None;
        let mut copy_msg: Option<String> = None;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for sms in self.messages.iter().filter(|m| {
                    needle.is_empty()
                        || m.address.to_lowercase().contains(&needle)
                        || m.body.to_lowercase().contains(&needle)
                }) {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
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
                            ui.add(
                                egui::Label::new(RichText::new(&sms.address).strong())
                                    .selectable(true),
                            );
                            ui.weak(format_timestamp(sms.date_ms));

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if row_button(ui, "📋 Copy Text")
                                        .on_hover_text("Copy message body")
                                        .clicked()
                                    {
                                        copy_msg = Some(sms.body.clone());
                                    }
                                    if row_button(ui, "📋 Copy Number")
                                        .on_hover_text("Copy phone number")
                                        .clicked()
                                    {
                                        copy_msg = Some(sms.address.clone());
                                    }
                                    if row_button(ui, "📞 Call")
                                        .on_hover_text("Call number")
                                        .clicked()
                                    {
                                        dial_number = Some(sms.address.clone());
                                    }
                                },
                            );
                        });

                        ui.add_space(3.0);

                        for line in sms.body.split('\n') {
                            if line.is_empty() {
                                ui.add_space(4.0);
                                continue;
                            }
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for segment in parse_text_segments(line) {
                                    match segment {
                                        TextSegment::Text(t) => {
                                            ui.add(egui::Label::new(t).selectable(true));
                                        }
                                        TextSegment::Url(url) => {
                                            ui.hyperlink(url);
                                        }
                                    }
                                }
                            });
                        }
                    });
                    ui.add_space(4.0);
                }
            });

        if let Some(number) = dial_number {
            self.dial_input.clone_from(&number);
            self.submit(Request::Dial(number));
        }
        if let Some(text) = copy_msg {
            ui.ctx().copy_text(text.clone());
            self.set_status(format!("Copied to clipboard: {text}"), false);
        }
    }

    #[allow(clippy::too_many_lines)]
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

        TableBuilder::new(ui)
            .id_salt("apps_table")
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .auto_shrink([false, false])
            .column(Column::initial(200.0).at_least(160.0).clip(true))
            .column(Column::remainder().at_least(220.0).clip(true))
            .column(Column::initial(390.0).at_least(360.0))
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
            Some(AppAction::Uninstall(package)) => self.pending_uninstall = Some(package),
            None => {}
        }
        if let Some(pkg) = copy_pkg {
            ui.ctx().copy_text(pkg.clone());
            self.set_status(format!("Copied {pkg} to clipboard."), false);
        }
    }

    /// Asks for confirmation before queueing the pending uninstall.
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

    /// File browser: storage picker, breadcrumb, and a table of the current directory.
    #[allow(clippy::too_many_lines)]
    fn ui_files(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<FileAction> = None;

        // Snapshot what the closures below need, so they do not borrow `self` as a whole.
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
            .column(Column::initial(280.0).at_least(260.0))
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
                    });
                });
            });

        match action {
            Some(FileAction::Browse(path)) => self.browse(&path),
            Some(FileAction::Reload) => match self.files_path.clone() {
                Some(path) => self.browse(&path),
                None => self.submit(Request::LoadStorage),
            },
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
    /// Removes the temporary copies made by "Open" so device files do not linger on disk.
    fn drop(&mut self) {
        adb::cleanup_open_cache();
    }
}

impl eframe::App for PhoneApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_responses();
        self.poll_call_state(ctx);

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
            match self.tab {
                Tab::Calls => self.ui_calls(ui),
                Tab::Messages => self.ui_messages(ui),
                Tab::Contacts => self.ui_contacts(ui),
                Tab::Apps => self.ui_apps(ui),
                Tab::Files => self.ui_files(ui),
            }
        });
        self.ui_uninstall_dialog(ctx);
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(WINDOW_SIZE)
            .with_title("ADB Phone Manager"),
        ..Default::default()
    };
    eframe::run_native(
        "ADB Phone Manager",
        options,
        Box::new(|cc| Ok(Box::new(PhoneApp::new(cc.egui_ctx.clone())))),
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
