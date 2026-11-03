//! Runs adb operations on background threads and reports results to the UI.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use eframe::egui;

use crate::adb::{
    Adb, AdbError, CallRecord, CallState, Contact, Device, RemoteEntry, Sms, StorageRoot,
    UninstallOutcome,
};

/// How often the device watcher asks adb for the current device list.
const DEVICE_WATCH_INTERVAL: Duration = Duration::from_secs(2);

/// An operation the UI can ask the worker to perform.
pub enum Request {
    /// Re-read the list of attached devices.
    RefreshDevices,
    /// Run `adb connect` against a `host:port` address.
    Connect(String),
    /// Load the address book.
    LoadContacts,
    /// Load up to `limit` SMS messages, newest first.
    LoadMessages { limit: usize },
    /// Load up to `limit` call history entries.
    LoadCallLogs { limit: usize },
    /// Load installed packages.
    LoadApps { third_party_only: bool },
    /// Launch the given package.
    Launch(String),
    /// Force-stop the given package.
    ForceStop(String),
    /// Uninstall the given package (system apps are removed for the primary user).
    Uninstall(String),
    /// Query the telephony state (background poll; failures are silent).
    PollCallState,
    /// Answer a ringing call.
    AnswerCall,
    /// Reject or hang up a call.
    EndCall,
    /// Dial a number.
    Dial(String),
    /// Discover the storage volumes (internal storage, SD card...).
    LoadStorage,
    /// List the directory at the given absolute device path.
    ListDir { path: String },
    /// Copy a file or directory from the device into the downloads folder.
    Download { path: String, is_dir: bool },
    /// Copy a file to a temporary folder and open it with the desktop's default application.
    OpenRemote { path: String },
}

impl Request {
    /// Returns whether the request needs a ready, selected device.
    pub const fn needs_device(&self) -> bool {
        !matches!(self, Self::RefreshDevices | Self::Connect(_))
    }
}

/// The outcome of a [`Request`].
pub enum Response {
    /// Fresh device list, requested by the UI.
    Devices(Vec<Device>),
    /// The watcher observed a change in the attached devices.
    DevicesChanged(Vec<Device>),
    /// `adb connect` finished with this message.
    Connected(String),
    /// Address book contents.
    Contacts(Vec<Contact>),
    /// SMS messages, newest first.
    Messages(Vec<Sms>),
    /// Call log history entries.
    CallLogs(Vec<CallRecord>),
    /// Installed package names.
    Apps(Vec<String>),
    /// A package was uninstalled successfully.
    Uninstalled {
        /// Package that was removed.
        package: String,
        /// Whether it was removed completely or only for the primary user.
        outcome: UninstallOutcome,
    },
    /// Result of a call-state poll.
    CallState(CallState),
    /// Storage volumes found on the device.
    Storage(Vec<StorageRoot>),
    /// Contents of the directory at `path`, directories first.
    Dir {
        /// Directory that was listed, exactly as requested.
        path: String,
        /// Entries found in it.
        entries: Vec<RemoteEntry>,
    },
    /// An action succeeded; holds a status line.
    Done(String),
    /// An action failed; holds the error text.
    Failed(String),
}

impl Response {
    /// Returns whether this result comes from a background job and therefore
    /// does not count against the foreground `pending` counter.
    pub const fn is_background(&self) -> bool {
        matches!(self, Self::CallState(_) | Self::DevicesChanged(_))
    }
}

/// Owns the response channel and spawns jobs.
pub struct Worker {
    /// Cloned into each job thread.
    tx: Sender<Response>,
    /// Drained by the UI once per frame.
    rx: Receiver<Response>,
    /// Used to wake the UI when a result is ready.
    ctx: egui::Context,
    /// Stops the device watcher when the app shuts down.
    _watcher: DeviceWatcher,
}

impl Worker {
    /// Creates a worker that wakes `ctx` whenever a result arrives and starts
    /// the background device watcher.
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let watcher = DeviceWatcher::spawn(tx.clone(), ctx.clone());
        Self {
            tx,
            rx,
            ctx,
            _watcher: watcher,
        }
    }

    /// Runs `request` against `serial` on a new thread. Never blocks.
    pub fn submit(&self, serial: Option<String>, request: Request) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let response = execute(serial, request);
            if tx.send(response).is_err() {
                return;
            }
            ctx.request_repaint();
        });
    }

    /// Returns the next finished result, if any, without blocking.
    pub fn try_recv(&self) -> Option<Response> {
        self.rx.try_recv().ok()
    }
}

/// Long-running thread that reports devices being plugged in or unplugged.
struct DeviceWatcher {
    /// Set on drop to make the thread finish on its next iteration.
    stop: Arc<AtomicBool>,
}

impl DeviceWatcher {
    fn spawn(tx: Sender<Response>, ctx: egui::Context) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        thread::spawn(move || watch_devices(&tx, &ctx, &thread_stop));
        Self { stop }
    }
}

impl Drop for DeviceWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Polls adb for device changes until `stop` is set.
///
/// Only changes are sent, so the UI is not woken up while the device list is
/// stable. The first failure is reported once, which surfaces a missing adb
/// binary without letting a broken server spam the status bar.
fn watch_devices(tx: &Sender<Response>, ctx: &egui::Context, stop: &AtomicBool) {
    let mut previous: Option<Vec<Device>> = None;
    let mut failure_reported = false;

    while !stop.load(Ordering::Relaxed) {
        match Adb::list_devices() {
            Ok(devices) => {
                failure_reported = false;
                if previous.as_ref() != Some(&devices) {
                    previous = Some(devices.clone());
                    if tx.send(Response::DevicesChanged(devices)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            }
            Err(err) => {
                if previous.is_none() && !failure_reported {
                    failure_reported = true;
                    if tx.send(Response::Failed(err.to_string())).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            }
        }
        thread::sleep(DEVICE_WATCH_INTERVAL);
    }
}

/// Executes one request synchronously and converts every outcome into a [`Response`].
fn execute(serial: Option<String>, request: Request) -> Response {
    let adb = Adb::new(serial);
    let result: Result<Response, AdbError> = match request {
        Request::RefreshDevices => Adb::list_devices().map(Response::Devices),
        Request::Connect(address) => Adb::connect(&address).map(Response::Connected),
        Request::LoadContacts => adb.contacts().map(Response::Contacts),
        Request::LoadMessages { limit } => adb.messages(limit).map(Response::Messages),
        Request::LoadCallLogs { limit } => adb.call_logs(limit).map(Response::CallLogs),
        Request::LoadApps { third_party_only } => {
            adb.packages(third_party_only).map(Response::Apps)
        }
        Request::Launch(package) => adb
            .launch(&package)
            .map(|()| Response::Done(format!("Launched {package}"))),
        Request::ForceStop(package) => adb
            .force_stop(&package)
            .map(|()| Response::Done(format!("Stopped {package}"))),
        Request::Uninstall(package) => adb
            .uninstall(&package)
            .map(|outcome| Response::Uninstalled { package, outcome }),
        Request::PollCallState => {
            return Response::CallState(adb.call_state().unwrap_or(CallState::Unknown));
        }
        Request::AnswerCall => adb
            .answer_call()
            .map(|()| Response::Done("Answered call".to_owned())),
        Request::EndCall => adb
            .end_call()
            .map(|()| Response::Done("Ended or rejected call".to_owned())),
        Request::Dial(number) => adb
            .dial(&number)
            .map(|()| Response::Done(format!("Dialling {number}"))),
        Request::LoadStorage => adb.storage_roots().map(Response::Storage),
        Request::ListDir { path } => adb
            .list_dir(&path)
            .map(|entries| Response::Dir { path, entries }),
        Request::Download { path, is_dir } => adb
            .download(&path, is_dir)
            .map(|local| Response::Done(format!("Downloaded {path} to {}", local.display()))),
        Request::OpenRemote { path } => adb
            .open_remote(&path)
            .map(|local| Response::Done(format!("Opened {}", local.display()))),
    };
    result.unwrap_or_else(|err| Response::Failed(err.to_string()))
}
