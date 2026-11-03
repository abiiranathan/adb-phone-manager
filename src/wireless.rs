//! Wireless ADB management: TCP/IP switching, pairing and mDNS scanning.

use crate::adb::{Adb, AdbError};

pub const DEFAULT_TCPIP_PORT: u16 = 5555;

pub fn enable_tcpip(adb: &Adb, port: u16) -> Result<String, AdbError> {
    let out = adb.run(&["tcpip", &port.to_string()])?;
    Ok(out.stdout.trim().to_owned())
}

pub fn pair(address: &str, code: &str) -> Result<String, AdbError> {
    let out = Adb::new(None).run(&["pair", address, code])?;
    Ok(out.stdout.trim().to_owned())
}

#[derive(Debug, Clone)]
pub struct Service {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub is_pairing: bool,
}

pub fn discover_mdns() -> Result<Vec<Service>, AdbError> {
    let out = Adb::new(None).run(&["mdns", "services"])?;
    let mut services = Vec::new();

    for line in out.stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("List of discovered") || trimmed.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() >= 3 {
            let name = tokens[0].to_owned();
            let s_type = tokens[1];
            let addr = tokens[2];
            let is_pairing = s_type.contains("pairing");

            if let Some((host, p_str)) = addr.rsplit_once(':') {
                if let Ok(port) = p_str.parse::<u16>() {
                    services.push(Service {
                        name,
                        host: host.to_owned(),
                        port,
                        is_pairing,
                    });
                }
            }
        }
    }

    Ok(services)
}
