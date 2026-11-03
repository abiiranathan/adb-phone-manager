//! Background app icon loader and disk cache.

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::thread;

use eframe::egui;

use crate::adb::Adb;
use crate::raster::{decode, DecodedImage};

pub struct IconResult {
    pub serial: String,
    pub package: String,
    pub image: Option<DecodedImage>,
}

pub struct IconLoader {
    tx: Sender<(String, String)>,
    rx: Receiver<IconResult>,
    requested: Mutex<HashSet<String>>,
}

pub fn default_cache_root() -> PathBuf {
    env::temp_dir().join("adb-phone-manager-icons")
}

impl IconLoader {
    pub fn new(ctx: &egui::Context, cache_dir: PathBuf) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<(String, String)>();
        let (res_tx, res_rx) = mpsc::channel::<IconResult>();
        let bg_ctx = ctx.clone();

        thread::spawn(move || {
            let _ = fs::create_dir_all(&cache_dir);
            while let Ok((serial, package)) = req_rx.recv() {
                let cache_file = cache_dir.join(format!("{package}.png"));
                let image = if cache_file.exists() {
                    fs::read(&cache_file).ok().and_then(|b| decode(&b).ok())
                } else {
                    let extracted = extract_icon(&serial, &package);
                    if let Some(ref bytes) = extracted {
                        let _ = fs::write(&cache_file, bytes);
                    }
                    extracted.and_then(|b| decode(&b).ok())
                };

                let _ = res_tx.send(IconResult {
                    serial,
                    package,
                    image,
                });
                bg_ctx.request_repaint();
            }
        });

        Self {
            tx: req_tx,
            rx: res_rx,
            requested: Mutex::new(HashSet::new()),
        }
    }

    pub fn request(&self, serial: String, package: String) {
        let key = format!("{serial}:{package}");
        let mut set = self.requested.lock().unwrap();
        if set.insert(key) {
            let _ = self.tx.send((serial, package));
        }
    }

    pub fn forget_requests(&self) {
        self.requested.lock().unwrap().clear();
    }

    pub fn try_recv(&self) -> Option<IconResult> {
        self.rx.try_recv().ok()
    }
}

fn extract_icon(serial: &str, package: &str) -> Option<Vec<u8>> {
    let adb = Adb::new(Some(serial.to_owned()));
    let out = adb.shell(&format!("pm path {package}")).ok()?;
    let line = out.stdout.lines().next()?;
    let apk_path = line.trim().strip_prefix("package:")?;

    let list_out = adb.shell(&format!("unzip -l {apk_path}")).ok()?;
    let mut icon_path: Option<&str> = None;

    for line in list_out.stdout.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with("ic_launcher.png")
            || trimmed.ends_with("ic_launcher_round.png")
            || trimmed.ends_with("icon.png")
        {
            if let Some(entry) = trimmed.split_whitespace().last() {
                icon_path = Some(entry);
                if trimmed.contains("xxhdpi") || trimmed.contains("xxxhdpi") {
                    break;
                }
            }
        }
    }

    let icon_entry = icon_path?;
    let raw = adb
        .run_raw(&["shell", &format!("unzip -p {apk_path} {icon_entry}")])
        .ok()?;

    if raw.starts_with(b"\x89PNG") {
        Some(raw)
    } else {
        None
    }
}
