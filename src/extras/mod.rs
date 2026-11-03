//! State and UI for extended features: scrcpy mirror, remote hardware control,
//! device info, logcat, wireless pairing, APK installer, file upload, export,
//! shell and icons.

mod control;
mod info;
mod log;
mod mirror;
mod tools;
mod windows;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;

use crate::adb::{Adb, CallRecord, Contact, Sms};
use crate::capture::Shutter;
use crate::controls::{Key, PowerAction};
use crate::device_info::DeviceInfo;
use crate::icons::{self, IconLoader};
use crate::jobs::{apply, JobResult, Jobs};
use crate::logcat::{Level, LogBuffer, LogStream};
use crate::package::{AppDetails, InstallOptions};
use crate::scrcpy;
use crate::wireless::{Service, DEFAULT_TCPIP_PORT};

pub struct DeviceCtx<'a> {
    pub serial: Option<&'a str>,
    pub ready: bool,
}

pub struct ExportData {
    pub contacts: Vec<Contact>,
    pub messages: Vec<Sms>,
    pub calls: Vec<CallRecord>,
}

struct ScreenshotView {
    path: PathBuf,
    texture: egui::TextureHandle,
}

#[derive(Clone)]
struct ManagedApp {
    package: String,
    details: Option<AppDetails>,
    loading: bool,
}

type Job = JobResult<Extras>;

fn done(message: impl Into<String> + Send + 'static) -> Job {
    apply(move |extras: &mut Extras| extras.say(message, false))
}

fn quiet() -> Job {
    apply(|_: &mut Extras| {})
}

#[allow(clippy::struct_excessive_bools)]
pub struct Extras {
    jobs: Jobs<Self>,
    ctx: egui::Context,
    status: Option<(String, bool)>,
    files_dirty: bool,

    // scrcpy
    scrcpy: scrcpy::Options,
    scrcpy_version: Option<Result<scrcpy::Version, String>>,
    scrcpy_probe_started: bool,
    cameras: Vec<scrcpy::CameraInfo>,
    sessions: Vec<scrcpy::Session>,

    // controls
    type_input: String,
    url_input: String,
    brightness: u8,
    media_volume: u32,
    ring_volume: u32,
    pending_power: Option<PowerAction>,

    // capture
    shutter: Shutter,
    record_secs: u32,
    record_mbps: u32,
    screenshot: Option<ScreenshotView>,

    // device info
    info: Option<DeviceInfo>,
    info_loading: bool,
    info_auto: bool,
    info_loaded_at: Option<Instant>,

    // logcat
    log_stream: Option<LogStream>,
    log_buffer: LogBuffer,
    log_filter: String,
    log_level: Level,
    log_follow: bool,

    // tools
    shell_input: String,
    shell_history: Vec<String>,
    pair_address: String,
    pair_code: String,
    tcpip_port: u16,
    services: Vec<Service>,
    install_options: InstallOptions,
    upload_dir: String,
    new_folder: Option<(String, String)>,

    // app management and icons
    managed: Option<ManagedApp>,
    pending_delete: Option<(String, bool)>,
    pub load_icons: bool,
    icon_loader: IconLoader,
    icon_textures: HashMap<String, Option<egui::TextureHandle>>,
    icon_serial: Option<String>,
}

impl Extras {
    pub fn new(ctx: &egui::Context) -> Self {
        Self {
            jobs: Jobs::new(ctx.clone()),
            ctx: ctx.clone(),
            status: None,
            files_dirty: false,
            scrcpy: scrcpy::Options::default(),
            scrcpy_version: None,
            scrcpy_probe_started: false,
            cameras: Vec::new(),
            sessions: Vec::new(),
            type_input: String::new(),
            url_input: String::new(),
            brightness: 128,
            media_volume: 7,
            ring_volume: 4,
            pending_power: None,
            shutter: Shutter::VolumeDown,
            record_secs: 30,
            record_mbps: 8,
            screenshot: None,
            info: None,
            info_loading: false,
            info_auto: true,
            info_loaded_at: None,
            log_stream: None,
            log_buffer: LogBuffer::default(),
            log_filter: String::new(),
            log_level: Level::Verbose,
            log_follow: true,
            shell_input: String::new(),
            shell_history: Vec::new(),
            pair_address: String::new(),
            pair_code: String::new(),
            tcpip_port: DEFAULT_TCPIP_PORT,
            services: Vec::new(),
            install_options: InstallOptions::default(),
            upload_dir: crate::transfer::DEFAULT_UPLOAD_DIR.to_owned(),
            new_folder: None,
            managed: None,
            pending_delete: None,
            load_icons: true,
            icon_loader: IconLoader::new(ctx, icons::default_cache_root()),
            icon_textures: HashMap::new(),
            icon_serial: None,
        }
    }

    pub fn take_status(&mut self) -> Option<(String, bool)> {
        self.status.take()
    }

    pub fn take_files_dirty(&mut self) -> bool {
        std::mem::take(&mut self.files_dirty)
    }

    pub fn busy(&self) -> bool {
        self.jobs.busy()
    }

    pub fn icon_for(&mut self, pkg: &str, dev: &DeviceCtx) -> Option<&egui::TextureHandle> {
        if !self.load_icons || !dev.ready {
            return None;
        }
        if let Some(serial) = dev.serial {
            if !self.icon_textures.contains_key(pkg) {
                self.icon_textures.insert(pkg.to_owned(), None);
                self.icon_loader.request(serial.to_owned(), pkg.to_owned());
            }
        }
        self.icon_textures.get(pkg).and_then(|opt| opt.as_ref())
    }

    pub fn open_app_details(&mut self, package: String, dev: &DeviceCtx) {
        self.managed = Some(ManagedApp {
            package: package.clone(),
            details: None,
            loading: true,
        });
        self.run(dev, move |adb| {
            let details = crate::package::get_details(&adb, &package);
            apply(move |extras: &mut Self| {
                if let Some(m) = &mut extras.managed {
                    if m.package == details.package {
                        m.details = Some(details);
                        m.loading = false;
                    }
                }
            })
        });
    }

    pub fn prompt_new_folder(&mut self, current_dir: String) {
        self.new_folder = Some((current_dir, String::new()));
    }

    pub fn prompt_delete(&mut self, path: String, is_dir: bool) {
        self.pending_delete = Some((path, is_dir));
    }

    pub fn upload_dialog(&mut self, dev: &DeviceCtx, target_dir: &str) {
        if let Some(files) = rfd::FileDialog::new().pick_files() {
            for file in files {
                self.upload_to(dev, file, target_dir.to_owned());
            }
        }
    }

    fn upload_to(&mut self, dev: &DeviceCtx, local: PathBuf, target_dir: String) {
        self.say(format!("Uploading {}…", local.display()), false);
        self.run(dev, move |adb| {
            crate::transfer::upload(&adb, &local, &target_dir)?;
            apply(move |extras: &mut Self| {
                extras.files_dirty = true;
                extras.say(
                    format!("Uploaded {} to {target_dir}.", local.display()),
                    false,
                );
            })
        });
    }

    fn say(&mut self, text: impl Into<String>, is_error: bool) {
        self.status = Some((text.into(), is_error));
    }

    fn run<F>(&mut self, dev: &DeviceCtx, work: F)
    where
        F: FnOnce(Adb) -> Job + Send + 'static,
    {
        if !dev.ready {
            self.say("No authorised device selected.", true);
            return;
        }
        let adb = Adb::new(dev.serial.map(str::to_owned));
        self.jobs.spawn(move || work(adb));
    }

    fn press(&mut self, dev: &DeviceCtx, key: Key) {
        self.run(dev, move |adb| {
            crate::controls::press(&adb, key)?;
            quiet()
        });
    }

    pub fn update(&mut self, ctx: &egui::Context, dev: &DeviceCtx) {
        for result in self.jobs.drain() {
            match result {
                Ok(merge) => merge(self),
                Err(err) => {
                    self.info_loading = false;
                    self.say(err.to_string(), true);
                }
            }
        }
        self.poll_sessions(ctx);
        self.poll_log();
        self.poll_icons(ctx, dev);
        self.handle_dropped_files(ctx, dev);
    }

    fn poll_sessions(&mut self, ctx: &egui::Context) {
        if self.sessions.is_empty() {
            return;
        }
        let mut index = 0;
        while index < self.sessions.len() {
            if let Some(exit) = self.sessions[index].poll() {
                let label = self.sessions.remove(index).label().to_owned();
                if exit.success {
                    self.say(format!("{label} closed."), false);
                } else {
                    self.say(format!("{label} stopped: {}", exit.message), true);
                }
            } else {
                index += 1;
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }

    fn poll_log(&mut self) {
        if let Some(stream) = &self.log_stream {
            let lines = stream.drain();
            if !lines.is_empty() {
                self.log_buffer.extend(lines);
            }
        }
    }

    fn poll_icons(&mut self, ctx: &egui::Context, dev: &DeviceCtx) {
        if self.icon_serial.as_deref() != dev.serial {
            self.icon_serial = dev.serial.map(str::to_owned);
            self.icon_textures.clear();
            self.icon_loader.forget_requests();
        }
        while let Some(result) = self.icon_loader.try_recv() {
            if self.icon_serial.as_deref() != Some(result.serial.as_str()) {
                continue;
            }
            let texture = result.image.map(|rgba| {
                ctx.load_texture(
                    format!("icon-{}", result.package),
                    rgba.to_color_image(),
                    egui::TextureOptions::LINEAR,
                )
            });
            self.icon_textures.insert(result.package, texture);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context, dev: &DeviceCtx) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        let (apks, others): (Vec<_>, Vec<_>) =
            dropped.into_iter().partition(|p| crate::package::is_apk(p));
        if !apks.is_empty() {
            self.install(dev, apks);
        }
        for path in others {
            self.upload(dev, path);
        }
    }

    fn install(&mut self, dev: &DeviceCtx, apks: Vec<PathBuf>) {
        let options = self.install_options;
        self.say("Installing…", false);
        self.run(dev, move |adb| {
            let message = crate::package::install(&adb, &apks, options)?;
            done(message)
        });
    }

    fn upload(&mut self, dev: &DeviceCtx, local: PathBuf) {
        let remote_dir = self.upload_dir.trim().to_owned();
        self.say(format!("Uploading {}…", local.display()), false);
        self.run(dev, move |adb| {
            crate::transfer::upload(&adb, &local, &remote_dir)?;
            apply(move |extras: &mut Self| {
                extras.files_dirty = true;
                extras.say(
                    format!("Uploaded {} to {remote_dir}.", local.display()),
                    false,
                );
            })
        });
    }
}

impl Drop for Extras {
    fn drop(&mut self) {
        self.sessions.clear();
        self.log_stream = None;
    }
}
