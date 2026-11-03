//! Wrapper and session manager for `scrcpy`.

use std::fmt;
use std::process::{Child, Command};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::adb::AdbError;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32);

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.0, self.1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mirror,
    Camera,
    DeviceAudio,
    Microphone,
    Webcam,
}

impl Mode {
    pub const ALL: [Self; 5] = [
        Self::Mirror,
        Self::Camera,
        Self::DeviceAudio,
        Self::Microphone,
        Self::Webcam,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Mirror => "Display mirror & control",
            Self::Camera => "Camera preview",
            Self::DeviceAudio => "Forward device audio",
            Self::Microphone => "Forward microphone",
            Self::Webcam => "Linux V4L2 webcam",
        }
    }

    pub const fn hint(self) -> &'static str {
        match self {
            Self::Mirror => "Mirror screen with optional mouse and keyboard control",
            Self::Camera => "Stream phone camera live to desktop window",
            Self::DeviceAudio => "Listen to phone audio output on computer",
            Self::Microphone => "Use phone as microphone input",
            Self::Webcam => "Expose phone camera as Linux v4l2loopback video device",
        }
    }

    pub const fn min_version(self) -> Version {
        match self {
            Self::Mirror => Version(1, 0),
            Self::Camera | Self::DeviceAudio | Self::Microphone | Self::Webcam => Version(2, 0),
        }
    }

    pub const fn available_here(self) -> bool {
        match self {
            Self::Webcam => cfg!(target_os = "linux"),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    Back,
    Front,
    External,
}

impl Facing {
    pub const ALL: [Self; 3] = [Self::Back, Self::Front, Self::External];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Back => "Back camera",
            Self::Front => "Front camera",
            Self::External => "External camera",
        }
    }

    pub const fn arg(self) -> &'static str {
        match self {
            Self::Back => "back",
            Self::Front => "front",
            Self::External => "external",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CameraInfo {
    pub id: String,
    pub facing: String,
    pub size: String,
    pub fps: String,
}

impl CameraInfo {
    pub fn label(&self) -> String {
        format!(
            "ID {} ({} - {} @ {}fps)",
            self.id, self.facing, self.size, self.fps
        )
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct Options {
    pub mode: Mode,
    pub control: bool,
    pub audio: bool,
    pub turn_screen_off: bool,
    pub stay_awake: bool,
    pub show_touches: bool,
    pub always_on_top: bool,
    pub audio_duplicate: bool,
    pub v4l2_device: String,
    pub facing: Facing,
    pub camera_id: String,
    pub camera_size: String,
    pub camera_fps: u32,
    pub max_size: u32,
    pub bit_rate_mbps: u32,
    pub max_fps: u32,
    pub record_path: String,
    pub record_only: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            mode: Mode::Mirror,
            control: true,
            audio: true,
            turn_screen_off: false,
            stay_awake: true,
            show_touches: false,
            always_on_top: false,
            audio_duplicate: false,
            v4l2_device: "/dev/video10".to_owned(),
            facing: Facing::Back,
            camera_id: String::new(),
            camera_size: "1920x1080".to_owned(), // Default to safe 1080p
            camera_fps: 30,
            max_size: 0,
            bit_rate_mbps: 0,
            max_fps: 0,
            record_path: String::new(),
            record_only: false,
        }
    }
}

pub struct SessionExit {
    pub success: bool,
    pub message: String,
}

pub struct Session {
    child: Child,
    label: String,
}

impl Session {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn poll(&mut self) -> Option<SessionExit> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(SessionExit {
                success: status.success(),
                message: format!("exited with {status}"),
            }),
            Ok(None) => None,
            Err(e) => Some(SessionExit {
                success: false,
                message: e.to_string(),
            }),
        }
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn detect_version() -> Result<Version, AdbError> {
    let mut cmd = Command::new("scrcpy");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.arg("--version");

    let out = cmd.output().map_err(AdbError::Spawn)?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("scrcpy ") {
            let ver_str = rest.split_whitespace().next().unwrap_or(rest);
            let mut parts = ver_str.split('.');
            if let (Some(maj), Some(min)) = (parts.next(), parts.next()) {
                if let (Ok(maj_num), Ok(min_num)) = (maj.parse::<u32>(), min.parse::<u32>()) {
                    return Ok(Version(maj_num, min_num));
                }
            }
        }
    }
    Err(AdbError::Failed(
        "Could not parse scrcpy version".to_owned(),
    ))
}

pub fn list_cameras(serial: Option<&str>) -> Result<Vec<CameraInfo>, AdbError> {
    let mut cmd = Command::new("scrcpy");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    if let Some(s) = serial {
        cmd.arg("-s").arg(s);
    }
    cmd.arg("--list-cameras");

    let out = cmd.output().map_err(AdbError::Spawn)?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut list = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("--camera-id=") {
            let id = trimmed
                .strip_prefix("--camera-id=")
                .and_then(|s| s.split_whitespace().next())
                .unwrap_or("")
                .trim_matches(['\'', '"', ','])
                .to_owned();

            let facing = if trimmed.contains("front") {
                "Front"
            } else if trimmed.contains("back") {
                "Back"
            } else {
                "External"
            };

            let size = trimmed
                .split('(')
                .nth(1)
                .and_then(|s| s.split(')').next())
                .unwrap_or(trimmed);

            list.push(CameraInfo {
                id,
                facing: facing.to_owned(),
                size: size.to_owned(),
                fps: "30".to_owned(),
            });
        }
    }

    Ok(list)
}

pub fn launch(opts: &Options, serial: Option<&str>) -> Result<Session, AdbError> {
    let mut cmd = Command::new("scrcpy");
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    if let Some(s) = serial {
        cmd.arg("-s").arg(s);
    }

    // Helper closure to apply camera resolution, ID/facing and FPS
    let apply_camera_params = |cmd: &mut Command| {
        cmd.arg("--video-source=camera");

        if opts.camera_id.is_empty() {
            cmd.arg(format!("--camera-facing={}", opts.facing.arg()));
        } else {
            cmd.arg(format!("--camera-id={}", opts.camera_id));
        }

        // Crucial: Fall back to 1920x1080 if unspecified to avoid MediaCodec crash
        let size = if opts.camera_size.trim().is_empty() {
            "1920x1080"
        } else {
            opts.camera_size.trim()
        };
        cmd.arg(format!("--camera-size={size}"));

        if opts.camera_fps > 0 {
            cmd.arg(format!("--camera-fps={}", opts.camera_fps));
        }
    };

    match opts.mode {
        Mode::Mirror => {
            if !opts.control {
                cmd.arg("--no-control");
            }
            if !opts.audio {
                cmd.arg("--no-audio");
            }
            if opts.turn_screen_off {
                cmd.arg("--turn-screen-off");
            }
            if opts.stay_awake {
                cmd.arg("--stay-awake");
            }
            if opts.show_touches {
                cmd.arg("--show-touches");
            }
        }
        Mode::Camera => {
            apply_camera_params(&mut cmd);
            if !opts.audio {
                cmd.arg("--no-audio");
            }
        }
        Mode::DeviceAudio => {
            cmd.arg("--no-video");
            cmd.arg("--audio-source=playback");
            if opts.audio_duplicate {
                cmd.arg("--audio-dup");
            }
        }
        Mode::Microphone => {
            cmd.arg("--no-video");
            cmd.arg("--audio-source=mic");
        }
        Mode::Webcam => {
            apply_camera_params(&mut cmd);
            cmd.arg(format!("--v4l2-sink={}", opts.v4l2_device));
            cmd.arg("--no-playback");
        }
    }

    if opts.always_on_top {
        cmd.arg("--always-on-top");
    }
    if opts.max_size > 0 {
        cmd.arg(format!("--max-size={}", opts.max_size));
    }
    if opts.bit_rate_mbps > 0 {
        cmd.arg(format!("--video-bit-rate={}M", opts.bit_rate_mbps));
    }
    if opts.max_fps > 0 {
        cmd.arg(format!("--max-fps={}", opts.max_fps));
    }
    if !opts.record_path.is_empty() {
        cmd.arg(format!("--record={}", opts.record_path));
    }
    if opts.record_only {
        cmd.arg("--no-playback");
    }

    let child = cmd.spawn().map_err(AdbError::Spawn)?;
    Ok(Session {
        child,
        label: opts.mode.label().to_owned(),
    })
}
