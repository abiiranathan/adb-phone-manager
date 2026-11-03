//! Synchronous wrapper around the `adb` command-line tool.
//!
//! Every method blocks until the underlying `adb` process exits, so callers
//! should run them off the UI thread. Values interpolated into remote shell
//! commands are validated first, because `adb shell` hands the command line to
//! a shell running on the device.

use std::env;
use std::error::Error;
use std::fmt;
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt; // for creation_flags

/// Name of the adb executable, resolved through `PATH`.
const ADB_PROGRAM: &str = "adb";
/// Windows process creation flag that suppresses a flashing console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Content provider URI exposing every phone number stored in the address book.
const CONTACTS_URI: &str = "content://com.android.contacts/data/phones";
/// Content provider URI exposing the whole SMS store (inbox, sent, drafts...).
const SMS_URI: &str = "content://sms";
/// Content provider URI exposing the device call history.
const CALL_LOG_URI: &str = "content://call_log/calls";
/// Prefix `content query` prints in front of every result row.
const ROW_PREFIX: &str = "Row: ";
/// Literal `content query` prints for SQL NULL.
const NULL_LITERAL: &str = "NULL";
/// Separator `content query` prints between columns.
const COLUMN_SEPARATOR: &str = ", ";

/// Android key codes accepted by `input keyevent`.
const KEYCODE_CALL: u32 = 5;
const KEYCODE_ENDCALL: u32 = 6;
const KEYCODE_HEADSETHOOK: u32 = 79;

/// Time given to the first answer attempt before falling back to a headset hook press.
const ANSWER_RETRY_DELAY: Duration = Duration::from_millis(700);

/// Upper bounds applied to caller-supplied strings.
const MAX_PACKAGE_LEN: usize = 255;
const MAX_NUMBER_LEN: usize = 32;
const MAX_ADDRESS_LEN: usize = 255;
/// Longest device path accepted (Linux `PATH_MAX`).
const MAX_REMOTE_PATH_LEN: usize = 4096;
/// Highest ` (n)` suffix tried when a local file name is already taken.
const MAX_NAME_COLLISIONS: u32 = 1000;

/// Directory under which Android mounts every storage volume.
const STORAGE_DIR: &str = "/storage";
/// Mount point of the primary (internal) shared storage for the first user.
const INTERNAL_STORAGE_PATH: &str = "/storage/emulated/0";
/// Display label of the primary shared storage.
const INTERNAL_STORAGE_LABEL: &str = "Internal storage";
/// Entry of `/storage` that holds the emulated (internal) volumes.
const EMULATED_VOLUME: &str = "emulated";
/// Entry of `/storage` that is a per-process symlink to the primary volume.
const SELF_VOLUME: &str = "self";
/// `stat -c` format producing `type|size|mtime-seconds|path`; the path comes last
/// because it is the only field that may itself contain the separator.
const STAT_FORMAT: &str = "%F|%s|%Y|%n";
/// Separator used by [`STAT_FORMAT`].
const FIELD_SEPARATOR: char = '|';
/// Folder created inside the user's Downloads directory for saved files.
const DOWNLOAD_SUBDIR: &str = "ADB Phone Manager";
/// Folder created inside the system temp directory for files opened directly.
const OPEN_TEMP_SUBDIR: &str = "adb-phone-manager-open";
/// File extensions that are never opened automatically, because the desktop
/// would execute them instead of merely displaying them. They can still be saved.
const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "app", "appimage", "bat", "cmd", "com", "command", "cpl", "desktop", "dll", "exe", "hta",
    "jar", "js", "jse", "lnk", "msi", "pif", "ps1", "reg", "scr", "sh", "vbe", "vbs", "wsf", "wsh",
];

/// Errors produced by adb operations.
#[derive(Debug)]
pub enum AdbError {
    /// The `adb` executable could not be started (not installed or not on `PATH`).
    Spawn(io::Error),
    /// `adb` ran but reported a failure; holds its message.
    Failed(String),
    /// A caller-supplied value was rejected before reaching the device.
    InvalidInput(String),
    /// A problem on the host side (local files, launching the default application).
    Local(String),
}

impl fmt::Display for AdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(err) => write!(
                f,
                "cannot run `{ADB_PROGRAM}` (is it installed and on PATH?): {err}"
            ),
            Self::Failed(msg) | Self::Local(msg) => write!(f, "{msg}"),
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

/// A device known to the adb server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Serial number, or `host:port` for network devices.
    pub serial: String,
    /// Connection state as reported by adb (`device`, `unauthorized`, `offline`...).
    pub state: String,
    /// Marketing model name, when adb reports one.
    pub model: Option<String>,
}

impl Device {
    /// Returns a human-readable label suitable for a picker.
    pub fn label(&self) -> String {
        self.model.as_ref().map_or_else(
            || format!("{} [{}]", self.serial, self.state),
            |model| format!("{model} ({}) [{}]", self.serial, self.state),
        )
    }
}

/// How an uninstall request was carried out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallOutcome {
    /// The package was removed from the device.
    Removed,
    /// The package is part of the system image and could only be removed for
    /// the primary user (`pm uninstall --user 0`).
    RemovedForUser,
}

/// A phone number stored in the address book.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    /// Display name.
    pub name: String,
    /// Phone number exactly as stored.
    pub number: String,
}

/// Direction of an SMS message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmsKind {
    /// Received message (`type = 1`).
    Received,
    /// Sent message (`type = 2`).
    Sent,
    /// Draft, outbox, failed or any other box.
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

/// A single SMS message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sms {
    /// Sender (received) or recipient (sent).
    pub address: String,
    /// Timestamp in milliseconds since the Unix epoch; 0 if unparsable.
    pub date_ms: i64,
    /// Message direction.
    pub kind: SmsKind,
    /// Message text; may contain newlines.
    pub body: String,
}

/// Type of a call log record.
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

/// A single entry in the call log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallRecord {
    pub name: String,
    pub number: String,
    pub date_ms: i64,
    pub duration_secs: u64,
    pub call_type: CallType,
}

/// Telephony state of the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// State could not be determined.
    Unknown,
    /// No call in progress.
    Idle,
    /// A call is active or being dialled.
    Offhook,
    /// An incoming call is ringing.
    Ringing,
}

impl CallState {
    /// Ordering used to merge per-SIM states: ringing beats off-hook beats idle.
    const fn priority(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Idle => 1,
            Self::Offhook => 2,
            Self::Ringing => 3,
        }
    }
}

/// A storage volume that can be browsed (internal storage, SD card, USB drive...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageRoot {
    /// Human-readable name shown in the picker.
    pub label: String,
    /// Absolute mount path on the device.
    pub path: String,
}

/// What a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A directory (or a symlink resolving to one).
    Directory,
    /// A regular file (or a symlink resolving to one).
    File,
    /// Anything else: sockets, FIFOs, device nodes.
    Other,
}

/// One entry of a directory listing on the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    /// File name without any directory part.
    pub name: String,
    /// Kind of entry.
    pub kind: EntryKind,
    /// Size in bytes as reported by `stat`.
    pub size: u64,
    /// Last modification time in milliseconds since the Unix epoch; 0 if unknown.
    pub modified_ms: i64,
}

/// A text segment that is either plain text or a clickable URL.
#[derive(Debug, PartialEq, Eq)]
pub enum TextSegment<'a> {
    Text(&'a str),
    Url(&'a str),
}

/// Scans `text` and splits it into alternating text and URL segments.
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

        // Strip trailing punctuation common in trailing prose.
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

/// Joins a directory and an entry name into a device path (always `/`-separated).
pub fn join_remote(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name)
}

/// Returns the parent directory of a device path, or `None` for `/` itself.
pub fn parent_dir(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    let idx = trimmed.rfind('/')?;
    Some(if idx == 0 { "/" } else { &trimmed[..idx] })
}

/// Splits `path` into the components below `root`, returning `(name, full path)` pairs.
///
/// Returns an empty list when `path` is `root` itself or is not inside it.
pub fn path_segments(root: &str, path: &str) -> Vec<(String, String)> {
    let root = root.trim_end_matches('/');
    let Some(rest) = path.strip_prefix(root) else {
        return Vec::new();
    };
    // Reject siblings that merely share a prefix (`/storage/ab` vs `/storage/abc`).
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

/// Removes the temporary files created by [`Adb::open_remote`] in this process.
///
/// Best effort: files still locked by a viewer (Windows) are left for the OS to clean up.
pub fn cleanup_open_cache() {
    // Nothing useful can be done about a failure while shutting down.
    let _ = fs::remove_dir_all(open_cache_dir());
}

/// Captured output of a finished `adb` invocation that exited successfully.
struct Captured {
    stdout: String,
    stderr: String,
}

/// Handle for talking to one device (or the default device when `serial` is `None`).
pub struct Adb {
    /// Target device passed as `-s <serial>`; `None` lets adb pick.
    serial: Option<String>,
}

impl Adb {
    /// Creates a handle targeting `serial`, or adb's default device if `None`.
    pub const fn new(serial: Option<String>) -> Self {
        Self { serial }
    }

    /// Lists devices currently known to the adb server (starting it if needed).
    pub fn list_devices() -> Result<Vec<Device>, AdbError> {
        let out = Self::new(None).run(&["devices", "-l"])?;
        Ok(parse_devices(&out.stdout))
    }

    /// Connects to a network device (`host:port`) and returns adb's message.
    pub fn connect(address: &str) -> Result<String, AdbError> {
        validate_address(address)?;
        let out = Self::new(None).run(&["connect", address])?;
        let message = out.stdout.trim().to_owned();
        if message.starts_with("failed") || message.starts_with("cannot") {
            return Err(AdbError::Failed(message));
        }
        Ok(message)
    }

    /// Reads every phone number from the address book, sorted by name and de-duplicated.
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

    /// Reads up to `limit` SMS messages, newest first.
    pub fn messages(&self, limit: usize) -> Result<Vec<Sms>, AdbError> {
        let rows = self.content_query(
            SMS_URI,
            &["address", "date", "type", "body"],
            Some("date DESC"),
        )?;
        Ok(rows
            .into_iter()
            .take(limit)
            .filter_map(|row| {
                let [address, date, kind, body] = <[String; 4]>::try_from(row).ok()?;
                Some(Sms {
                    address,
                    date_ms: date.trim().parse().unwrap_or(0),
                    kind: SmsKind::from_type(&kind),
                    body,
                })
            })
            .collect())
    }

    /// Reads up to `limit` call log entries, newest first.
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

    /// Lists installed package names, sorted. With `third_party_only` system apps are omitted.
    pub fn packages(&self, third_party_only: bool) -> Result<Vec<String>, AdbError> {
        let script = if third_party_only {
            "pm list packages -3"
        } else {
            "pm list packages"
        };
        let out = self.shell(script)?;
        Ok(parse_packages(&out.stdout))
    }

    /// Launches the main launcher activity of `package`.
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

    /// Force-stops `package`.
    pub fn force_stop(&self, package: &str) -> Result<(), AdbError> {
        validate_package(package)?;
        self.shell(&format!("am force-stop {package}")).map(|_| ())
    }

    /// Uninstalls `package`, falling back to removing it for the primary user
    /// when the package belongs to the read-only system image.
    ///
    /// Android's package manager exits successfully even when it refuses an
    /// uninstall, so the textual result has to be inspected.
    pub fn uninstall(&self, package: &str) -> Result<UninstallOutcome, AdbError> {
        validate_package(package)?;

        // A regular uninstall removes user apps and their data completely.
        if let Ok(out) = self.shell(&format!("pm uninstall {package}")) {
            if pm_succeeded(&out) {
                return Ok(UninstallOutcome::Removed);
            }
        }

        // System apps cannot be deleted from the system image, but they can be
        // uninstalled for the primary user, which hides them from the launcher.
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

    /// Reads the current telephony state from `dumpsys telephony.registry`.
    pub fn call_state(&self) -> Result<CallState, AdbError> {
        let out = self.shell("dumpsys telephony.registry")?;
        Ok(parse_call_state(&out.stdout))
    }

    /// Answers a ringing call.
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

    /// Rejects a ringing call or hangs up an active one.
    pub fn end_call(&self) -> Result<(), AdbError> {
        self.keyevent(KEYCODE_ENDCALL)
    }

    /// Places a call to `number` (digits and an optional leading `+` only).
    pub fn dial(&self, number: &str) -> Result<(), AdbError> {
        // Replace all spaces in the number with nothing, because `adb shell` will split on spaces.
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

    /// Discovers the browsable storage volumes: the internal storage first, then
    /// every other volume mounted under `/storage` (SD card, USB drive).
    pub fn storage_roots(&self) -> Result<Vec<StorageRoot>, AdbError> {
        let listing = match self.shell(&format!("ls -1 {}", shell_quote(STORAGE_DIR))) {
            Ok(out) => out.stdout,
            // A device that refuses to list /storage still has internal storage.
            Err(AdbError::Failed(_)) => String::new(),
            Err(err) => return Err(err),
        };
        Ok(parse_storage_roots(&listing))
    }

    /// Lists the entries of the device directory `dir`, directories first, then by name.
    ///
    /// Needs `find` and `stat -c` on the device (toybox, Android 6.0 or newer).
    pub fn list_dir(&self, dir: &str) -> Result<Vec<RemoteEntry>, AdbError> {
        validate_remote_path(dir)?;
        // The trailing slash makes `find` descend into a symlinked directory such as /sdcard.
        // The trailing `true` keeps the exit status 0 when `stat` trips over a broken
        // symlink; real failures are detected from the output below instead.
        let script = format!(
            "find {} -maxdepth 1 -mindepth 1 -exec stat -L -c '{STAT_FORMAT}' {{}} + ; true",
            shell_quote(&join_remote(dir, ""))
        );
        let out = self.shell(&script)?;
        let entries = parse_dir_listing(&out.stdout);
        if entries.is_empty() {
            // Nothing parsed: either an empty directory or an error such as
            // "Permission denied". Older adb versions merge stderr into stdout.
            let problem = first_line(&out.stderr).or_else(|| first_line(&out.stdout));
            if let Some(problem) = problem {
                return Err(AdbError::Failed(problem));
            }
        }
        Ok(entries)
    }

    /// Copies a file or directory from the device into the downloads folder
    /// (`~/Downloads/ADB Phone Manager`) and returns its local path.
    ///
    /// A file never overwrites an existing one: a ` (n)` suffix is added to its name.
    /// A directory is merged into an existing local directory of the same name.
    pub fn download(&self, remote: &str, is_dir: bool) -> Result<PathBuf, AdbError> {
        validate_remote_path(remote)?;
        let name = remote_file_name(remote)?;
        let dest_dir = download_dir()?;
        if is_dir {
            // `adb pull` creates the directory inside an existing destination directory.
            self.run(&["pull", remote, path_arg(&dest_dir)?])?;
            return Ok(dest_dir.join(name));
        }
        let local = unique_path(&dest_dir, &sanitise_file_name(name))?;
        self.run(&["pull", remote, path_arg(&local)?])?;
        Ok(local)
    }

    /// Copies a file into a per-process temporary folder and opens it with the
    /// desktop's default application. Returns the temporary path.
    ///
    /// Executable and script file types are refused; use [`Adb::download`] for those.
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

    /// Injects a key event on the device.
    fn keyevent(&self, code: u32) -> Result<(), AdbError> {
        self.shell(&format!("input keyevent {code}")).map(|_| ())
    }

    /// Runs `content query` and returns the parsed rows.
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

    /// Runs `adb shell <script>`.
    fn shell(&self, script: &str) -> Result<Captured, AdbError> {
        self.run(&["shell", script])
    }

    /// Runs `adb [-s serial] <args...>` and captures its output.
    fn run(&self, args: &[&str]) -> Result<Captured, AdbError> {
        let mut cmd = Command::new(ADB_PROGRAM);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        if let Some(serial) = &self.serial {
            cmd.arg("-s").arg(serial);
        }
        cmd.args(args);

        let output = cmd.output().map_err(AdbError::Spawn)?;
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
}

/// Detects error text that the Android shell tools print while still exiting 0.
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

/// Returns whether a `pm uninstall` invocation reported success.
///
/// `pm` always exits 0, so the status line is the only reliable signal.
fn pm_succeeded(out: &Captured) -> bool {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|text| text.lines())
        .any(|line| line.trim() == "Success")
}

/// Picks the most useful line from a failed `pm` invocation.
fn pm_message(out: &Captured) -> String {
    [&out.stdout, &out.stderr]
        .into_iter()
        .flat_map(|text| text.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("package manager returned no output")
        .to_owned()
}

/// Returns the first non-blank line of `text`, trimmed.
fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// Parses `adb devices -l` output.
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

/// Parses `pm list packages` output into a sorted, de-duplicated list.
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

/// Merges the per-SIM `mCallState=N` fields of `dumpsys telephony.registry`.
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

/// Parses `content query` output into rows of column values ordered like `keys`.
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

/// Finds `key=` in `row` at or after `from`, accepting only occurrences that start a column.
fn find_marker(row: &str, key: &str, from: usize) -> Option<usize> {
    let marker = format!("{key}=");
    row[from..]
        .match_indices(&marker)
        .map(|(i, _)| i + from)
        .find(|&idx| idx == from || row[..idx].ends_with(COLUMN_SEPARATOR))
}

/// Splits one raw row into the values of `keys`, which must appear in this order.
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

/// Builds the volume list from the output of `ls -1 /storage`.
///
/// Internal storage is always first, even when the listing is empty or unreadable.
fn parse_storage_roots(listing: &str) -> Vec<StorageRoot> {
    let mut roots = vec![StorageRoot {
        label: INTERNAL_STORAGE_LABEL.to_owned(),
        path: INTERNAL_STORAGE_PATH.to_owned(),
    }];
    for name in listing.lines().map(str::trim) {
        // Volume ids look like `1234-ABCD`; anything else is noise or an error message.
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

/// Parses the output of the `find ... stat -c STAT_FORMAT` pipeline in [`Adb::list_dir`].
///
/// Directories sort first, then names case-insensitively. Unparsable lines are skipped.
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

/// Parses one `type|size|mtime|path` line.
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

/// Quotes `value` for a POSIX shell so it is passed as exactly one literal word.
fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for c in value.chars() {
        if c == '\'' {
            // Close the quote, emit an escaped quote, reopen the quote.
            quoted.push_str("'\\''");
        } else {
            quoted.push(c);
        }
    }
    quoted.push('\'');
    quoted
}

/// Accepts absolute device paths: bounded, no control characters, no `..` components.
fn validate_remote_path(path: &str) -> Result<(), AdbError> {
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

/// Returns the last component of a device path.
fn remote_file_name(path: &str) -> Result<&str, AdbError> {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AdbError::InvalidInput(format!("path has no file name: {path:?}")))
}

/// Makes a device file name safe to create on any desktop file system.
///
/// Android permits characters (`:`, `?`, `*`...) that Windows rejects, and Windows
/// silently strips trailing dots and spaces.
fn sanitise_file_name(name: &str) -> String {
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

/// Returns whether the desktop would execute, rather than display, a file with this name.
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

/// Returns a path in `dir` for `name` that does not exist yet, appending ` (n)` if needed.
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

/// Converts a local path to the `&str` that [`Adb::run`] expects.
fn path_arg(path: &Path) -> Result<&str, AdbError> {
    path.to_str()
        .ok_or_else(|| AdbError::Local(format!("path is not valid UTF-8: {}", path.display())))
}

/// Returns (creating it if needed) the folder that downloads are saved into.
fn download_dir() -> Result<PathBuf, AdbError> {
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let base = env::var_os(home_var)
        .map_or_else(env::temp_dir, |home| PathBuf::from(home).join("Downloads"));
    let dir = base.join(DOWNLOAD_SUBDIR);
    fs::create_dir_all(&dir)
        .map_err(|err| AdbError::Local(format!("cannot create {}: {err}", dir.display())))?;
    Ok(dir)
}

/// Per-process temporary folder holding files opened directly from the device.
fn open_cache_dir() -> PathBuf {
    env::temp_dir()
        .join(OPEN_TEMP_SUBDIR)
        .join(process::id().to_string())
}

/// Opens `path` with the desktop's default application without waiting for it to exit.
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
    // Reap the child in the background so it does not linger as a zombie. Its exit
    // status is meaningless (explorer.exe, for one, reports 1 on success).
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Accepts Android package names: non-empty, bounded, `[A-Za-z0-9._]` only.
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

/// Accepts dialable numbers: digits with an optional leading `+`.
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

/// Accepts `host:port` style addresses (hostnames, IPv4, bracketed IPv6).
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

    #[test]
    fn parses_device_list() {
        let out = "* daemon started successfully\nList of devices attached\n\
                   emulator-5554 device product:sdk model:Pixel_6 device:generic transport_id:1\n\
                   ABC123 unauthorized transport_id:2\n\n";
        let devices = parse_devices(out);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].model.as_deref(), Some("Pixel 6"));
        assert_eq!(devices[1].state, "unauthorized");
        assert_eq!(devices[1].model, None);
    }

    #[test]
    fn parses_rows_with_multiline_body_and_null() {
        let out =
            "Row: 0 address=+256700, date=1700000000000, type=1, body=Hello, world\nsecond line\n\
                   Row: 1 address=MTN, date=1, type=2, body=NULL\n";
        let keys = ["address", "date", "type", "body"];
        let rows = parse_rows(out, &keys);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "+256700");
        assert_eq!(rows[0][3], "Hello, world\nsecond line");
        assert_eq!(rows[1][3], "");
    }

    #[test]
    fn parses_call_log_rows() {
        let out = "Row: 0 number=+123456, date=1700000000000, duration=42, type=1, name=Alice\n\
                   Row: 1 number=+987654, date=1700000005000, duration=0, type=3, name=NULL\n";
        let keys = ["number", "date", "duration", "type", "name"];
        let rows = parse_rows(out, &keys);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            vec!["+123456", "1700000000000", "42", "1", "Alice"]
        );
        assert_eq!(rows[1], vec!["+987654", "1700000005000", "0", "3", ""]);
    }

    #[test]
    fn parses_url_segments() {
        let text = "Check https://example.com/test. Cool site!";
        let segs = parse_text_segments(text);
        assert_eq!(
            segs,
            vec![
                TextSegment::Text("Check "),
                TextSegment::Url("https://example.com/test"),
                TextSegment::Text("."),
                TextSegment::Text(" Cool site!")
            ]
        );
    }

    #[test]
    fn body_containing_key_lookalike_is_not_split() {
        let out = "Row: 0 address=A, date=2, type=1, body=see date=5, type=9";
        let rows = parse_rows(out, &["address", "date", "type", "body"]);
        assert_eq!(rows[0][3], "see date=5, type=9");
    }

    #[test]
    fn empty_result_yields_no_rows() {
        assert!(parse_rows("No result found.\n", &["a"]).is_empty());
    }

    #[test]
    fn call_state_prefers_ringing() {
        assert_eq!(
            parse_call_state("mCallState=0\nmCallState=1\n"),
            CallState::Ringing
        );
        assert_eq!(
            parse_call_state("mCallState=2 mCallState=0"),
            CallState::Offhook
        );
        assert_eq!(parse_call_state("mCallState=0"), CallState::Idle);
        assert_eq!(parse_call_state("nothing"), CallState::Unknown);
    }

    #[test]
    fn parses_packages() {
        assert_eq!(
            parse_packages("package:com.b\npackage:com.a\npackage:com.a\n"),
            vec!["com.a".to_owned(), "com.b".to_owned()]
        );
    }

    #[test]
    fn validators_reject_shell_metacharacters() {
        assert!(validate_package("com.example.app_1").is_ok());
        assert!(validate_package("com.x; reboot").is_err());
        assert!(validate_package("").is_err());
        assert!(validate_number("+256700123456").is_ok());
        assert!(validate_number("123;rm").is_err());
        assert!(validate_number("*123#").is_err());
        assert!(validate_number("+").is_err());
        assert!(validate_address("192.168.1.20:5555").is_ok());
        assert!(validate_address("a b").is_err());
    }

    #[test]
    fn storage_roots_list_internal_then_removable_volumes() {
        let roots = parse_storage_roots("emulated\n1A2B-3C4D\nself\n\n");
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].path, "/storage/emulated/0");
        assert_eq!(roots[1].path, "/storage/1A2B-3C4D");
        assert!(roots[1].label.contains("1A2B-3C4D"));
    }

    #[test]
    fn storage_roots_fall_back_to_internal_only() {
        let roots = parse_storage_roots("");
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].label, INTERNAL_STORAGE_LABEL);
        // An error message in the listing must not become a volume.
        assert_eq!(
            parse_storage_roots("ls: /storage: Permission denied").len(),
            1
        );
    }

    #[test]
    fn dir_listing_sorts_directories_first_and_keeps_odd_names() {
        let out = "regular file|2048|1700000000|/sdcard/b file|with pipe.txt\n\
                   directory|4096|1700000100|/sdcard/Zeta\n\
                   regular empty file|0|1700000200|/sdcard/a.txt\n\
                   directory|4096|1700000300|/sdcard/alpha\n\
                   socket|0|1|/sdcard/sock\n\
                   stat: '/sdcard/broken': No such file or directory\n";
        let entries = parse_dir_listing(out);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["alpha", "Zeta", "a.txt", "b file|with pipe.txt", "sock"]
        );
        assert_eq!(entries[0].kind, EntryKind::Directory);
        assert_eq!(entries[2].kind, EntryKind::File);
        assert_eq!(entries[3].size, 2048);
        assert_eq!(entries[3].modified_ms, 1_700_000_000_000);
        assert_eq!(entries[4].kind, EntryKind::Other);
    }

    #[test]
    fn shell_quote_neutralises_metacharacters() {
        assert_eq!(shell_quote("/sdcard/a b"), "'/sdcard/a b'");
        assert_eq!(shell_quote("/sdcard/it's"), "'/sdcard/it'\\''s'");
        assert_eq!(shell_quote("/x; reboot #"), "'/x; reboot #'");
    }

    #[test]
    fn remote_path_validation() {
        assert!(validate_remote_path("/storage/emulated/0/DCIM").is_ok());
        assert!(validate_remote_path("relative/path").is_err());
        assert!(validate_remote_path("/sdcard/../data").is_err());
        assert!(validate_remote_path("/sdcard/a\nb").is_err());
        assert!(validate_remote_path("").is_err());
        assert!(validate_remote_path("/sdcard/..hidden").is_ok());
    }

    #[test]
    fn path_helpers() {
        assert_eq!(join_remote("/a/b/", "c"), "/a/b/c");
        assert_eq!(join_remote("/a/b", "c"), "/a/b/c");
        assert_eq!(join_remote("/a/b", ""), "/a/b/");
        assert_eq!(parent_dir("/a/b/c"), Some("/a/b"));
        assert_eq!(parent_dir("/a"), Some("/"));
        assert_eq!(parent_dir("/"), None);
        assert_eq!(remote_file_name("/a/b/c.txt/").unwrap(), "c.txt");
        assert!(remote_file_name("/").is_err());
    }

    #[test]
    fn breadcrumb_segments() {
        let segs = path_segments("/storage/emulated/0", "/storage/emulated/0/DCIM/Camera");
        assert_eq!(
            segs,
            vec![
                ("DCIM".to_owned(), "/storage/emulated/0/DCIM".to_owned()),
                (
                    "Camera".to_owned(),
                    "/storage/emulated/0/DCIM/Camera".to_owned()
                ),
            ]
        );
        assert!(path_segments("/storage/ab", "/storage/ab").is_empty());
        assert!(path_segments("/storage/ab", "/storage/abc/x").is_empty());
    }

    #[test]
    fn file_names_are_made_portable() {
        assert_eq!(sanitise_file_name("a:b?c*.txt"), "a_b_c_.txt");
        assert_eq!(sanitise_file_name("report. "), "report");
        assert_eq!(sanitise_file_name("..."), "file");
        assert_eq!(sanitise_file_name("IMG 001.jpg"), "IMG 001.jpg");
    }

    #[test]
    fn executable_types_are_not_opened() {
        assert!(is_executable_name("setup.EXE"));
        assert!(is_executable_name("run.sh"));
        assert!(is_executable_name("evil.lnk"));
        assert!(!is_executable_name("photo.jpg"));
        assert!(!is_executable_name("exe"));
        assert!(!is_executable_name("notes"));
    }

    #[test]
    fn pm_uninstall_result_is_read_from_output() {
        let success = Captured {
            stdout: "Success\n".to_owned(),
            stderr: String::new(),
        };
        assert!(pm_succeeded(&success));

        let failure = Captured {
            stdout: "Failure [DELETE_FAILED_INTERNAL_ERROR]\n".to_owned(),
            stderr: String::new(),
        };
        assert!(!pm_succeeded(&failure));
        assert_eq!(
            pm_message(&failure),
            "Failure [DELETE_FAILED_INTERNAL_ERROR]"
        );

        let stderr_only = Captured {
            stdout: String::new(),
            stderr: "Failure [not installed for 0]\n".to_owned(),
        };
        assert!(!pm_succeeded(&stderr_only));
        assert_eq!(pm_message(&stderr_only), "Failure [not installed for 0]");
    }

    #[test]
    fn unique_path_adds_numeric_suffix() {
        let dir = env::temp_dir().join(format!("adbpm-test-{}", process::id()));
        fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "photo.jpg").unwrap();
        assert_eq!(first, dir.join("photo.jpg"));
        fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, "photo.jpg").unwrap();
        assert_eq!(second, dir.join("photo (1).jpg"));
        fs::write(&second, b"x").unwrap();
        assert_eq!(
            unique_path(&dir, "photo.jpg").unwrap(),
            dir.join("photo (2).jpg")
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
