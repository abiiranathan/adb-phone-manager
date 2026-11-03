//! Live logcat stream and log buffer.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::adb::{Adb, AdbError};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Verbose,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    pub const ALL: [Self; 6] = [
        Self::Verbose,
        Self::Debug,
        Self::Info,
        Self::Warn,
        Self::Error,
        Self::Fatal,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Verbose => "V - Verbose",
            Self::Debug => "D - Debug",
            Self::Info => "I - Info",
            Self::Warn => "W - Warning",
            Self::Error => "E - Error",
            Self::Fatal => "F - Fatal",
        }
    }

    pub const fn from_char(c: char) -> Self {
        match c {
            'D' => Self::Debug,
            'I' => Self::Info,
            'W' => Self::Warn,
            'E' => Self::Error,
            'F' => Self::Fatal,
            _ => Self::Verbose,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LogLine {
    pub level: Level,
    pub raw: String,
}

impl LogLine {
    pub fn to_text(&self) -> String {
        self.raw.clone()
    }
}

#[derive(Default)]
pub struct LogBuffer {
    lines: Vec<LogLine>,
}

impl LogBuffer {
    pub fn clear(&mut self) {
        self.lines.clear();
    }

    pub fn extend(&mut self, new_lines: Vec<LogLine>) {
        self.lines.extend(new_lines);
        if self.lines.len() > 30_000 {
            let excess = self.lines.len() - 25_000;
            self.lines.drain(..excess);
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn filtered(&self, min_level: Level, needle: &str) -> Vec<&LogLine> {
        let needle = needle.to_lowercase();
        self.lines
            .iter()
            .filter(|l| {
                l.level >= min_level
                    && (needle.is_empty() || l.raw.to_lowercase().contains(&needle))
            })
            .collect()
    }
}

pub struct LogStream {
    child: Child,
    queue: Arc<Mutex<Vec<LogLine>>>,
    stop: Arc<AtomicBool>,
}

impl LogStream {
    pub fn drain(&self) -> Vec<LogLine> {
        let mut q = self.queue.lock().unwrap();
        std::mem::take(&mut *q)
    }
}

impl Drop for LogStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
    }
}

pub fn start(adb: &Adb, ctx: egui::Context) -> Result<LogStream, AdbError> {
    let mut cmd = Command::new("adb");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    if let Some(s) = adb.serial() {
        cmd.arg("-s").arg(s);
    }
    cmd.args(["logcat", "-v", "time"]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().map_err(AdbError::Spawn)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AdbError::Failed("Could not capture stdout of logcat process".to_owned()))?;

    let queue = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));

    let t_queue = Arc::clone(&queue);
    let t_stop = Arc::clone(&stop);

    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line_res in reader.lines() {
            if t_stop.load(Ordering::Relaxed) {
                break;
            }
            if let Ok(raw) = line_res {
                let level = parse_level(&raw);
                t_queue.lock().unwrap().push(LogLine { level, raw });
                ctx.request_repaint();
            }
        }
    });

    Ok(LogStream { child, queue, stop })
}

pub fn clear_device_log(adb: &Adb) -> Result<(), AdbError> {
    adb.run(&["logcat", "-c"]).map(|_| ())
}

fn parse_level(line: &str) -> Level {
    if let Some(pos) = line.find('/') {
        if pos > 0 {
            if let Some(c) = line[..pos].chars().last() {
                if matches!(c, 'V' | 'D' | 'I' | 'W' | 'E' | 'F') {
                    return Level::from_char(c);
                }
            }
        }
    }
    Level::Verbose
}
