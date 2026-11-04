//! Synchronous wrapper around the `adb` command-line tool.

use std::env;
use std::error::Error;
use std::fmt;
use std::fmt::Write;
use std::fs;
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const ADB_PROGRAM: &str = "adb";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const CONTACTS_URI: &str = "content://com.android.contacts/data/phones";
const SMS_URI: &str = "content://sms";
const CALL_LOG_URI: &str = "content://call_log/calls";
const ROW_PREFIX: &str = "Row: ";
const NULL_LITERAL: &str = "NULL";
const COLUMN_SEPARATOR: &str = ", ";

const KEYCODE_CALL: u32 = 5;
const KEYCODE_ENDCALL: u32 = 6;
const KEYCODE_HEADSETHOOK: u32 = 79;

const ANSWER_RETRY_DELAY: Duration = Duration::from_millis(700);

/// How long a regular adb command may run before it is killed. Anything that
/// moves data to or from the device gets [`TRANSFER_TIMEOUT`] instead.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_LIST_TIMEOUT: Duration = Duration::from_secs(10);
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const READER_GRACE: Duration = Duration::from_secs(2);
const MAX_COMMAND_SUMMARY_LEN: usize = 120;

const MAX_PACKAGE_LEN: usize = 255;
const MAX_NUMBER_LEN: usize = 32;
const MAX_ADDRESS_LEN: usize = 255;
const MAX_REMOTE_PATH_LEN: usize = 4096;
const MAX_NAME_COLLISIONS: u32 = 1000;

const STORAGE_DIR: &str = "/storage";
const INTERNAL_STORAGE_PATH: &str = "/storage/emulated/0";
const INTERNAL_STORAGE_LABEL: &str = "Internal storage";
const EMULATED_VOLUME: &str = "emulated";
const SELF_VOLUME: &str = "self";
const STAT_FORMAT: &str = "%F|%s|%Y|%n";
const FIELD_SEPARATOR: char = '|';
const DOWNLOAD_SUBDIR: &str = "ADB Phone Manager";
const OPEN_TEMP_SUBDIR: &str = "adb-phone-manager-open";
const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "app", "appimage", "bat", "cmd", "com", "command", "cpl", "desktop", "dll", "exe", "hta",
    "jar", "js", "jse", "lnk", "msi", "pif", "ps1", "reg", "scr", "sh", "vbe", "vbs", "wsf", "wsh",
];

#[derive(Debug)]
pub enum AdbError {
    Spawn(io::Error),
    Failed(String),
    InvalidInput(String),
    Local(String),
    Timeout(String),
}

impl fmt::Display for AdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(err) => write!(
                f,
                "cannot run `{ADB_PROGRAM}` (is it installed and on PATH?): {err}"
            ),
            Self::Failed(msg) | Self::Local(msg) | Self::Timeout(msg) => write!(f, "{msg}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
        }
    }
}

impl Error for AdbError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Spawn(err) => Some(err),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub serial: String,
    pub state: String,
    pub model: Option<String>,
}

impl Device {
    pub fn label(&self) -> String {
        self.model.as_ref().map_or_else(
            || format!("{} [{}]", self.serial, self.state),
            |model| format!("{model} ({}) [{}]", self.serial, self.state),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallOutcome {
    Removed,
    RemovedForUser,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub name: String,
    pub number: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmsKind {
    Received,
    Sent,
    Other,
}

impl SmsKind {
    fn from_type(raw: &str) -> Self {
        match raw.trim() {
            "1" => Self::Received,
            "2" => Self::Sent,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sms {
    /// Provider row id (`_id`), used to delete the message.
    pub id: i64,
    pub address: String,
    pub date_ms: i64,
    pub kind: SmsKind,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallType {
    Incoming,
    Outgoing,
    Missed,
    Voicemail,
    Rejected,
    Blocked,
    Other,
}

impl CallType {
    fn from_type(raw: &str) -> Self {
        match raw.trim() {
            "1" => Self::Incoming,
            "2" => Self::Outgoing,
            "3" => Self::Missed,
            "4" => Self::Voicemail,
            "5" => Self::Rejected,
            "6" => Self::Blocked,
            _ => Self::Other,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Incoming => "Incoming",
            Self::Outgoing => "Outgoing",
            Self::Missed => "Missed",
            Self::Voicemail => "Voicemail",
            Self::Rejected => "Rejected",
            Self::Blocked => "Blocked",
            Self::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRecord {
    pub name: String,
    pub number: String,
    pub date_ms: i64,
    pub duration_secs: u64,
    pub call_type: CallType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    Unknown,
    Idle,
    Offhook,
    Ringing,
}

impl CallState {
    const fn priority(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Idle => 1,
            Self::Offhook => 2,
            Self::Ringing => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRoot {
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub modified_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TextSegment<'a> {
    Text(&'a str),
    Url(&'a str),
}

pub fn parse_text_segments(text: &str) -> Vec<TextSegment<'_>> {
    let mut segments = Vec::new();
    let mut cursor = 0;

    while cursor < text.len() {
        let rest = &text[cursor..];
        let next_http = match (rest.find("https://"), rest.find("http://")) {
            (Some(s), Some(h)) => Some(s.min(h)),
            (Some(s), None) => Some(s),
            (None, Some(h)) => Some(h),
            (None, None) => None,
        };

        let Some(http_pos) = next_http else {
            segments.push(TextSegment::Text(rest));
            break;
        };

        if http_pos > 0 {
            segments.push(TextSegment::Text(&rest[..http_pos]));
        }

        let url_start = cursor + http_pos;
        let url_rest = &text[url_start..];
        let end_idx = url_rest.find(char::is_whitespace).unwrap_or(url_rest.len());
        let mut raw_url = &url_rest[..end_idx];

        let mut trimmed_len = 0;
        while let Some(last) = raw_url.chars().last() {
            let should_strip = match last {
                '.' | ',' | ';' | ':' | '!' | '?' | ']' | '>' | '\'' | '"' => true,
                ')' => {
                    let open = raw_url.chars().filter(|&c| c == '(').count();
                    let close = raw_url.chars().filter(|&c| c == ')').count();
                    close > open
                }
                _ => false,
            };

            if should_strip {
                raw_url = &raw_url[..raw_url.len() - last.len_utf8()];
                trimmed_len += last.len_utf8();
            } else {
                break;
            }
        }

        if !raw_url.is_empty() {
            segments.push(TextSegment::Url(raw_url));
        }
        if trimmed_len > 0 {
            let trailing = &url_rest[raw_url.len()..end_idx];
            segments.push(TextSegment::Text(trailing));
        }

        cursor = url_start + end_idx;
    }

    segments
}

pub fn join_remote(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name)
}

pub fn parent_dir(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    let idx = trimmed.rfind('/')?;
    Some(if idx == 0 { "/" } else { &trimmed[..idx] })
}

pub fn path_segments(root: &str, path: &str) -> Vec<(String, String)> {
    let root = root.trim_end_matches('/');
    let Some(rest) = path.strip_prefix(root) else {
        return Vec::new();
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        return Vec::new();
    }
    let mut full = root.to_owned();
    let mut segments = Vec::new();
    for name in rest.split('/').filter(|s| !s.is_empty()) {
        full.push('/');
        full.push_str(name);
        segments.push((name.to_owned(), full.clone()));
    }
    segments
}

pub fn cleanup_open_cache() {
    let _ = fs::remove_dir_all(open_cache_dir());
}

pub struct Captured {
    pub stdout: String,
    pub stderr: String,
}

pub fn resolve_program(program: &str) -> Result<PathBuf, AdbError> {
    let mut candidates = Vec::new();

    if let Some(root) = env::var_os("ANDROID_HOME") {
        let root = PathBuf::from(root);
        candidates.push(root.join("platform-tools").join(program));
        candidates.push(
            root.join("cmdline-tools")
                .join("latest")
                .join("bin")
                .join(program),
        );
        candidates.push(root.join("tools").join(program));
    }
    if let Some(root) = env::var_os("ANDROID_SDK_ROOT") {
        let root = PathBuf::from(root);
        candidates.push(root.join("platform-tools").join(program));
        candidates.push(
            root.join("cmdline-tools")
                .join("latest")
                .join("bin")
                .join(program),
        );
        candidates.push(root.join("tools").join(program));
    }

    if let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        candidates.push(
            home.join("Android")
                .join("Sdk")
                .join("platform-tools")
                .join(program),
        );
        candidates.push(
            home.join(".local")
                .join("share")
                .join("Android")
                .join("Sdk")
                .join("platform-tools")
                .join(program),
        );
        if cfg!(windows) {
            candidates.push(
                home.join("AppData")
                    .join("Local")
                    .join("Android")
                    .join("Sdk")
                    .join("platform-tools")
                    .join(program),
            );
        }
    }

    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            candidates.push(dir.join(program));
        }
    }

    if let Some(sdk) = env::var_os("ANDROID_SDK_HOME") {
        let sdk = PathBuf::from(sdk);
        candidates.push(sdk.join("platform-tools").join(program));
    }

    if cfg!(windows) {
        let windows_dir = env::var_os("SystemRoot").map(PathBuf::from);
        if let Some(dir) = windows_dir {
            candidates.push(dir.join("System32").join(program));
        }
    }

    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(AdbError::Spawn(io::Error::new(
        ErrorKind::NotFound,
        format!("`{program}` is not installed or not on PATH; install Android platform-tools and ensure ANDROID_HOME/ANDROID_SDK_ROOT point to the SDK"),
    )))
}

pub struct Adb {
    serial: Option<String>,
}

impl Adb {
    pub const fn new(serial: Option<String>) -> Self {
        Self { serial }
    }

    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    pub fn list_devices() -> Result<Vec<Device>, AdbError> {
        let out = Self::new(None).run(&["devices", "-l"])?;
        Ok(parse_devices(&out.stdout))
    }

    pub fn connect(address: &str) -> Result<String, AdbError> {
        validate_address(address)?;
        let out = Self::new(None).run(&["connect", address])?;
        let message = out.stdout.trim().to_owned();
        if message.starts_with("failed") || message.starts_with("cannot") {
            return Err(AdbError::Failed(message));
        }
        Ok(message)
    }

    pub fn contacts(&self) -> Result<Vec<Contact>, AdbError> {
        let rows = self.content_query(CONTACTS_URI, &["display_name", "data1"], None)?;
        let mut contacts: Vec<Contact> = rows
            .into_iter()
            .filter_map(|mut row| {
                let number = row.pop()?;
                let name = row.pop()?;
                Some(Contact { name, number })
            })
            .collect();
        contacts.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.number.cmp(&b.number))
        });
        contacts.dedup();
        Ok(contacts)
    }

    pub fn messages(&self, limit: usize) -> Result<Vec<Sms>, AdbError> {
        let rows = self.content_query(
            SMS_URI,
            &["_id", "address", "date", "type", "body"],
            Some("date DESC"),
        )?;
        Ok(rows
            .into_iter()
            .take(limit)
            .filter_map(|row| {
                let [id, address, date, kind, body] = <[String; 5]>::try_from(row).ok()?;
                Some(Sms {
                    id: id.trim().parse().unwrap_or(0),
                    address,
                    date_ms: date.trim().parse().unwrap_or(0),
                    kind: SmsKind::from_type(&kind),
                    body,
                })
            })
            .collect())
    }

    /// Deletes messages from the phone's SMS provider by their `_id`.
    pub fn delete_messages(&self, ids: &[i64]) -> Result<(), AdbError> {
        if ids.is_empty() {
            return Ok(());
        }
        let id_list = ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let where_clause = format!("_id IN ({id_list})");
        let out = self.shell(&format!(
            "content delete --uri {SMS_URI} --where {}",
            shell_quote(&where_clause)
        ))?;
        if let Some(problem) = remote_failure(&out) {
            return Err(AdbError::Failed(problem));
        }
        Ok(())
    }

    pub fn call_logs(&self, limit: usize) -> Result<Vec<CallRecord>, AdbError> {
        let rows = self.content_query(
            CALL_LOG_URI,
            &["number", "date", "duration", "type", "name"],
            Some("date DESC"),
        )?;
        Ok(rows
            .into_iter()
            .take(limit)
            .filter_map(|row| {
                let [number, date, duration, call_type, name] =
                    <[String; 5]>::try_from(row).ok()?;
                Some(CallRecord {
                    name,
                    number,
                    date_ms: date.trim().parse().unwrap_or(0),
                    duration_secs: duration.trim().parse().unwrap_or(0),
                    call_type: CallType::from_type(&call_type),
                })
            })
            .collect())
    }

    pub fn packages(&self, third_party_only: bool) -> Result<Vec<String>, AdbError> {
        let script = if third_party_only {
            "pm list packages -3"
        } else {
            "pm list packages"
        };
        let out = self.shell(script)?;
        Ok(parse_packages(&out.stdout))
    }

    pub fn launch(&self, package: &str) -> Result<(), AdbError> {
        validate_package(package)?;
        let out = self.shell(&format!(
            "monkey -p {package} -c android.intent.category.LAUNCHER 1"
        ))?;
        if out.stdout.contains("No activities found") {
            return Err(AdbError::Failed(format!(
                "{package} has no launchable activity"
            )));
        }
        Ok(())
    }

    pub fn force_stop(&self, package: &str) -> Result<(), AdbError> {
        validate_package(package)?;
        self.shell(&format!("am force-stop {package}")).map(|_| ())
    }

    pub fn uninstall(&self, package: &str) -> Result<UninstallOutcome, AdbError> {
        validate_package(package)?;

        if let Ok(out) = self.shell(&format!("pm uninstall {package}")) {
            if pm_succeeded(&out) {
                return Ok(UninstallOutcome::Removed);
            }
        }

        let out = self.shell(&format!("pm uninstall --user 0 {package}"))?;
        if pm_succeeded(&out) {
            Ok(UninstallOutcome::RemovedForUser)
        } else {
            Err(AdbError::Failed(format!(
                "could not uninstall {package}: {}",
                pm_message(&out)
            )))
        }
    }

    pub fn call_state(&self) -> Result<CallState, AdbError> {
        let out = self.shell("dumpsys telephony.registry")?;
        Ok(parse_call_state(&out.stdout))
    }

    pub fn answer_call(&self) -> Result<(), AdbError> {
        match self.call_state()? {
            CallState::Ringing | CallState::Unknown => {}
            _ => return Err(AdbError::Failed("no incoming call is ringing".to_owned())),
        }
        self.keyevent(KEYCODE_CALL)?;
        thread::sleep(ANSWER_RETRY_DELAY);
        if self.call_state()? == CallState::Ringing {
            self.keyevent(KEYCODE_HEADSETHOOK)?;
        }
        Ok(())
    }

    pub fn end_call(&self) -> Result<(), AdbError> {
        self.keyevent(KEYCODE_ENDCALL)
    }

    pub fn dial(&self, number: &str) -> Result<(), AdbError> {
        let number = number.replace(' ', "");
        validate_number(&number)?;

        let out = self.shell(&format!(
            "am start -a android.intent.action.CALL -d tel:{number}"
        ))?;
        if let Some(problem) = remote_failure(&out) {
            return Err(AdbError::Failed(problem));
        }
        Ok(())
    }

    pub fn storage_roots(&self) -> Result<Vec<StorageRoot>, AdbError> {
        let listing = match self.shell(&format!("ls -1 {}", shell_quote(STORAGE_DIR))) {
            Ok(out) => out.stdout,
            Err(AdbError::Failed(_)) => String::new(),
            Err(err) => return Err(err),
        };
        Ok(parse_storage_roots(&listing))
    }

    pub fn list_dir(&self, dir: &str) -> Result<Vec<RemoteEntry>, AdbError> {
        validate_remote_path(dir)?;
        let script = format!(
            "find {} -maxdepth 1 -mindepth 1 -exec stat -L -c '{STAT_FORMAT}' {{}} + ; true",
            shell_quote(&join_remote(dir, ""))
        );
        let out = self.shell(&script)?;
        let entries = parse_dir_listing(&out.stdout);
        if entries.is_empty() {
            let problem = first_line(&out.stderr).or_else(|| first_line(&out.stdout));
            if let Some(problem) = problem {
                return Err(AdbError::Failed(problem));
            }
        }
        Ok(entries)
    }

    pub fn download(&self, remote: &str, is_dir: bool) -> Result<PathBuf, AdbError> {
        validate_remote_path(remote)?;
        let name = remote_file_name(remote)?;
        let dest_dir = download_dir()?;
        if is_dir {
            self.run(&["pull", remote, path_arg(&dest_dir)?])?;
            return Ok(dest_dir.join(name));
        }
        let local = unique_path(&dest_dir, &sanitise_file_name(name))?;
        self.run(&["pull", remote, path_arg(&local)?])?;
        Ok(local)
    }

    pub fn open_remote(&self, remote: &str) -> Result<PathBuf, AdbError> {
        validate_remote_path(remote)?;
        let name = sanitise_file_name(remote_file_name(remote)?);
        if is_executable_name(&name) {
            return Err(AdbError::Failed(format!(
                "refusing to open {name:?}: executable and script files can only be saved"
            )));
        }
        let dir = open_cache_dir();
        fs::create_dir_all(&dir)
            .map_err(|err| AdbError::Local(format!("cannot create {}: {err}", dir.display())))?;
        let local = unique_path(&dir, &name)?;
        self.run(&["pull", remote, path_arg(&local)?])?;
        launch_default(&local)?;
        Ok(local)
    }

    fn keyevent(&self, code: u32) -> Result<(), AdbError> {
        self.shell(&format!("input keyevent {code}")).map(|_| ())
    }

    fn content_query(
        &self,
        uri: &str,
        projection: &[&str],
        sort: Option<&str>,
    ) -> Result<Vec<Vec<String>>, AdbError> {
        let mut script = format!(
            "content query --uri {uri} --projection {}",
            projection.join(":")
        );
        if let Some(sort) = sort {
            let _ = write!(script, " --sort '{sort}'");
        }
        let out = self.shell(&script)?;
        if !out.stdout.contains(ROW_PREFIX) {
            if let Some(problem) = remote_failure(&out) {
                return Err(AdbError::Failed(problem));
            }
        }
        Ok(parse_rows(&out.stdout, projection))
    }

    pub fn shell(&self, script: &str) -> Result<Captured, AdbError> {
        self.run(&["shell", script])
    }

    pub fn run(&self, args: &[&str]) -> Result<Captured, AdbError> {
        let output = self.execute(args)?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            let message = if stderr.trim().is_empty() {
                stdout.trim()
            } else {
                stderr.trim()
            };
            return Err(AdbError::Failed(message.to_owned()));
        }
        Ok(Captured { stdout, stderr })
    }

    pub fn run_raw(&self, args: &[&str]) -> Result<Vec<u8>, AdbError> {
        let output = self.execute(args)?;
        if !output.status.success() {
            let msg = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(AdbError::Failed(msg));
        }
        Ok(output.stdout)
    }

    /// Runs adb with a deadline so a dead USB link cannot leave a background
    /// job (and its worker thread) blocked forever.
    fn execute(&self, args: &[&str]) -> Result<ProcessOutput, AdbError> {
        let mut cmd = Command::new(resolve_program(ADB_PROGRAM)?);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        if let Some(serial) = &self.serial {
            cmd.arg("-s").arg(serial);
        }
        cmd.args(args);

        run_with_timeout(cmd, args, timeout_for(args))
    }
}

struct ProcessOutput {
    status: process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Long-running commands that stream data from or to the device.
fn timeout_for(args: &[&str]) -> Duration {
    match args.first().copied() {
        Some("devices") => DEVICE_LIST_TIMEOUT,
        Some("pull" | "push" | "install" | "install-multiple" | "uninstall") => TRANSFER_TIMEOUT,
        _ => COMMAND_TIMEOUT,
    }
}

fn run_with_timeout(
    mut cmd: Command,
    args: &[&str],
    timeout: Duration,
) -> Result<ProcessOutput, AdbError> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(AdbError::Spawn)?;

    // Drain the pipes on helper threads, but never join them: a stray child
    // that inherited the pipe could otherwise keep this thread blocked forever.
    let (stdout_tx, stdout_rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        thread::spawn(move || {
            let _ = stdout_tx.send(read_all(stdout));
        });
    } else {
        drop(stdout_tx);
    }
    let (stderr_tx, stderr_rx) = mpsc::channel();
    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            let _ = stderr_tx.send(read_all(stderr));
        });
    } else {
        drop(stderr_tx);
    }

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(PROCESS_POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AdbError::Timeout(command_timeout_message(args, timeout)));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AdbError::Spawn(err));
            }
        }
    };

    // The process has exited, so at most one pipe buffer is still in flight.
    let stdout = stdout_rx.recv_timeout(READER_GRACE).unwrap_or_default();
    let stderr = stderr_rx.recv_timeout(READER_GRACE).unwrap_or_default();
    Ok(ProcessOutput {
        status,
        stdout,
        stderr,
    })
}

fn read_all(mut reader: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = reader.read_to_end(&mut bytes);
    bytes
}

fn command_timeout_message(args: &[&str], timeout: Duration) -> String {
    let mut command = args.join(" ");
    if command.len() > MAX_COMMAND_SUMMARY_LEN {
        let cut = command
            .char_indices()
            .take_while(|(index, _)| *index <= MAX_COMMAND_SUMMARY_LEN)
            .map(|(index, _)| index)
            .last()
            .unwrap_or(0);
        command.truncate(cut);
        command.push('…');
    }
    format!(
        "`{ADB_PROGRAM} {command}` did not finish within {} second(s)",
        timeout.as_secs()
    )
}

fn remote_failure(out: &Captured) -> Option<String> {
    [&out.stderr, &out.stdout]
        .into_iter()
        .find(|text| text.contains("Exception") || text.contains("Error"))
        .map(|text| {
            text.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .to_owned()
        })
}

fn pm_succeeded(out: &Captured) -> bool {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|text| text.lines())
        .any(|line| line.trim() == "Success")
}

fn pm_message(out: &Captured) -> String {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|text| text.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("package manager returned no output")
        .to_owned()
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn parse_devices(output: &str) -> Vec<Device> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with('*') && !line.starts_with("List of devices")
        })
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            let serial = tokens.next()?.to_owned();
            let state = tokens.next()?.to_owned();
            let model = tokens
                .find_map(|t| t.strip_prefix("model:"))
                .map(|m| m.replace('_', " "));
            Some(Device {
                serial,
                state,
                model,
            })
        })
        .collect()
}

fn parse_packages(output: &str) -> Vec<String> {
    let mut packages: Vec<String> = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("package:"))
        .map(str::to_owned)
        .collect();
    packages.sort_unstable();
    packages.dedup();
    packages
}

fn parse_call_state(dump: &str) -> CallState {
    let mut best = CallState::Unknown;
    for token in dump.split_whitespace() {
        let Some(value) = token.strip_prefix("mCallState=") else {
            continue;
        };
        let state = match value {
            "0" => CallState::Idle,
            "1" => CallState::Ringing,
            "2" => CallState::Offhook,
            _ => continue,
        };
        if state.priority() > best.priority() {
            best = state;
        }
    }
    best
}

fn parse_rows(output: &str, keys: &[&str]) -> Vec<Vec<String>> {
    let mut raw_rows: Vec<String> = Vec::new();
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(ROW_PREFIX) {
            let rest = rest
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start();
            raw_rows.push(rest.to_owned());
        } else if let Some(last) = raw_rows.last_mut() {
            last.push('\n');
            last.push_str(line);
        }
    }
    raw_rows
        .iter()
        .filter_map(|row| split_row(row, keys))
        .collect()
}

fn find_marker(row: &str, key: &str, from: usize) -> Option<usize> {
    let marker = format!("{key}=");
    row[from..]
        .match_indices(&marker)
        .map(|(i, _)| i + from)
        .find(|&idx| idx == from || row[..idx].ends_with(COLUMN_SEPARATOR))
}

fn split_row(row: &str, keys: &[&str]) -> Option<Vec<String>> {
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(keys.len());
    let mut cursor = 0;
    for key in keys {
        let marker_start = find_marker(row, key, cursor)?;
        let value_start = marker_start + key.len() + 1;
        spans.push((marker_start, value_start));
        cursor = value_start;
    }

    let mut values = Vec::with_capacity(keys.len());
    for (i, &(_, value_start)) in spans.iter().enumerate() {
        let raw = match spans.get(i + 1) {
            Some(&(next_marker_start, _)) => {
                let head = &row[value_start..next_marker_start];
                head.strip_suffix(COLUMN_SEPARATOR).unwrap_or(head)
            }
            None => &row[value_start..],
        };
        let value = raw.trim_end();
        values.push(if value == NULL_LITERAL {
            String::new()
        } else {
            value.to_owned()
        });
    }
    Some(values)
}

fn parse_storage_roots(listing: &str) -> Vec<StorageRoot> {
    let mut roots = vec![StorageRoot {
        label: INTERNAL_STORAGE_LABEL.to_owned(),
        path: INTERNAL_STORAGE_PATH.to_owned(),
    }];
    for name in listing.lines().map(str::trim) {
        let valid = !name.is_empty()
            && name != EMULATED_VOLUME
            && name != SELF_VOLUME
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if valid {
            roots.push(StorageRoot {
                label: format!("SD card / external ({name})"),
                path: format!("{STORAGE_DIR}/{name}"),
            });
        }
    }
    roots
}

fn parse_dir_listing(output: &str) -> Vec<RemoteEntry> {
    let mut entries: Vec<RemoteEntry> = output.lines().filter_map(parse_dir_line).collect();
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Directory)
            .cmp(&(b.kind != EntryKind::Directory))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    entries
}

fn parse_dir_line(line: &str) -> Option<RemoteEntry> {
    let mut fields = line.splitn(4, FIELD_SEPARATOR);
    let kind_text = fields.next()?;
    let size = fields.next()?;
    let mtime = fields.next()?;
    let path = fields.next()?;
    let name = path.rsplit('/').next().filter(|n| !n.is_empty())?;
    let kind = if kind_text.contains("directory") {
        EntryKind::Directory
    } else if kind_text.contains("regular") {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    Some(RemoteEntry {
        name: name.to_owned(),
        kind,
        size: size.trim().parse().unwrap_or(0),
        modified_ms: mtime
            .trim()
            .parse::<i64>()
            .unwrap_or(0)
            .saturating_mul(1000),
    })
}

pub fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for c in value.chars() {
        if c == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    quoted
}

pub fn validate_remote_path(path: &str) -> Result<(), AdbError> {
    let valid = path.starts_with('/')
        && path.len() <= MAX_REMOTE_PATH_LEN
        && !path.chars().any(|c| matches!(c, '\0' | '\n' | '\r'))
        && !path.split('/').any(|component| component == "..");
    if valid {
        Ok(())
    } else {
        Err(AdbError::InvalidInput(format!("bad device path {path:?}")))
    }
}

fn remote_file_name(path: &str) -> Result<&str, AdbError> {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AdbError::InvalidInput(format!("path has no file name: {path:?}")))
}

pub fn sanitise_file_name(name: &str) -> String {
    let replaced: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = replaced.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        "file".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn is_executable_name(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            EXECUTABLE_EXTENSIONS
                .iter()
                .any(|blocked| ext.eq_ignore_ascii_case(blocked))
        })
}

fn unique_path(dir: &Path, name: &str) -> Result<PathBuf, AdbError> {
    let first = dir.join(name);
    if !first.exists() {
        return Ok(first);
    }
    let as_path = Path::new(name);
    let stem = as_path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let extension = as_path.extension().and_then(|e| e.to_str());
    for n in 1..=MAX_NAME_COLLISIONS {
        let candidate = extension.map_or_else(
            || format!("{stem} ({n})"),
            |ext| format!("{stem} ({n}).{ext}"),
        );
        let path = dir.join(candidate);
        if !path.exists() {
            return Ok(path);
        }
    }
    Err(AdbError::Local(format!(
        "too many files named {name:?} in {}",
        dir.display()
    )))
}

fn path_arg(path: &Path) -> Result<&str, AdbError> {
    path.to_str()
        .ok_or_else(|| AdbError::Local(format!("path is not valid UTF-8: {}", path.display())))
}

pub fn download_dir() -> Result<PathBuf, AdbError> {
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let base = env::var_os(home_var)
        .map_or_else(env::temp_dir, |home| PathBuf::from(home).join("Downloads"));
    let dir = base.join(DOWNLOAD_SUBDIR);
    fs::create_dir_all(&dir)
        .map_err(|err| AdbError::Local(format!("cannot create {}: {err}", dir.display())))?;
    Ok(dir)
}

fn open_cache_dir() -> PathBuf {
    env::temp_dir()
        .join(OPEN_TEMP_SUBDIR)
        .join(process::id().to_string())
}

fn launch_default(path: &Path) -> Result<(), AdbError> {
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(any(windows, target_os = "macos")))]
    let opener = "xdg-open";

    let mut child = Command::new(opener)
        .arg(path)
        .spawn()
        .map_err(|err| AdbError::Local(format!("cannot run `{opener}`: {err}")))?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn validate_package(package: &str) -> Result<(), AdbError> {
    let valid = !package.is_empty()
        && package.len() <= MAX_PACKAGE_LEN
        && package
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(AdbError::InvalidInput(format!(
            "bad package name {package:?}"
        )))
    }
}

fn validate_number(number: &str) -> Result<(), AdbError> {
    let digits = number.strip_prefix('+').unwrap_or(number);
    let valid = !digits.is_empty()
        && number.len() <= MAX_NUMBER_LEN
        && digits.chars().all(|c| c.is_ascii_digit());
    if valid {
        Ok(())
    } else {
        Err(AdbError::InvalidInput(format!(
            "phone number must be digits with an optional leading '+', got {number:?}"
        )))
    }
}

fn validate_address(address: &str) -> Result<(), AdbError> {
    let valid = !address.is_empty()
        && address.len() <= MAX_ADDRESS_LEN
        && address
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '_' | '[' | ']'));
    if valid {
        Ok(())
    } else {
        Err(AdbError::InvalidInput(format!(
            "bad device address {address:?}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn kills_commands_that_exceed_their_deadline() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 10"]);
        let started = Instant::now();
        let result = run_with_timeout(cmd, &["sh", "sleep 10"], Duration::from_millis(100));
        assert!(matches!(result, Err(AdbError::Timeout(_))));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn parses_message_ids_alongside_the_other_columns() {
        let output = concat!(
            "Row: 0 _id=42, address=+256700123456, date=1700000000000, ",
            "type=1, body=Hello there\n",
            "Row: 1 _id=43, address=+256700999999, date=1700000001000, ",
            "type=2, body=Second message\n",
        );
        let keys = ["_id", "address", "date", "type", "body"];
        let rows = parse_rows(output, &keys);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "42");
        assert_eq!(rows[1][0], "43");
        assert_eq!(rows[0][4], "Hello there");
    }
}
