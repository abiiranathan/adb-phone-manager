//! Device specifications and battery status collector.

use crate::adb::Adb;

#[derive(Debug, Clone, Default)]
pub struct BatteryInfo {
    pub level: Option<u8>,
    pub status: String,
    pub plugged: String,
    pub temperature_c: Option<f32>,
    pub voltage_mv: Option<u32>,
    pub health: String,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub manufacturer: String,
    pub model: String,
    pub android: String,
    pub sdk: String,
    pub security_patch: String,
    pub abi: String,
    pub screen_size: String,
    pub screen_density: String,
    pub wifi_ip: Option<String>,
    pub uptime_secs: Option<u64>,
    pub battery: BatteryInfo,
    pub mem_total_kb: Option<u64>,
    pub mem_available_kb: Option<u64>,
    pub storage_total_kb: Option<u64>,
    pub storage_free_kb: Option<u64>,
}

#[allow(clippy::cast_sign_loss)]
#[allow(clippy::cast_possible_truncation)]
pub fn collect(adb: &Adb) -> DeviceInfo {
    let mut info = DeviceInfo::default();

    if let Ok(out) = adb.shell("getprop ro.product.manufacturer") {
        out.stdout.trim().clone_into(&mut info.manufacturer);
    }
    if let Ok(out) = adb.shell("getprop ro.product.model") {
        out.stdout.trim().clone_into(&mut info.model);
    }
    if let Ok(out) = adb.shell("getprop ro.build.version.release") {
        out.stdout.trim().clone_into(&mut info.android);
    }
    if let Ok(out) = adb.shell("getprop ro.build.version.sdk") {
        out.stdout.trim().clone_into(&mut info.sdk);
    }
    if let Ok(out) = adb.shell("getprop ro.build.version.security_patch") {
        out.stdout.trim().clone_into(&mut info.security_patch);
    }
    if let Ok(out) = adb.shell("getprop ro.product.cpu.abi") {
        out.stdout.trim().clone_into(&mut info.abi);
    }

    if let Ok(out) = adb.shell("wm size") {
        out.stdout
            .lines()
            .find(|l| l.contains("Physical size:") || l.contains("size:"))
            .and_then(|l| l.split(':').nth(1))
            .unwrap_or("unknown")
            .trim()
            .clone_into(&mut info.screen_size);
    }

    if let Ok(out) = adb.shell("wm density") {
        out.stdout
            .lines()
            .find(|l| l.contains("Physical density:") || l.contains("density:"))
            .and_then(|l| l.split(':').nth(1))
            .unwrap_or("unknown")
            .trim()
            .clone_into(&mut info.screen_density);
    }

    if let Ok(out) = adb.shell("ip -o -4 addr show wlan0 2>/dev/null") {
        for token in out.stdout.split_whitespace() {
            if token.contains('/') && token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                info.wifi_ip = Some(token.split('/').next().unwrap_or(token).to_owned());
                break;
            }
        }
    }

    if let Ok(out) = adb.shell("cat /proc/uptime") {
        if let Some(sec_str) = out.stdout.split_whitespace().next() {
            if let Ok(secs) = sec_str.parse::<f64>() {
                info.uptime_secs = Some(secs as u64);
            }
        }
    }

    if let Ok(out) = adb.shell("dumpsys battery") {
        parse_battery(&out.stdout, &mut info.battery);
    }

    if let Ok(out) = adb.shell("cat /proc/meminfo") {
        for line in out.stdout.lines() {
            if line.starts_with("MemTotal:") {
                info.mem_total_kb = parse_kb(line);
            } else if line.starts_with("MemAvailable:") {
                info.mem_available_kb = parse_kb(line);
            }
        }
    }

    if let Ok(out) = adb.shell("df -k /data") {
        if let Some(line) = out.stdout.lines().nth(1) {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            if tokens.len() >= 4 {
                info.storage_total_kb = tokens[1].parse().ok();
                info.storage_free_kb = tokens[3].parse().ok();
            }
        }
    }

    info
}

fn parse_kb(line: &str) -> Option<u64> {
    line.split_whitespace().nth(1).and_then(|s| s.parse().ok())
}

fn parse_battery(dump: &str, bat: &mut BatteryInfo) {
    for line in dump.lines() {
        let trimmed = line.trim();
        if let Some(val) = trimmed.strip_prefix("level:") {
            bat.level = val.trim().parse().ok();
        } else if let Some(val) = trimmed.strip_prefix("status:") {
            bat.status = match val.trim() {
                "2" => "Charging".to_owned(),
                "3" => "Discharging".to_owned(),
                "4" => "Not charging".to_owned(),
                "5" => "Full".to_owned(),
                _ => "Unknown".to_owned(),
            };
        } else if let Some(val) = trimmed.strip_prefix("plugged:") {
            bat.plugged = match val.trim() {
                "1" => "AC charger".to_owned(),
                "2" => "USB port".to_owned(),
                "4" => "Wireless".to_owned(),
                _ => String::new(),
            };
        } else if let Some(val) = trimmed.strip_prefix("temperature:") {
            if let Ok(t) = val.trim().parse::<f32>() {
                bat.temperature_c = Some(t / 10.0);
            }
        } else if let Some(val) = trimmed.strip_prefix("voltage:") {
            bat.voltage_mv = val.trim().parse().ok();
        } else if let Some(val) = trimmed.strip_prefix("health:") {
            bat.health = match val.trim() {
                "2" => "Good".to_owned(),
                "3" => "Overheat".to_owned(),
                "4" => "Dead".to_owned(),
                "5" => "Over voltage".to_owned(),
                _ => "Normal".to_owned(),
            };
        }
    }
}
