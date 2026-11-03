//! `uxdroid install`: the installer's part for an app the framework launcher starts. It puts the APK's
//! native libraries where `ApplicationInfo.nativeLibraryDir` points, `<data>/data/<package>/lib`, and
//! records the ABI it chose, so starting the app only loads them.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use uxdroid_apk::{ExecutionPath, extract_native_libs, host_abi, inspect};

use crate::{EXIT_FAILURE, EXIT_USAGE, INSTALL_USAGE, fail, print};

/// App ids become directory names, so only their documented characters are accepted.
pub(crate) fn valid_package(package: &str) -> bool {
    !package.is_empty()
        && !package.starts_with('.')
        && package
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'_')
}

fn parse(args: &[OsString]) -> Option<(&Path, &Path)> {
    match args {
        [apk, flag, data] if flag == "--data" && !apk.as_encoded_bytes().starts_with(b"--") => {
            Some((Path::new(apk), Path::new(data)))
        }
        _ => None,
    }
}

pub fn run_install(args: &[OsString]) -> ExitCode {
    let Some((apk, data)) = parse(args) else {
        return fail(INSTALL_USAGE, EXIT_USAGE);
    };
    match install(apk, data) {
        Ok(report) => print(&report),
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}

fn install(apk_path: &Path, data: &Path) -> Result<String, String> {
    if !data.is_absolute() {
        return Err(format!(
            "the data directory must be absolute, not {}",
            data.display()
        ));
    }
    let apk = inspect(apk_path).map_err(|error| error.to_string())?;
    let package = &apk.manifest.package;
    if !valid_package(package) {
        return Err(format!("bad package name {}", package.escape_debug()));
    }
    let abi = host_abi();
    if let ExecutionPath::NeedsAbiBridge(abis) = apk.execution_path(abi) {
        return Err(format!(
            "this APK has native code only for {}, and running it on {abi} needs translation, which is not built yet",
            abis.join(", ").escape_debug()
        ));
    }

    let app_dir = data.join("data").join(package);
    let libraries = app_dir.join("lib");
    // Extracted beside the final directory and renamed into place, so a failed install never leaves
    // a mix of old and new libraries.
    let staging = app_dir.join(format!(".lib-{}", std::process::id()));
    let cannot =
        |path: &PathBuf, error: std::io::Error| format!("cannot write {}: {error}", path.display());
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| cannot(&staging, error))?;
    let result = extract_native_libs(apk_path, abi, &staging)
        .map_err(|error| error.to_string())
        .and_then(|count| {
            if libraries.exists() {
                std::fs::remove_dir_all(&libraries).map_err(|error| cannot(&libraries, error))?;
            }
            std::fs::rename(&staging, &libraries).map_err(|error| cannot(&libraries, error))?;
            let record = app_dir.join("abi");
            std::fs::write(&record, format!("{abi}\n")).map_err(|error| cannot(&record, error))?;
            Ok(count)
        });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    let count = result?;
    Ok(format!(
        "installed {}: {count} native libraries for {abi} in {}\n",
        package.escape_debug(),
        libraries.display()
    ))
}
