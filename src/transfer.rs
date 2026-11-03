//! Device file push, upload and directory creation.

use std::path::Path;

use crate::adb::{shell_quote, validate_remote_path, Adb, AdbError};

pub const DEFAULT_UPLOAD_DIR: &str = "/storage/emulated/0/Download";

pub fn upload(adb: &Adb, local: &Path, remote_dir: &str) -> Result<(), AdbError> {
    validate_remote_path(remote_dir)?;
    let local_str = local
        .to_str()
        .ok_or_else(|| AdbError::Local("Local path is not valid UTF-8".to_owned()))?;
    adb.run(&["push", local_str, remote_dir]).map(|_| ())
}

pub fn create_remote_dir(adb: &Adb, remote_dir: &str, name: &str) -> Result<String, AdbError> {
    let clean_name = name.trim().replace('/', "_");
    let target = format!("{}/{}", remote_dir.trim_end_matches('/'), clean_name);
    validate_remote_path(&target)?;
    adb.shell(&format!("mkdir -p {}", shell_quote(&target)))?;
    Ok(target)
}

pub fn delete_remote(adb: &Adb, remote_path: &str, is_dir: bool) -> Result<(), AdbError> {
    validate_remote_path(remote_path)?;
    let flag = if is_dir { "-rf" } else { "-f" };
    adb.shell(&format!("rm {} {}", flag, shell_quote(remote_path)))
        .map(|_| ())
}
