//! Android remote navigation, hardware keys, toggles and input.

use crate::adb::{Adb, AdbError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Back,
    Home,
    Recents,
    Menu,
    Power,
    Wake,
    Sleep,
    DpadLeft,
    DpadUp,
    DpadDown,
    DpadRight,
    DpadCenter,
    Previous,
    PlayPause,
    Next,
    VolumeDown,
    VolumeUp,
    Mute,
    Enter,
    Delete,
    Tab,
    Escape,
}

impl Key {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Back => "◀ Back",
            Self::Home => "● Home",
            Self::Recents => "■ Recents",
            Self::Menu => "Menu",
            Self::Power => "⏻ Power",
            Self::Wake => "Wake",
            Self::Sleep => "Sleep",
            Self::DpadLeft => "🡄 Left",
            Self::DpadUp => "🡅 Up",
            Self::DpadDown => "🡇 Down",
            Self::DpadRight => "🡆 Right",
            Self::DpadCenter => "OK / Select",
            Self::Previous => "⏮ Prev",
            Self::PlayPause => "⏯ Play/Pause",
            Self::Next => "⏭ Next",
            Self::VolumeDown => "🔉 Vol -",
            Self::VolumeUp => "🔊 Vol +",
            Self::Mute => "🔇 Mute",
            Self::Enter => "Enter",
            Self::Delete => "Backspace",
            Self::Tab => "Tab",
            Self::Escape => "Esc",
        }
    }

    pub const fn keycode(self) -> u32 {
        match self {
            Self::Back => 4,
            Self::Home => 3,
            Self::Recents => 187,
            Self::Menu => 82,
            Self::Power => 26,
            Self::Wake => 224,
            Self::Sleep => 223,
            Self::DpadLeft => 21,
            Self::DpadUp => 19,
            Self::DpadDown => 20,
            Self::DpadRight => 22,
            Self::DpadCenter => 23,
            Self::Previous => 88,
            Self::PlayPause => 85,
            Self::Next => 87,
            Self::VolumeDown => 25,
            Self::VolumeUp => 24,
            Self::Mute => 164,
            Self::Enter => 66,
            Self::Delete => 67,
            Self::Tab => 61,
            Self::Escape => 111,
        }
    }
}

pub fn press(adb: &Adb, key: Key) -> Result<(), AdbError> {
    adb.shell(&format!("input keyevent {}", key.keycode()))
        .map(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Notifications,
    QuickSettings,
    Collapse,
}

pub fn panel(adb: &Adb, p: Panel) -> Result<(), AdbError> {
    let script = match p {
        Panel::Notifications => "cmd statusbar expand-notifications",
        Panel::QuickSettings => "cmd statusbar expand-settings",
        Panel::Collapse => "cmd statusbar collapse",
    };
    adb.shell(script).map(|_| ())
}

pub fn set_brightness(adb: &Adb, level: u8) -> Result<(), AdbError> {
    adb.shell(&format!("settings put system screen_brightness {level}"))
        .map(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Media,
    Ring,
}

pub fn set_volume(adb: &Adb, stream: Stream, level: u32) -> Result<(), AdbError> {
    let id = match stream {
        Stream::Media => 3,
        Stream::Ring => 2,
    };
    adb.shell(&format!(
        "cmd media_session volume --stream {id} --set {level}"
    ))
    .map(|_| ())
}

pub fn type_text(adb: &Adb, text: &str) -> Result<(), AdbError> {
    let escaped = text.replace(' ', "%s");
    adb.shell(&format!("input text '{escaped}'")).map(|_| ())
}

pub fn open_url(adb: &Adb, url: &str) -> Result<(), AdbError> {
    adb.shell(&format!(
        "am start -a android.intent.action.VIEW -d '{url}'"
    ))
    .map(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Wifi,
    Bluetooth,
    MobileData,
    Airplane,
    AutoRotate,
    Torch,
}

impl Toggle {
    pub const ALL: [Self; 6] = [
        Self::Wifi,
        Self::Bluetooth,
        Self::MobileData,
        Self::Airplane,
        Self::AutoRotate,
        Self::Torch,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Wifi => "Wi-Fi",
            Self::Bluetooth => "Bluetooth",
            Self::MobileData => "Mobile Data",
            Self::Airplane => "Airplane Mode",
            Self::AutoRotate => "Auto-Rotate",
            Self::Torch => "Flashlight / Torch",
        }
    }
}

pub fn set_toggle(adb: &Adb, toggle: Toggle, on: bool) -> Result<(), AdbError> {
    let script = match toggle {
        Toggle::Wifi => format!("svc wifi {}", if on { "enable" } else { "disable" }),
        Toggle::Bluetooth => {
            format!(
                "cmd bluetooth_manager {} || svc bluetooth {}",
                if on { "enable" } else { "disable" },
                if on { "enable" } else { "disable" }
            )
        }
        Toggle::MobileData => format!("svc data {}", if on { "enable" } else { "disable" }),
        Toggle::Airplane => {
            format!(
                "cmd connectivity airplane-mode {}",
                if on { "enable" } else { "disable" }
            )
        }
        Toggle::AutoRotate => {
            format!(
                "settings put system accelerometer_rotation {}",
                i32::from(on)
            )
        }
        Toggle::Torch => {
            format!("cmd camera set-torch-mode 0 {}", i32::from(on))
        }
    };
    adb.shell(&script).map(|_| ())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Reboot,
    Recovery,
    Bootloader,
    PowerOff,
}

impl PowerAction {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Reboot => "Restart Phone",
            Self::Recovery => "Reboot to Recovery",
            Self::Bootloader => "Reboot to Bootloader / Fastboot",
            Self::PowerOff => "Power Off",
        }
    }

    pub fn execute(self, adb: &Adb) -> Result<(), AdbError> {
        match self {
            Self::Reboot => adb.run(&["reboot"]).map(|_| ()),
            Self::Recovery => adb.run(&["reboot", "recovery"]).map(|_| ()),
            Self::Bootloader => adb.run(&["reboot", "bootloader"]).map(|_| ()),
            Self::PowerOff => adb.shell("reboot -p").map(|_| ()),
        }
    }
}
