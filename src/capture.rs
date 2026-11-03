//! Screenshot, screen recording and remote camera operations.

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::adb::{download_dir, Adb, AdbError};

pub const MAX_RECORD_SECS: u32 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutter {
    VolumeDown,
    VolumeUp,
    Camera,
    Enter,
}

impl Shutter {
    pub const ALL: [Self; 4] = [Self::VolumeDown, Self::VolumeUp, Self::Camera, Self::Enter];

    pub const fn label(self) -> &'static str {
        match self {
            Self::VolumeDown => "Volume Down",
            Self::VolumeUp => "Volume Up",
            Self::Camera => "Camera Key",
            Self::Enter => "Enter",
        }
    }

    pub const fn keycode(self) -> u32 {
        match self {
            Self::VolumeDown => 25,
            Self::VolumeUp => 24,
            Self::Camera => 27,
            Self::Enter => 66,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraMode {
    Photo,
    Video,
}

pub struct Screenshot {
    pub path: PathBuf,
    pub png: Vec<u8>,
}

pub fn screenshot(adb: &Adb) -> Result<Screenshot, AdbError> {
    let bytes = match adb.run_raw(&["exec-out", "screencap", "-p"]) {
        Ok(b) if !b.is_empty() => b,
        _ => {
            adb.shell("screencap -p /sdcard/_screen_cap.png")?;
            let temp_local = adb.download("/sdcard/_screen_cap.png", false)?;
            let b = fs::read(&temp_local)
                .map_err(|e| AdbError::Local(format!("cannot read screenshot: {e}")))?;
            let _ = fs::remove_file(temp_local);
            let _ = adb.shell("rm -f /sdcard/_screen_cap.png");
            b
        }
    };

    let dir = download_dir()?;
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let path = dir.join(format!("Screenshot_{stamp}.png"));
    fs::write(&path, &bytes)
        .map_err(|e| AdbError::Local(format!("failed to write {}: {e}", path.display())))?;

    Ok(Screenshot { path, png: bytes })
}

pub fn record_screen(adb: &Adb, secs: u32, mbps: u32) -> Result<PathBuf, AdbError> {
    let secs = secs.clamp(1, MAX_RECORD_SECS);
    let bitrate = u64::from(mbps.clamp(1, 50)) * 1_000_000;
    let remote = "/sdcard/_screen_rec.mp4";

    adb.shell(&format!(
        "screenrecord --time-limit {secs} --bit-rate {bitrate} {remote}"
    ))?;

    let local = adb.download(remote, false)?;
    let _ = adb.shell(&format!("rm -f {remote}"));
    Ok(local)
}

pub fn open_camera(adb: &Adb, mode: CameraMode) -> Result<(), AdbError> {
    let action = match mode {
        CameraMode::Photo => "android.media.action.STILL_IMAGE_CAMERA",
        CameraMode::Video => "android.media.action.VIDEO_CAMERA",
    };
    adb.shell(&format!("am start -a {action}")).map(|_| ())
}

pub fn press_shutter(adb: &Adb, shutter: Shutter) -> Result<(), AdbError> {
    adb.shell(&format!("input keyevent {}", shutter.keycode()))
        .map(|_| ())
}

pub fn take_photo(adb: &Adb, shutter: Shutter) -> Result<PathBuf, AdbError> {
    open_camera(adb, CameraMode::Photo)?;
    thread::sleep(Duration::from_millis(1200));
    press_shutter(adb, shutter)?;
    thread::sleep(Duration::from_millis(2000));
    fetch_newest_media(adb)
}

pub fn fetch_newest_media(adb: &Adb) -> Result<PathBuf, AdbError> {
    let out = adb.shell("ls -1t /storage/emulated/0/DCIM/Camera 2>/dev/null | head -1")?;
    let file = out.stdout.trim();
    if file.is_empty() {
        return Err(AdbError::Failed(
            "No recent photo or video found in DCIM/Camera".to_owned(),
        ));
    }
    let remote = format!("/storage/emulated/0/DCIM/Camera/{file}");
    adb.download(&remote, false)
}
