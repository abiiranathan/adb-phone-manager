//! Package inspector, installer and APK extractor.

use std::path::{Path, PathBuf};

use crate::adb::{download_dir, sanitise_file_name, shell_quote, Adb, AdbError};

pub fn is_apk(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("apk"))
}

#[derive(Debug, Clone, Copy)]
pub struct InstallOptions {
    pub reinstall: bool,
    pub allow_downgrade: bool,
    pub grant_permissions: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            reinstall: true,
            allow_downgrade: false,
            grant_permissions: true,
        }
    }
}

pub fn install(adb: &Adb, apks: &[PathBuf], opts: InstallOptions) -> Result<String, AdbError> {
    if apks.is_empty() {
        return Err(AdbError::InvalidInput("No APKs specified".to_owned()));
    }

    let mut args = Vec::new();
    if apks.len() == 1 {
        args.push("install");
    } else {
        args.push("install-multiple");
    }
    if opts.reinstall {
        args.push("-r");
    }
    if opts.allow_downgrade {
        args.push("-d");
    }
    if opts.grant_permissions {
        args.push("-g");
    }
    for apk in apks {
        let p_str = apk
            .to_str()
            .ok_or_else(|| AdbError::Local("Path contains non-UTF-8 characters".to_owned()))?;
        args.push(p_str);
    }

    let out = adb.run(&args)?;
    Ok(out.stdout.trim().to_owned())
}

#[derive(Debug, Clone, Default)]
pub struct AppDetails {
    pub package: String,
    pub version_name: String,
    pub version_code: String,
    pub target_sdk: String,
    pub min_sdk: String,
    pub apk_path: String,
    pub apk_size: u64,
    pub data_dir: String,
    pub is_system: bool,
    pub permissions: Vec<String>,
}

pub fn get_details(adb: &Adb, package: &str) -> AppDetails {
    let mut details = AppDetails {
        package: package.to_owned(),
        ..Default::default()
    };

    if let Ok(out) = adb.shell(&format!("pm path {}", shell_quote(package))) {
        if let Some(line) = out.stdout.lines().next() {
            if let Some(path) = line.trim().strip_prefix("package:") {
                path.clone_into(&mut details.apk_path);
            }
        }
    }

    if !details.apk_path.is_empty() {
        if let Ok(out) = adb.shell(&format!("stat -c %s {}", shell_quote(&details.apk_path))) {
            details.apk_size = out.stdout.trim().parse().unwrap_or(0);
        }
    }

    if let Ok(out) = adb.shell(&format!("dumpsys package {}", shell_quote(package))) {
        for line in out.stdout.lines() {
            let trimmed = line.trim();
            if let Some(val) = trimmed.strip_prefix("versionName=") {
                val.trim().clone_into(&mut details.version_name);
            } else if let Some(val) = trimmed.strip_prefix("versionCode=") {
                val.split_whitespace()
                    .next()
                    .unwrap_or(val)
                    .clone_into(&mut details.version_code);
            } else if let Some(val) = trimmed.strip_prefix("targetSdk=") {
                val.trim().clone_into(&mut details.target_sdk);
            } else if let Some(val) = trimmed.strip_prefix("minSdk=") {
                val.trim().clone_into(&mut details.min_sdk);
            } else if let Some(val) = trimmed.strip_prefix("dataDir=") {
                val.trim().clone_into(&mut details.data_dir);
            } else if trimmed.contains("pkgFlags=[") {
                details.is_system = trimmed.contains("SYSTEM");
            } else if trimmed.starts_with("android.permission.") {
                let perm = trimmed.split(':').next().unwrap_or(trimmed).to_owned();
                if !details.permissions.contains(&perm) {
                    details.permissions.push(perm);
                }
            }
        }
    }

    details
}

pub fn clear_data(adb: &Adb, package: &str) -> Result<(), AdbError> {
    adb.shell(&format!("pm clear {}", shell_quote(package)))
        .map(|_| ())
}

pub fn enable_app(adb: &Adb, package: &str) -> Result<(), AdbError> {
    adb.shell(&format!("pm enable {}", shell_quote(package)))
        .map(|_| ())
}

pub fn disable_app(adb: &Adb, package: &str) -> Result<(), AdbError> {
    adb.shell(&format!(
        "pm disable-user --user 0 {}",
        shell_quote(package)
    ))
    .map(|_| ())
}

pub fn extract_apk(adb: &Adb, package: &str) -> Result<PathBuf, AdbError> {
    let details = get_details(adb, package);
    if details.apk_path.is_empty() {
        return Err(AdbError::Failed(
            "Could not locate APK on device".to_owned(),
        ));
    }
    let dest_dir = download_dir()?;
    let dest = dest_dir.join(format!("{}.apk", sanitise_file_name(package)));
    adb.run(&[
        "pull",
        &details.apk_path,
        dest.to_str()
            .ok_or_else(|| AdbError::Local("Invalid UTF-8 in destination path".to_owned()))?,
    ])?;
    Ok(dest)
}
