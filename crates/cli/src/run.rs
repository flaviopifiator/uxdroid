//! `uxdroid run`: starts an APK's launcher Activity as this process.

use std::ffi::OsString;
use std::fs::File;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use uxdroid_apk::{ExecutionPath, extract_native_libs, fingerprint, host_abi, inspect};

use crate::install::valid_package;
use crate::{EXIT_FAILURE, EXIT_USAGE, RUN_USAGE, fail};

/// The boot class path, in the order the boot image was compiled with.
const BOOT_JARS: [&str; 7] = [
    "core-oj",
    "core-libart",
    "okhttp",
    "bouncycastle",
    "apache-xml",
    "core-icu4j",
    "conscrypt",
];
const LAUNCHER_CLASS: &str = "dev.uxdroid.runtime.Launcher";
/// Running apps hold a shared lock on this file, inside their install directory.
const IN_USE_MARKER: &str = "in-use";
const MAX_INSTALL_ATTEMPTS: u32 = 5;
const INSTALL_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

struct RunArgs<'a> {
    apk: &'a Path,
    runtime: Option<PathBuf>,
    exit_after_first_frame: bool,
}

fn parse(args: &[OsString]) -> Option<RunArgs<'_>> {
    let (apk, mut rest) = args.split_first()?;
    if apk.as_encoded_bytes().starts_with(b"--") {
        return None;
    }
    let mut parsed = RunArgs {
        apk: Path::new(apk),
        runtime: None,
        exit_after_first_frame: false,
    };
    while let Some((flag, remaining)) = rest.split_first() {
        if flag == "--exit-after-first-frame" && !parsed.exit_after_first_frame {
            parsed.exit_after_first_frame = true;
            rest = remaining;
        } else if flag == "--runtime" && parsed.runtime.is_none() {
            let (value, remaining) = remaining.split_first()?;
            parsed.runtime = Some(PathBuf::from(value));
            rest = remaining;
        } else {
            return None;
        }
    }
    Some(parsed)
}

fn home_dir(variable: &str, fallback: &str) -> Result<PathBuf, String> {
    // The XDG specification treats an empty value as unset.
    if let Some(value) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(Path::new(&home).join(fallback))
}

fn boot_class_path(runtime: &Path) -> String {
    let jars: Vec<String> = BOOT_JARS
        .iter()
        .map(|jar| {
            runtime
                .join("javalib")
                .join(format!("{jar}.jar"))
                .display()
                .to_string()
        })
        .collect();
    jars.join(":")
}

fn runtime_environment(command: &mut Command, runtime: &Path, data: &Path, library_path: &str) {
    command
        .env("ANDROID_ROOT", runtime)
        .env("ANDROID_ART_ROOT", runtime)
        .env("ANDROID_I18N_ROOT", runtime.join("com.android.i18n"))
        .env("ANDROID_TZDATA_ROOT", runtime.join("com.android.tzdata"))
        .env("ANDROID_DATA", data)
        .env("LD_LIBRARY_PATH", library_path);
}

/// Compiles the APK at `work` to machine code, as the file it will become at `location`.
fn compile_app(
    runtime: &Path,
    work: &Path,
    location: &Path,
    data: &Path,
    library_path: &str,
) -> Result<(), String> {
    let boot = boot_class_path(runtime);
    let mut command = Command::new(runtime.join("bin/dex2oat64"));
    command
        .arg("--compiler-filter=speed")
        .arg("--instruction-set=x86_64")
        .arg("--host")
        .arg(format!("--android-root={}", runtime.display()))
        .args(["--runtime-arg", &format!("-Xbootclasspath:{boot}")])
        .args([
            "--runtime-arg",
            &format!("-Xbootclasspath-locations:{boot}"),
        ])
        .arg(format!(
            "--boot-image={}",
            runtime.join("framework/boot.art").display()
        ))
        .arg(format!(
            "--class-loader-context=PCL[{}]",
            runtime.join("platform/platform.jar").display()
        ))
        .arg(format!("--dex-file={}", work.join("app.apk").display()))
        .arg(format!(
            "--dex-location={}",
            location.join("app.apk").display()
        ))
        .arg(format!(
            "--oat-file={}",
            work.join("oat/x86_64/app.odex").display()
        ));
    runtime_environment(&mut command, runtime, data, library_path);
    let output = command
        .output()
        .map_err(|error| format!("cannot start dex2oat: {error}"))?;
    if !output.status.success() {
        // Its last line is usually a timing summary, so errors are searched for first.
        let log = String::from_utf8_lossy(&output.stderr);
        let line = log
            .lines()
            .rfind(|line| line.contains(" E ") || line.contains(" F "))
            .or(log.lines().last())
            .unwrap_or("");
        return Err(format!("dex2oat failed: {}", line.escape_debug()));
    }
    Ok(())
}

/// The identity of an installed app: its contents and everything its compiled code depends on.
/// ART quietly discards compiled code when the boot image or platform changes or moves.
///
/// Contents come from the archive directory, which is fast but trusts declared CRCs: a crafted
/// APK can match another one's. That reuses code the user already ran, and goes no further.
fn install_id(apk: &Path, runtime: &Path) -> Result<String, String> {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = fingerprint(apk).map_err(|error| error.to_string())?;
    let mut mix = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME);
        }
    };
    mix(runtime.as_os_str().as_encoded_bytes());
    for dependency in ["framework/x86_64/boot.oat", "platform/platform.jar"] {
        let meta = std::fs::metadata(runtime.join(dependency))
            .map_err(|error| format!("cannot read the runtime: {error}"))?;
        mix(&meta.mtime().to_le_bytes());
        mix(&meta.mtime_nsec().to_le_bytes());
        mix(&meta.len().to_le_bytes());
    }
    Ok(format!("{hash:016x}"))
}

/// Marks an install as in use for as long as this process and what it `exec`s live.
///
/// Returns false if the install was removed before the mark was placed.
fn hold_install(installed: &Path) -> Result<bool, String> {
    let marker = installed.join(IN_USE_MARKER);
    let file = match File::open(&marker) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot read {}: {error}", marker.display())),
    };
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is open. Clearing close-on-exec lets the lock outlive the exec into the runtime,
    // and blocking only waits for a remover that holds it briefly.
    let locked =
        unsafe { libc::fcntl(fd, libc::F_SETFD, 0) == 0 && libc::flock(fd, libc::LOCK_SH) == 0 };
    if !locked {
        return Err(format!(
            "cannot lock {}: {}",
            marker.display(),
            std::io::Error::last_os_error()
        ));
    }
    // A remover may have finished just before the lock was taken, and a new install could already
    // stand at the same path, so the file itself must still be the one at that path.
    let opened = file.metadata().map_err(|error| error.to_string())?;
    let still_there = std::fs::metadata(&marker)
        .is_ok_and(|current| current.ino() == opened.ino() && current.dev() == opened.dev());
    if still_there {
        // The descriptor must stay open until the process ends.
        std::mem::forget(file);
    }
    Ok(still_there)
}

/// Unpacks and compiles the APK into a private directory, then renames it into place, so a failed
/// or concurrent install never leaves anything another run could pick up.
fn install(
    args: &RunArgs,
    runtime: &Path,
    package_dir: &Path,
    id: &str,
    data: &Path,
    library_path: &str,
) -> Result<PathBuf, String> {
    let installed = package_dir.join(id);
    if installed.is_dir() {
        return Ok(installed);
    }
    let work = package_dir.join(format!(".installing-{}", std::process::id()));
    let result = (|| {
        let cannot =
            |error: std::io::Error| format!("cannot install in {}: {error}", work.display());
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(work.join("oat/x86_64")).map_err(cannot)?;
        std::fs::create_dir_all(work.join("lib")).map_err(cannot)?;
        File::create(work.join(IN_USE_MARKER)).map_err(cannot)?;
        // A read-only APK must not make its copy impossible to replace, so contents are copied
        // and not its mode.
        let mut source = File::open(args.apk).map_err(cannot)?;
        let mut copy = File::create(work.join("app.apk")).map_err(cannot)?;
        std::io::copy(&mut source, &mut copy).map_err(cannot)?;
        extract_native_libs(args.apk, host_abi(), &work.join("lib"))
            .map_err(|error| error.to_string())?;
        compile_app(runtime, &work, &installed, data, library_path)?;
        match std::fs::rename(&work, &installed) {
            Ok(()) => Ok(()),
            // Another run finished first with the same contents, so its copy serves just as well.
            Err(_) if installed.is_dir() => {
                let _ = std::fs::remove_dir_all(&work);
                Ok(())
            }
            Err(error) => Err(cannot(error)),
        }
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&work);
    }
    result?;
    Ok(installed)
}

/// Removes what nothing needs: other versions not in use, and installs whose process died.
/// Best effort, because a leftover only costs disk.
fn remove_unused_installs(package_dir: &Path, current: &str) {
    let Ok(entries) = std::fs::read_dir(package_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let text = name.to_string_lossy();
        // A rename first, so an interrupted removal never leaves a damaged install at its id.
        if text.starts_with(".removing-") {
            let _ = std::fs::remove_dir_all(entry.path());
            continue;
        }
        if let Some(pid) = text.strip_prefix(".installing-") {
            let alive = Path::new("/proc").join(pid).exists();
            if !alive {
                let _ = std::fs::remove_dir_all(entry.path());
            }
            continue;
        }
        let is_install = text.len() == 16 && text.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !is_install || text == current {
            continue;
        }
        let Ok(marker) = File::open(entry.path().join(IN_USE_MARKER)) else {
            continue;
        };
        // SAFETY: the descriptor is open, and a failed lock means the install is in use.
        let unused = unsafe { libc::flock(marker.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 };
        if unused {
            let doomed = package_dir.join(format!(".removing-{text}-{}", std::process::id()));
            if std::fs::rename(entry.path(), &doomed).is_ok() {
                let _ = std::fs::remove_dir_all(&doomed);
            }
        }
    }
}

fn prepare_and_exec(args: &RunArgs) -> Result<std::convert::Infallible, String> {
    let apk = inspect(args.apk).map_err(|error| error.to_string())?;
    let abi = host_abi();
    if let ExecutionPath::NeedsAbiBridge(abis) = apk.execution_path(abi) {
        return Err(format!(
            "this APK has native code only for {}, and running it on {abi} needs translation, which is not built yet",
            abis.join(", ").escape_debug()
        ));
    }
    let activity = apk
        .manifest
        .launcher
        .as_deref()
        .ok_or("this APK has no launcher Activity")?;
    if !valid_package(&apk.manifest.package) {
        return Err(format!(
            "bad package name {}",
            apk.manifest.package.escape_debug()
        ));
    }

    let runtime = match &args.runtime {
        Some(runtime) => runtime.clone(),
        None => match std::env::var_os("UXDROID_RUNTIME").filter(|value| !value.is_empty()) {
            Some(runtime) => PathBuf::from(runtime),
            None => home_dir("XDG_DATA_HOME", ".local/share")?.join("uxdroid/runtime"),
        },
    };
    let runtime = std::fs::canonicalize(&runtime)
        .map_err(|error| format!("cannot find the runtime at {}: {error}", runtime.display()))?;

    let package_dir = home_dir("XDG_CACHE_HOME", ".cache")?
        .join("uxdroid")
        .join(&apk.manifest.package);
    let data = package_dir.join("data");
    std::fs::create_dir_all(data.join("dalvik-cache/x86_64"))
        .map_err(|error| format!("cannot prepare {}: {error}", package_dir.display()))?;
    // Class paths separate entries with a colon, and have no way to escape one.
    let package_dir = std::fs::canonicalize(&package_dir).map_err(|error| error.to_string())?;
    if package_dir.as_os_str().as_encoded_bytes().contains(&b':') {
        return Err(format!(
            "the cache path {} contains a colon",
            package_dir.display()
        ));
    }
    let data = package_dir.join("data");

    // The uxdroid native libraries are installed next to this program.
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let native_dir = exe.parent().ok_or("cannot locate this program")?;
    let library_path = format!(
        "{}:{}",
        runtime.join("lib64").display(),
        native_dir.display()
    );
    let id = install_id(args.apk, &runtime)?;
    let mut installed = install(args, &runtime, &package_dir, &id, &data, &library_path)?;
    // A concurrent run of another version may remove an install nobody has marked yet.
    let mut attempts = 0;
    while !hold_install(&installed)? {
        attempts += 1;
        if attempts == MAX_INSTALL_ATTEMPTS {
            return Err("the install keeps being removed by other runs".to_string());
        }
        std::thread::sleep(INSTALL_RETRY_DELAY);
        installed = install(args, &runtime, &package_dir, &id, &data, &library_path)?;
    }
    remove_unused_installs(&package_dir, &id);
    let app = installed.join("app.apk");
    let lib_dir = installed.join("lib");

    let platform = runtime.join("platform/platform.jar");
    let boot = boot_class_path(&runtime);
    let mut command = Command::new(runtime.join("bin/dalvikvm64"));
    command
        // Native bridge testing mode creates ./code_cache, so the current directory is the app's.
        .current_dir(&data)
        .arg(format!("-Xbootclasspath:{boot}"))
        .arg(format!("-Xbootclasspath-locations:{boot}"))
        .arg(format!(
            "-Ximage:{}",
            runtime.join("framework/boot.art").display()
        ))
        // These keep memory low; see runtime/art/run.sh for what each one saves.
        .args(["-Xnorelocate", "-XX:HeapMinFree=256k", "-XX:HeapMaxFree=1m"])
        .args([
            "-XX:LargeObjectSpace=disabled",
            "-XX:ForegroundHeapGrowthMultiplier=1.0",
        ])
        .args([
            "-XX:NativeBridge=libuxdroid_nativebridge.so",
            "-Xforce-nb-testing",
        ])
        .arg(format!(
            "-Djava.library.path={}:{}",
            lib_dir.display(),
            native_dir.display()
        ))
        .arg(format!("-Duxdroid.package={}", apk.manifest.package))
        .arg(format!(
            "-Duxdroid.exit_after_first_frame={}",
            args.exit_after_first_frame
        ))
        .args(["-cp", &format!("{}:{}", platform.display(), app.display())])
        .args([LAUNCHER_CLASS, activity]);
    runtime_environment(&mut command, &runtime, &data, &library_path);
    Err(format!("cannot start the runtime: {}", command.exec()))
}

pub fn run_app(args: &[OsString]) -> ExitCode {
    let Some(args) = parse(args) else {
        return fail(RUN_USAGE, EXIT_USAGE);
    };
    match prepare_and_exec(&args) {
        Ok(never) => match never {},
        Err(error) => fail(&format!("error: {error}"), EXIT_FAILURE),
    }
}
